#!/usr/bin/env bash
# 12-cwd-boards.sh — Git-root/CWD scopes isolate pipelines/cards and drive the scoped TUI picker.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

board_at() {
  local cwd="$1"
  shift
  (cd "$cwd" && env -u BOARD_SCOPE_PATH -u HERDR_PLUGIN_CONTEXT_JSON "$BOARD_BIN" "$@")
}

step "Create disposable Git repo/subdir and non-Git cwd"
REPO="$E2E_TMP/project-one"
SUB="$REPO/backend"
PLAIN="$E2E_TMP/plain-notes"
mkdir -p "$SUB" "$PLAIN"
git -C "$REPO" init --quiet
REPO="$(cd "$REPO" && pwd -P)"
SUB="$(cd "$SUB" && pwd -P)"
PLAIN="$(cd "$PLAIN" && pwd -P)"

step "Root and subdirectory share a board; non-Git cwd is isolated"
root_card="$(board_at "$REPO" card new --title root-card --json)"
ROOT_CARD_ID="$(printf '%s' "$root_card" | jget id)"
sub_cards="$(board_at "$SUB" card list --json)"
printf '%s' "$sub_cards" | grep -q 'root-card' || fail "Git subdir did not resolve root board"
# The selected project prevails over the current directory, so reaching the
# plain cwd's project is an explicit open (creates it and selects it; the
# same rule the CLI tests pin).
board_at "$PLAIN" board open "$PLAIN" >/dev/null
plain_card="$(board_at "$PLAIN" card new --title plain-card --json)"
PLAIN_CARD_ID="$(printf '%s' "$plain_card" | jget id)"
plain_cards="$(board_at "$PLAIN" card list --json)"
printf '%s' "$plain_cards" | grep -q 'plain-card' || fail "plain cwd card missing"
printf '%s' "$plain_cards" | grep -q 'root-card' && fail "Git-root card leaked into plain board"
[ "$ROOT_CARD_ID" != "$PLAIN_CARD_ID" ] || fail "card ids unexpectedly reused"
ok "Git root/subdir share; exact non-Git cwd is independent"

step "Columns are independent per board"
repo_open="$(brpc board.open "$(python3 -c 'import json,sys; print(json.dumps({"scope_path":sys.argv[1]}))' "$REPO")")"
plain_open="$(brpc board.open "$(python3 -c 'import json,sys; print(json.dumps({"scope_path":sys.argv[1]}))' "$PLAIN")")"
REPO_BOARD_ID="$(printf '%s' "$repo_open" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["id"])')"
PLAIN_BOARD_ID="$(printf '%s' "$plain_open" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["id"])')"
brpc column.create "{\"board_id\":$REPO_BOARD_ID,\"name\":\"Repo Only\"}" >/dev/null
brpc column.create "{\"board_id\":$PLAIN_BOARD_ID,\"name\":\"Plain Only\"}" >/dev/null
# The selected project prevails over the cwd, so each listing names its
# project explicitly (also the selection update rule --board <path> follows).
repo_columns="$(board_at "$SUB" column list --board "$REPO" --json)"
plain_columns="$(board_at "$PLAIN" column list --board "$PLAIN" --json)"
printf '%s' "$repo_columns" | grep -q 'Repo Only' || fail "repo column missing from Git subdir"
printf '%s' "$repo_columns" | grep -q 'Plain Only' && fail "plain column leaked into repo board"
printf '%s' "$plain_columns" | grep -q 'Plain Only' || fail "plain column missing"
ok "pipeline columns stay board-scoped"

step "Global remains available through the protocol"
global="$(brpc board.get '{}')"
printf '%s' "$global" | python3 -c '
import json, sys
board = json.load(sys.stdin)["board"]
assert board["name"] == "main", "Global project first board must be main"
assert board["project_id"] == 1, "Global is project 1"
assert board["scope_path"] is None
' || fail "legacy Global board unavailable"

step "HERDR MUTATION: open scoped TUI from explicit plugin context"
# Re-select the repo project explicitly: after the plain-cwd card above, the
# selection is plain-notes, and the selected project prevails over the plugin
# context until another explicit selection.
board_at "$REPO" project select "$REPO" >/dev/null
ws_json="$(e2e_herdr_mutate -- workspace create --cwd "$SUB" --label cwd-boards --no-focus \
  --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET")"
WS_ID="$(printf '%s' "$ws_json" | jget workspace_id)"
e2e_ws_defer_close "$WS_ID"
tab_json="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label cwd-boards --no-focus)"
PANE_ID="$(printf '%s' "$tab_json" | jget pane_id)"
CONTEXT="$(python3 -c 'import json,sys; print(json.dumps({"focused_pane_cwd":sys.argv[1],"workspace_cwd":sys.argv[2]}))' "$SUB" "$PLAIN")"
CONTEXT_Q="$(printf '%q' "$CONTEXT")"
e2e_launch_tui "$PANE_ID" \
  "env -u BOARD_SCOPE_PATH HERDR_PLUGIN_CONTEXT_JSON=$CONTEXT_Q HERDR_PLUGIN_ID=herdr-board HERDR_PANE_ID=$PANE_ID HERDR_BIN_PATH=$HERDR_BIN HERDR_SOCKET_PATH=$HERDR_SOCKET_PATH BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG"

expected="Board [$(basename "$REPO") · ACTIVE]"
label=""
for _ in $(seq 1 60); do
  label="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
