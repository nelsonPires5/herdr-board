#!/usr/bin/env bash
# 03-sessions.sh — multi-session behavior against a SECOND ephemeral herdr session.
#
# One boardd drives every herdr session. The primary ephemeral session (the one
# e2e_init binds the daemon to = its "default") is session A; this scenario BOOTS
# its OWN collision-resistant second ephemeral session B
# (`hb-e2e-b-<pid>-<random>-<random>`) and exercises the
# session/space-scoped paths against it:
#   - space list scoped per session (A/default vs B differ),
#   - a `workspace` card dispatched into B (pane lands in B's kanban tab),
#   - a `new-workspace` card (label + cwd) the daemon creates in B,
#   - validation error when --space-cwd is missing for new-workspace,
#   - unknown-session error.
# The daemon reaches B by name (`--session B`): session enumeration shells out to
# `herdr session list --json` (board-daemon/src/session.rs), so B is visible even
# though the daemon is bound to A. Cleanup closes both disposable workspaces in B
# and tears B down (kept for review under E2E_KEEP=1). No pre-existing second
# session is required anymore — this boots one itself.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"   # keep panes alive for the assertions

e2e_init          # boots/adopts session A, binds the daemon's HERDR_SOCKET_PATH to it
e2e_build
e2e_isolate
e2e_daemon_start

# --- boot a SECOND ephemeral session B --------------------------------------
SESS="$(e2e_session_name 'hb-e2e-b-')"
step "Booting a second ephemeral herdr session '$SESS' (~2s)"
e2e_session_boot "$SESS" SESS_SOCK SESS_B_PID SESS_B_IDENTITY
e2e_defer_session_teardown "$SESS" "$SESS_B_PID" "$SESS_B_IDENTITY"
[ -n "$SESS_SOCK" ] || fail "could not resolve socket path for session '$SESS'"
echo "  session B: $SESS"
echo "  session B socket: $SESS_SOCK (server pid ${SESS_B_PID:-?})"
hrpc_sess() { HERDR_SOCKET_PATH="$SESS_SOCK" python3 "$HRPC" "$@"; }

# assert_kanban_pane <ws_id> <card_id> — the workspace in SESS has a single
# `kanban` tab holding an agent pane named card-<card_id>-execute.
assert_kanban_pane() {
  local ws="$1" card="$2" tabs panes
  tabs="$(hrpc_sess tab.list "{\"workspace_id\":\"$ws\"}")"
  panes="$(hrpc_sess pane.list "{\"workspace_id\":\"$ws\"}")"
  python3 - "$tabs" "$panes" "$card" <<'PY' || fail "kanban pane assertion failed (ws $ws, card $card)"
import json, re, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
card = sys.argv[3]
kanban = [t for t in tabs if t.get("label") == "kanban"]
if len(kanban) != 1:
    sys.exit(f"expected one kanban tab, got {[t.get('label') for t in tabs]}")
ktab = kanban[0]["tab_id"]
# the daemon names the agent pane via its herdr label; on an agent_name_taken
# collision (the session may already hold a card-<id>-execute pane) it retries
# with a -r<run> fallback, so accept that suffix (see AGENTS.md).
want = re.compile(rf"^card-{re.escape(card)}-execute(-r\d+)?$")
labels = [p.get("label") for p in panes if p.get("tab_id") == ktab]
match = next((l for l in labels if l and want.match(l)), None)
if not match:
    sys.exit(f"no pane matching card-{card}-execute[-r<n>] in kanban tab labels {labels}")
print(f"  [ok] kanban tab {ktab} holds agent pane {match}", file=sys.stderr)
PY
}

# --- space list scoped per session ------------------------------------------
step "HERDR MUTATION: create a disposable workspace in session '$SESS'"
e2e_ws_create bsess-ws "$SESS_SOCK" "$SESS_B_PID" "$SESS_B_IDENTITY"; WS_SESS="$E2E_WS"
echo "  workspace in $SESS: $WS_SESS"

