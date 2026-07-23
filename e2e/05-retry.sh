#!/usr/bin/env bash
# 05-retry.sh — `board retry` re-runs a finished card as a NEW run.
#
# A card runs in an auto 'Execute' column with NO on_fail target, so a failing
# run parks the card `failed` in place (no transition). `board retry` then
# enqueues a fresh run in the SAME column. Asserts:
#   - after the first run: exactly 1 run row, outcome fail, card status failed;
#   - after `board retry`: a SECOND run row spawns and finishes (run count 1->2);
#   - the card is back to a terminal state driven by that new run.
#
# Session semantics: the fake harness is not a real coding agent and never
# reports a harness conversation id (`session_id` stays null), so a live retry
# cannot prove `--resume` reuse the way the crate test
# `retry_creates_new_forked_run` does. This scenario asserts only what IS
# observable over herdr: the new run row, its outcome, and card state.
#
# Grounds: ops.rs::run_retry -> enqueue_run(is_retry=true) -> a new `runs` row in
# the card's current column; db.list_runs counts every run (no update-in-place).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_OUTCOME=fail"   # both runs report failure

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create an auto column 'Execute' (no on_fail -> a failed run parks in place)"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
[ -n "$EXEC_ID" ] || fail "could not create/parse Execute column"
echo "  Execute column id: $EXEC_ID"

step "Create a card and move it into 'Execute' (first run fails)"
card_json="$("$BOARD_BIN" card new --title "Retry Card" -d "retry me" \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Execute -> agent.start in $WS_ID"
e2e_board_herdr_mutate -- move "$CARD_ID" Execute --json >/dev/null

step "Wait for the FIRST run to finish; assert 1 run, outcome fail, card failed in place"
oc="$(wait_runs "$CARD_ID" 1)" || { fail "first run never finished"; }
[ "$oc" = "fail" ] || fail "first run outcome '$oc', expected fail"
n1="$("$BOARD_BIN" card show "$CARD_ID" --json | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("runs",[])))')"
[ "$n1" = "1" ] || fail "expected exactly 1 run before retry, got $n1"
st="$(card_field "$CARD_ID" card.status || true)"
col="$(card_field "$CARD_ID" card.column_id || true)"
[ "$st" = "failed" ] || fail "card status '$st', expected 'failed'"
[ "$col" = "$EXEC_ID" ] || fail "card moved to $col; a no-on_fail failure must stay in Execute ($EXEC_ID)"
ok "1 run, outcome fail, card failed and still in Execute"

step "HERDR MUTATION: board retry $CARD_ID -> enqueue a NEW run in the same column"
mut "board retry $CARD_ID"
e2e_board_herdr_mutate -- retry "$CARD_ID" >/dev/null || fail "board retry failed"

step "Wait for the SECOND run; assert run count grew to 2 and the new run finished"
oc2="$(wait_runs "$CARD_ID" 2)" || { e2e_card_failure_diag "$CARD_ID"; fail "retry did not spawn/finish a 2nd run"; }
n2="$("$BOARD_BIN" card show "$CARD_ID" --json | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("runs",[])))')"
[ "$n2" = "2" ] || fail "expected 2 run rows after retry, got $n2"
echo "  run count: $n1 -> $n2 ; new run outcome: $oc2"
[ -n "$oc2" ] && [ "$oc2" != "<timeout>" ] || fail "new run has no recorded outcome"
ok "board retry spawned a new run row (1 -> 2) that finished with outcome '$oc2'"

step "05-retry: ALL CHECKS PASSED"