pid=sys.argv[1]
for pane in json.load(sys.stdin).get("panes",[]):
    if pane.get("pane_id")==pid:
        print(pane.get("label") or ""); break
' "$PANE_ID")"
  [ "$label" = "$expected" ] && break
  sleep .1
done
[ "$label" = "$expected" ] || fail "scoped TUI label '$label' (expected '$expected')"
e2e_herdr_mutate -- pane send-keys "$PANE_ID" b
sleep 0.5
screen="$("$HERDR_BIN" pane read "$PANE_ID" --source recent-unwrapped --lines 200 || true)"
grep -Fq 'Switch board' <<<"$screen" || fail "board picker did not open"
grep -Fq 'Other projects' <<<"$screen" || fail "Other projects entry missing from board picker"
grep -Fq 'main' <<<"$screen" \
  || fail "the project's first board (main) missing from board picker"
ok "TUI uses focused pane Git root; board picker offers the main board and other projects"

# Drill to the project picker: the row below the current board is
# 'Other projects…'. Global lives there as the special project, last.
e2e_herdr_mutate -- pane send-keys "$PANE_ID" down
e2e_herdr_mutate -- pane send-keys "$PANE_ID" enter
sleep 0.5
screen="$("$HERDR_BIN" pane read "$PANE_ID" --source recent-unwrapped --lines 200 || true)"
grep -Fq 'Switch project' <<<"$screen" || fail "project picker did not open after Other projects…"
grep -Fq 'Global' <<<"$screen" || fail "Global missing from project picker"
# The picker wraps long project rows (label + repo path split across lines at
# narrow widths), so the label and the canonical repo path are asserted
# separately rather than as one contiguous prefix.
grep -Fq 'project-one —' <<<"$screen" \
  || fail "project label missing from narrow project picker"
VISIBLE_REPO_PREFIX="$(dirname "$REPO")/project"
grep -Fq "$VISIBLE_REPO_PREFIX" <<<"$screen" \
  || fail "canonical repo path prefix missing from narrow project picker -- got: $screen"
ok "project picker lists the current project and Global last"

step "HERDR MUTATION: make the workspace cwd heterogeneous"
foreign_tab="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --cwd "$PLAIN" \
  --label heterogeneous-cwd --no-focus)"
FOREIGN_PANE_ID="$(printf '%s' "$foreign_tab" | jget pane_id)"
[ -n "$FOREIGN_PANE_ID" ] || fail "heterogeneous cwd pane was not created"
distinct_cwds="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,sys
values=sorted({p.get("cwd") for p in json.load(sys.stdin).get("panes",[]) if p.get("cwd")})
print("\n".join(values))
')"
grep -Fxq "$SUB" <<<"$distinct_cwds" || fail "workspace lost its original cwd: $distinct_cwds"
grep -Fxq "$PLAIN" <<<"$distinct_cwds" || fail "workspace lacks the second cwd: $distinct_cwds"

step "Explicit space_cwd controls auto-spawn placement"
brpc column.create "{\"board_id\":$REPO_BOARD_ID,\"name\":\"Cwd Execute\",\"trigger\":\"auto\"}" >/dev/null
explicit_card="$(board_at "$REPO" card create --title explicit-cwd --harness fake \
  --space-kind workspace --space-ref "$WS_ID" --space-cwd "$REPO" --json)"
EXPLICIT_CARD_ID="$(printf '%s' "$explicit_card" | jget id)"
BOARD_SCOPE_PATH="$REPO" e2e_board_herdr_mutate -- move "$EXPLICIT_CARD_ID" "Cwd Execute" --json >/dev/null
outcome="$(wait_ok "$EXPLICIT_CARD_ID")" || fail "explicit cwd card outcome '$outcome'"
run_cwd="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json,re,sys
card=re.escape(sys.argv[1])
pattern=re.compile(rf"^card-{card}-cwd-execute(?:-r[0-9]+)?$")
print(next((p.get("cwd") or "" for p in json.load(sys.stdin).get("panes",[]) if pattern.match(p.get("label") or "")), ""))
' "$EXPLICIT_CARD_ID")"
[ "$run_cwd" = "$REPO" ] || fail "explicit cwd pane landed at '$run_cwd' (expected '$REPO')"
ok "auto-spawn pane landed at the explicit Git root"

step "Missing override fails closed for heterogeneous workspace cwd"
ambiguous_card="$(board_at "$REPO" card create --title ambiguous-cwd --harness fake \
  --space-kind workspace --space-ref "$WS_ID" --json)"
AMBIGUOUS_CARD_ID="$(printf '%s' "$ambiguous_card" | jget id)"
BOARD_SCOPE_PATH="$REPO" e2e_board_herdr_mutate -- move "$AMBIGUOUS_CARD_ID" "Cwd Execute" --json >/dev/null
ambiguous_outcome="$(wait_ok "$AMBIGUOUS_CARD_ID" || true)"
[ "$ambiguous_outcome" = fail ] || fail "ambiguous cwd outcome '$ambiguous_outcome' (expected fail)"
ambiguous_show="$(board_at "$REPO" card show "$AMBIGUOUS_CARD_ID" --json)"
grep -Fq 'multiple live pane cwd candidates' <<<"$ambiguous_show" \
  || fail "ambiguous cwd failure did not explain the candidates"
ok "heterogeneous workspace without space_cwd failed closed"

step "12-cwd-boards: ALL CHECKS PASSED"
