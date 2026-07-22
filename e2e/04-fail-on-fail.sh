#!/usr/bin/env bash
# 04-fail-on-fail.sh — a failing run follows the column's on_fail_column_id.
#
# Wires an auto column 'Execute' with on_fail -> a manual 'Backlog' column, runs
# a fake agent that reports `board done --outcome fail`, and asserts:
#   - the run's outcome is recorded as `fail`,
#   - the card is moved into the on_fail column (Backlog), status `idle`
#     (transition into a MANUAL column parks the card, no re-enqueue),
#   - a `system` transition comment records "Execute failed in <dur> -> Backlog".
#
# Grounds: engine.rs::decide_transition (fail -> on_fail_column_id), ops.rs::run_done
# -> finalize_run(Fail, transition=true). Mirrors the crate test
# `fail_path_applies_on_fail` (crates/board-cli/tests/integration.rs) live over herdr.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_OUTCOME=fail"   # the run reports failure

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create the on_fail target column 'Backlog' (manual) and 'Execute' (auto, on_fail->Backlog)"
BACKLOG_ID="$(col_create '{"name":"Backlog","trigger":"manual"}')"
[ -n "$BACKLOG_ID" ] || fail "could not create/parse Backlog column"
echo "  Backlog column id: $BACKLOG_ID"
EXEC_ID="$(col_create "{\"name\":\"Execute\",\"trigger\":\"auto\",\"on_fail_column_id\":$BACKLOG_ID}")"
[ -n "$EXEC_ID" ] || fail "could not create/parse Execute column"
echo "  Execute column id: $EXEC_ID (on_fail -> $BACKLOG_ID)"

step "Create a card and move it into 'Execute' (dispatches a fake agent that fails)"
card_json="$("$BOARD_BIN" card new --title "Fail Card" -d "fails on purpose" \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Execute -> agent.start in $WS_ID"
e2e_board_herdr_mutate -- move "$CARD_ID" Execute --json >/dev/null

step "Wait for the run to finish and assert outcome=fail"
oc="$(wait_runs "$CARD_ID" 1)" || { fail "run never finished (last outcome '$oc')"; }
[ "$oc" = "fail" ] || { e2e_card_failure_diag "$CARD_ID"; fail "expected run outcome 'fail', got '$oc'"; }
ok "run outcome recorded as fail"

step "Assert the card moved into the on_fail column (Backlog) and parked idle"
col_now="$(card_field "$CARD_ID" card.column_id || true)"
status_now="$(card_field "$CARD_ID" card.status || true)"
echo "  card column_id=$col_now status=$status_now (want column=$BACKLOG_ID status=idle)"
[ "$col_now" = "$BACKLOG_ID" ] || { e2e_card_failure_diag "$CARD_ID"; fail "card in column $col_now, expected on_fail column $BACKLOG_ID"; }
[ "$status_now" = "idle" ] || fail "card status '$status_now', expected 'idle' (manual on_fail parks the card)"
ok "card landed in the on_fail column, status idle"

step "Assert a system transition comment records the fail -> Backlog move"
show="$("$BOARD_BIN" card show "$CARD_ID" --json)"
grep -q "Execute failed in" <<<"$show" \
  || { e2e_card_failure_diag "$CARD_ID"; fail "no 'Execute failed in ... -> Backlog' system comment"; }
grep -q "Backlog" <<<"$show" \
  || { e2e_card_failure_diag "$CARD_ID"; fail "transition comment does not name the Backlog target"; }
ok "system transition comment present"

step "04-fail-on-fail: ALL CHECKS PASSED"
