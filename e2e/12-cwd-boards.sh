#!/usr/bin/env bash
# 12-cwd-boards.sh — Git-root/CWD scopes isolate pipelines/cards and drive the scoped TUI picker.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

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
repo_columns="$(board_at "$SUB" column list --json)"
plain_columns="$(board_at "$PLAIN" column list --json)"
printf '%s' "$repo_columns" | grep -q 'Repo Only' || fail "repo column missing from Git subdir"
printf '%s' "$repo_columns" | grep -q 'Plain Only' && fail "plain column leaked into repo board"
printf '%s' "$plain_columns" | grep -q 'Plain Only' || fail "plain column missing"
ok "pipeline columns stay board-scoped"

step "Global remains available through the protocol"
global="$(brpc board.get '{}')"
[ "$(printf '%s' "$global" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["name"])')" = "Global" ] \
  || fail "legacy Global board unavailable"

step "HERDR MUTATION: open scoped TUI from explicit plugin context"
ws_json="$(e2e_herdr_mutate -- workspace create --cwd "$SUB" --label cwd-boards --no-focus \
  --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET")"
WS_ID="$(printf '%s' "$ws_json" | jget workspace_id)"
e2e_ws_defer_close "$WS_ID"
tab_json="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label cwd-boards --no-focus)"
PANE_ID="$(printf '%s' "$tab_json" | jget pane_id)"
CONTEXT="$(python3 -c 'import json,sys; print(json.dumps({"focused_pane_cwd":sys.argv[1],"workspace_cwd":sys.argv[2]}))' "$SUB" "$PLAIN")"
CONTEXT_Q="$(printf '%q' "$CONTEXT")"
e2e_herdr_mutate -- pane run "$PANE_ID" \
  "env -u BOARD_SCOPE_PATH HERDR_PLUGIN_CONTEXT_JSON=$CONTEXT_Q HERDR_PLUGIN_ID=herdr-board HERDR_PANE_ID=$PANE_ID HERDR_BIN_PATH=$HERDR_BIN HERDR_SOCKET_PATH=$HERDR_SOCKET_PATH BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG $BOARD_BIN tui"

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
grep -Fq 'Global' <<<"$screen" || fail "Global missing from board picker"
VISIBLE_REPO_PREFIX="project-one — $(dirname "$REPO")/project-on"
grep -Fq "$VISIBLE_REPO_PREFIX" <<<"$screen" \
  || fail "canonical repo path/prefix missing from narrow board picker"
ok "TUI uses focused pane Git root and picker includes Global"

step "12-cwd-boards: ALL CHECKS PASSED"
