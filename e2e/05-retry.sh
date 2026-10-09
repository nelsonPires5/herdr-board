#!/usr/bin/env bash
# 05-retry.sh — `board retry` re-runs a finished card as a NEW run.
#
# A card runs in an auto 'Execute' column with NO on_fail target, so a failing
# run parks the card `failed` in place (no transition). `board retry` then
# enqueues a fresh run in the SAME column. Asserts:
#   - after the first run: exactly 1 run row, outcome fail, started_at set
#     (the run really spawned), card status failed in the same column;
#   - after `board retry`: a SECOND run row spawns (distinct id, started_at
#     set, argv/prompt evidence present) and finishes with the expected
#     outcome fail; the card is failed in the same column again.
# A row without started_at (enqueue without spawn) or an unexpected outcome
# must NOT pass: both runs use FAKE_AGENT_OUTCOME=fail, so any other outcome
# (or a missing spawn) is a harness/dispatcher failure, not a pass.
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

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

e2e_ws_standard board-e2e   # step + e2e_ws_create + WS_ID + echo

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

step "Wait for the FIRST run to finish; assert 1 run, outcome fail, spawned, card failed in place"
oc="$(wait_runs "$CARD_ID" 1)" || { fail "first run never finished"; }
[ "$oc" = "fail" ] || fail "first run outcome '$oc', expected fail"
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/retry-first.json"
python3 - "$E2E_TMP/retry-first.json" "$EXEC_ID" <<'PY' || fail "first-run evidence failed"
import json, sys
show = json.load(open(sys.argv[1], encoding="utf-8"))
exec_id = int(sys.argv[2])
runs = show.get("runs", [])
assert len(runs) == 1, f"expected exactly 1 run before retry, got {len(runs)}"
run = runs[0]
assert run["outcome"] == "fail", run
assert run["started_at"], "first run has no started_at (spawn failure must not pass)"
assert run["argv_json"] and run["prompt_snapshot"], "first run missing argv/prompt evidence"
card = show["card"]
assert card["status"] == "failed", f"card status {card['status']!r}, expected 'failed'"
assert card["column_id"] == exec_id, f"card moved to {card['column_id']}; a no-on_fail failure must stay in Execute ({exec_id})"
print(f"[ok] 1 run (id {run['id']}), outcome fail, started, card failed in Execute", file=sys.stderr)
PY
n1=1
ok "1 run, outcome fail, started, card failed and still in Execute"

step "HERDR MUTATION: board retry $CARD_ID -> enqueue a NEW run in the same column"
mut "board retry $CARD_ID"
e2e_board_herdr_mutate -- retry "$CARD_ID" >/dev/null || fail "board retry failed"

step "Wait for the SECOND run; assert run count grew to 2 and the new run finished with the expected outcome"
oc2="$(wait_runs "$CARD_ID" 2)" || { e2e_card_failure_diag "$CARD_ID"; fail "retry did not spawn/finish a 2nd run"; }
[ "$oc2" = "fail" ] || { e2e_card_failure_diag "$CARD_ID"; fail "second run outcome '$oc2', expected 'fail' (FAKE_AGENT_OUTCOME=fail for both runs)"; }
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/retry-show.json"
python3 - "$E2E_TMP/retry-show.json" "$EXEC_ID" <<'PY' || fail "retry run evidence failed"
import json, sys
show = json.load(open(sys.argv[1], encoding="utf-8"))
exec_id = int(sys.argv[2])
runs = show.get("runs", [])
assert len(runs) == 2, f"expected 2 run rows after retry, got {len(runs)}"
first, second = runs
assert first["id"] != second["id"], "retry must create a distinct run row (no update-in-place)"
for label, run in (("first", first), ("second", second)):
    assert run["outcome"] == "fail", f"{label} run outcome {run['outcome']!r}, expected 'fail'"
    assert run["started_at"], f"{label} run has no started_at (spawn failure must not pass)"
    assert run["ended_at"], f"{label} run has no ended_at"
    assert run["argv_json"], f"{label} run has no argv evidence"
    assert run["prompt_snapshot"], f"{label} run has no prompt evidence"
card = show["card"]
assert card["status"] == "failed", f"card status {card['status']!r}, expected 'failed'"
assert card["column_id"] == exec_id, f"card moved to {card['column_id']}; retry must stay in Execute ({exec_id})"
print(f"[ok] run count 1 -> 2 (ids {first['id']} -> {second['id']}); both started, both fail, card failed in Execute", file=sys.stderr)
PY
n2=2
ok "board retry spawned a new run row (1 -> 2) that started and finished with outcome 'fail'; card failed in Execute"

step "05-retry: ALL CHECKS PASSED"
