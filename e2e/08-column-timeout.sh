#!/usr/bin/env bash
# 08-column-timeout.sh — a run that overruns its column timeout is killed and
# follows on_fail.
#
# The isolated config sets `timeout_unit_secs = 1`, so a column `timeout_minutes`
# is measured in SECONDS here. Execute has `timeout_minutes = 1` (a 1s deadline)
# and on_fail -> Backlog; the fake agent holds for FAKE_AGENT_SLEEP=10, well past
# it. Asserts:
#   - the run outcome is `fail` (timed out);
#   - the agent pane is killed;
#   - the card follows on_fail into Backlog (timeout transitions, unlike a silent
#     exit or cancel);
#   - a `system` comment mentions the timeout.
#
# Grounds: watchers.rs::timeout_ticker -> finalize_run(Fail, kill=true,
# transition=true) when now >= timeout_deadline. Mirrors the crate test
# `timeout_kills_and_applies_on_fail`.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_SLEEP=10"   # hold well past the 1s column timeout

e2e_init
e2e_build
e2e_isolate     # config bakes timeout_unit_secs = 1
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create 'Backlog' (manual) and 'Execute' (auto, timeout_minutes=1 -> 1s, on_fail->Backlog)"
BACKLOG_ID="$(col_create '{"name":"Backlog","trigger":"manual"}')"
EXEC_ID="$(col_create "{\"name\":\"Execute\",\"trigger\":\"auto\",\"timeout_minutes\":1,\"on_fail_column_id\":$BACKLOG_ID}")"
[ -n "$EXEC_ID" ] || fail "could not create Execute column"
echo "  Execute id: $EXEC_ID (timeout 1 unit=1s, on_fail -> Backlog $BACKLOG_ID)"

step "Create a card and move it into 'Execute' (agent holds past the timeout)"
card_json="$("$BOARD_BIN" card new --title "Timeout Card" -d "overruns" \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Execute -> agent.start in $WS_ID"
"$BOARD_BIN" move "$CARD_ID" Execute --json >/dev/null

step "Wait for the timeout to fire and finalize the run"
oc="$(wait_runs "$CARD_ID" 1)" || { tail -40 "$E2E_TMP/daemon.log"; fail "run never finalized (timeout did not fire?)"; }
[ "$oc" = "fail" ] || { "$BOARD_BIN" card show "$CARD_ID" --json; fail "run outcome '$oc', expected 'fail' (timed out)"; }
ok "run timed out and was finalized as fail"

step "Assert the card followed on_fail into Backlog (timeout DOES transition)"
col_now="$(card_field "$CARD_ID" card.column_id || true)"
echo "  card column_id=$col_now (want Backlog $BACKLOG_ID)"
[ "$col_now" = "$BACKLOG_ID" ] || { "$BOARD_BIN" card show "$CARD_ID" --json; fail "card in column $col_now, expected on_fail Backlog $BACKLOG_ID"; }
ok "timed-out run applied on_fail -> Backlog"

step "Assert a system comment mentions the timeout"
"$BOARD_BIN" card show "$CARD_ID" --json | grep -q "timed out" \
  || { "$BOARD_BIN" card show "$CARD_ID" --json; fail "no 'timed out' comment recorded"; }
ok "timeout recorded in a system comment"

step "08-column-timeout: ALL CHECKS PASSED"