step "space list scoped per session (default vs $SESS)"
default_spaces="$("$BOARD_BIN" space list --json)"
sess_spaces="$("$BOARD_BIN" space list --session "$SESS" --json)"
echo "  default:"; printf '%s\n' "$default_spaces" | sed 's/^/    /'
echo "  $SESS:";   printf '%s\n' "$sess_spaces" | sed 's/^/    /'
printf '%s' "$sess_spaces" | grep -q '"label": "bsess-ws"' \
  || fail "bsess-ws not listed in session '$SESS' spaces"
printf '%s' "$default_spaces" | grep -q '"label": "bsess-ws"' \
  && fail "bsess-ws leaked into the DEFAULT session's spaces"
ok "space list is correctly scoped per session"

# --- cross-session workspace card -------------------------------------------
step "Dispatch a 'workspace' card into session '$SESS'"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title "Sess WS Card" -d "cross-session ws" \
  --harness fake --space-kind workspace --space-ref "$WS_SESS" --session "$SESS" --json)"
CARD_WS="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
mut "board move $CARD_WS Execute -> agent.start in session $SESS / ws $WS_SESS"
"$BOARD_BIN" move "$CARD_WS" Execute --json >/dev/null
oc="$(wait_ok "$CARD_WS")" || { tail -40 "$E2E_TMP/daemon.log"; fail "card $CARD_WS outcome '$oc'"; }
echo "  outcome: $oc"
assert_kanban_pane "$WS_SESS" "$CARD_WS"
ok "workspace card ran in session '$SESS' and landed a pane in its kanban tab"

# --- new-workspace card (daemon creates the workspace) ----------------------
step "Dispatch a 'new-workspace' card (label+cwd) into session '$SESS'"
card_json="$("$BOARD_BIN" card new --title "Sess New Card" -d "cross-session new-ws" \
  --harness fake --space-kind new-workspace --space-ref bsess-new --space-cwd "$E2E_TMP" \
  --session "$SESS" --json)"
CARD_NEW="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
mut "board move $CARD_NEW Execute -> daemon workspace.create(label=bsess-new) in $SESS"
"$BOARD_BIN" move "$CARD_NEW" Execute --json >/dev/null
oc="$(wait_ok "$CARD_NEW")" || { tail -40 "$E2E_TMP/daemon.log"; fail "card $CARD_NEW outcome '$oc'"; }
echo "  outcome: $oc"

# Find the workspace the daemon created (by label) and register its cleanup.
NEW_WS="$(hrpc_sess workspace.list '{}' | python3 -c '
import json, sys
ws = json.load(sys.stdin).get("workspaces", [])
print(next((w["workspace_id"] for w in ws if w.get("label") == "bsess-new"), ""))
')"
[ -n "$NEW_WS" ] || fail "daemon did not create the bsess-new workspace in session '$SESS'"
e2e_ws_defer_close "$NEW_WS" "$SESS_SOCK" "$SESS_B_PID" "$SESS_B_IDENTITY"
echo "  daemon-created workspace: $NEW_WS"
assert_kanban_pane "$NEW_WS" "$CARD_NEW"
ok "new-workspace card created a workspace in '$SESS' with a kanban pane"

# --- validation & error paths -----------------------------------------------
step "Validation: new-workspace WITHOUT --space-cwd must error"
if "$BOARD_BIN" card new --title bad -d bad --harness fake \
     --space-kind new-workspace --space-ref nope --session "$SESS" --json >/dev/null 2>"$E2E_TMP/err.txt"; then
  fail "expected an error for new-workspace missing --space-cwd"
fi
grep -q "space_cwd" "$E2E_TMP/err.txt" || fail "unexpected error: $(cat "$E2E_TMP/err.txt")"
ok "missing --space-cwd is rejected: $(cat "$E2E_TMP/err.txt")"

step "Unknown-session error (space list --session <bogus>)"
if "$BOARD_BIN" space list --session __no_such_session__ --json >/dev/null 2>"$E2E_TMP/err2.txt"; then
  fail "expected an error for an unknown session"
fi
grep -q "not found" "$E2E_TMP/err2.txt" || fail "unexpected error: $(cat "$E2E_TMP/err2.txt")"
ok "unknown session is rejected: $(cat "$E2E_TMP/err2.txt")"

step "03-sessions: ALL CHECKS PASSED"
