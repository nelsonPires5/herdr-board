#!/usr/bin/env bash
# 40-adopt-existing-agent.sh - link a live Herdr agent without taking ownership.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_enable_fake_pi
export FAKE_PI_EXTERNAL=1 FAKE_PI_SLEEP=300
e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "Create a disposable workspace and start a provider-free external Pi agent"
e2e_ws_create adopt-existing; WS_ID="$E2E_WS"
PANE_ID="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json, sys
panes = json.load(sys.stdin).get("panes", [])
assert len(panes) == 1, panes
print(panes[0]["pane_id"])
')"
mut "agent start external-adopt --kind pi --pane $PANE_ID"
e2e_herdr_mutate -- agent start external-adopt --kind pi --pane "$PANE_ID" --timeout 30000 >/dev/null
hrpc agent.get "{\"target\":\"$PANE_ID\"}" >/dev/null \
  || fail "external agent was not visible through agent.get"

step "Adopt the exact live pane into the Global board"
adopted="$($BOARD_BIN --board 1 card adopt --title external-adopt \
  --description "linked provider-free E2E agent" --pane "$PANE_ID" \
  --origin-socket "$HERDR_SOCKET_PATH" --json)"
read -r CARD_ID RUN_ID ADOPTED_PANE < <(printf '%s' "$adopted" | python3 -c '
import json, sys
result = json.load(sys.stdin)
print(result["card"]["id"], result["run"]["id"], result["run"]["herdr_pane_id"])
')
[ "$ADOPTED_PANE" = "$PANE_ID" ] \
  || fail "adopted run did not retain the exact pane"
[ -n "$CARD_ID" ] && [ -n "$RUN_ID" ] || fail "adoption did not create a card and run"
ok "card $CARD_ID links external pane $PANE_ID"

step "Focus the linked run without creating or moving a pane"
before="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["panes"]))')"
e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$RUN_ID" \
  --origin-socket "$HERDR_SOCKET_PATH" --json >/dev/null
after="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["panes"]))')"
[ "$before" = "$after" ] || fail "focus changed the external workspace pane count"

step "Cancel the board link and prove the external pane stays alive"
e2e_board_herdr_mutate -- cancel "$CARD_ID" --json >/dev/null
hrpc pane.get "{\"pane_id\":\"$PANE_ID\"}" >/dev/null \
  || fail "board cancel killed the externally owned pane"
[ "$(card_field "$CARD_ID" runs[-1].outcome)" = "cancelled" ] \
  || fail "external run was not finalized as cancelled"
ok "cancel finalized only the board run; external pane remains alive"

step "40-adopt-existing-agent: ALL CHECKS PASSED"
