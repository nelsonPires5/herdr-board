#!/usr/bin/env bash
# 15-awaiting.sh — a live Herdr agent-status signal parks a run in `awaiting`;
# explicit board confirmation then closes it as `done` in the same column.
#
# Herdr 0.7.5 / protocol 17 and Pi integration v6 were verified before
# pinning the argv below:
#   herdr pane report-agent <pane_id> --source ID --agent LABEL
#     --state idle|working|blocked|unknown [--seq N]
# `done` is an output AgentStatus but is NOT an accepted pane.report_agent input
# state, so a supported CLI cannot inject it directly. This scenario emulates
# the installed Pi integration's public report path (`source=herdr:pi`): prove
# blocked/working events reach boardd, then report the integration's end-of-turn
# idle state. On a managed agent.start pane Herdr derives the output status
# `done`; the scenario asserts that live status and the board's `agent_done` arm.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Keep the pane alive without board done. Cleanup closes its disposable workspace.
export E2E_FAKE_ENV="FAKE_AGENT_SLEEP=300"

e2e_init
e2e_build
e2e_isolate

# Core config keys are top-level. Keep idle expiry well beyond this scenario's
# expected immediate Herdr `done` derivation so it cannot win the signal race.
python3 - "$HERDR_BOARD_CONFIG" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
p.write_text("idle_grace_seconds = 10\n" + p.read_text(encoding="utf-8"), encoding="utf-8")
PY

e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-awaiting-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create an auto column with no on_success and a four-second test timeout"
EXEC_ID="$(col_create '{"name":"Await Execute","trigger":"auto","timeout_minutes":4}')"
[ -n "$EXEC_ID" ] || fail "could not create Await Execute column"

step "Create and dispatch a held fake-agent card"
card_json="$("$BOARD_BIN" card new --title "Awaiting Card" \
  --description "wait for human confirmation" --harness fake \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
mut "board move $CARD_ID 'Await Execute' -> agent.start in $WS_ID"
e2e_board_herdr_mutate -- move "$CARD_ID" "Await Execute" --json >/dev/null

step "Wait for the run to start and retain its live pane"
PANE_ID=""
for _ in $(seq 1 80); do
  PANE_ID="$(card_field "$CARD_ID" runs[-1].herdr_pane_id 2>/dev/null || true)"
  STARTED="$(card_field "$CARD_ID" runs[-1].started_at 2>/dev/null || true)"
  if [ -n "$PANE_ID" ] && [ -n "$STARTED" ] \
      && hrpc pane.get "{\"pane_id\":\"$PANE_ID\"}" >/dev/null 2>&1; then
    break
  fi
  sleep .1
done
[ -n "$PANE_ID" ] || { fail "run never recorded a live pane"; }
monotonic_ms() { python3 -c 'import time; print(time.monotonic_ns() // 1_000_000)'; }
RUN_STARTED_OBSERVED_MS="$(monotonic_ms)"
ok "run started in live pane $PANE_ID"

status_of() { card_field "$CARD_ID" card.status 2>/dev/null || true; }
wait_status() {
  local expected="$1" tries="${2:-60}" actual="" i
  for (( i=0; i<tries; i++ )); do
    actual="$(status_of)"
    [ "$actual" = "$expected" ] && return 0
    sleep .1
  done
  fail "card status '$actual' (expected '$expected')"
}

# Use increasing authoritative-integration sequence numbers.
SEQ="$(( $(date +%s) * 1000 ))"
report_agent() {
  local state="$1"
  SEQ=$((SEQ + 1))
  e2e_herdr_mutate -- pane report-agent "$PANE_ID" \
    --source herdr:pi --agent pi --state "$state" --seq "$SEQ" >/dev/null
}

step "Prove the live pane.agent_status_changed subscription reaches boardd"
# If the first report races watcher subscription setup, toggle once more so
# Herdr emits another transition instead of relying on an arbitrary sleep.
for _ in $(seq 1 10); do
  report_agent working
  report_agent blocked
  [ "$(status_of)" = "blocked" ] && break
  sleep .1
done
wait_status blocked
ok "integration-style blocked signal changed the card to blocked"

report_agent working
wait_status running
ok "integration-style working signal resumed the card to running"

step "Report the integration's end-of-turn idle; wait for Herdr done -> awaiting"
report_agent idle
wait_status awaiting 80
AWAITING_OBSERVED_MS="$(monotonic_ms)"

"$BOARD_BIN" card show "$CARD_ID" --json | python3 -c '
import json, sys
x = json.load(sys.stdin)
card, run = x["card"], x["runs"][-1]
assert card["status"] == "awaiting"
assert card["awaiting_reason"] == "agent_done"
assert run["outcome"] is None
assert run["ended_at"] is None
print("  awaiting_reason=agent_done; run remains open")
' || fail "awaiting/open-run assertions failed"
hrpc pane.get "{\"pane_id\":\"$PANE_ID\"}" | python3 -c '
import json, sys
pane = json.load(sys.stdin)["pane"]
assert pane["agent_status"] == "done"
print("  live Herdr pane agent_status=done")
' || fail "agent pane is not live with Herdr status done"

step "Assert awaiting pauses the original column timeout"
TIMEOUT_DURATION_MS=4000
PAUSE_MARGIN_MS=1000
PAUSE_PROOF_DEADLINE_MS=$((AWAITING_OBSERVED_MS + TIMEOUT_DURATION_MS + PAUSE_MARGIN_MS))
# Start this proof clock only after `awaiting` was observed: spawn and signal setup
# cannot consume the interval. Staying awaiting for a full configured timeout plus
# margin, while polling card/run/pane state, proves the column timeout is paused.
while :; do
  "${BOARD_BIN}" card show "$CARD_ID" --json | python3 -c '
import json, sys
x = json.load(sys.stdin)
assert x["card"]["status"] == "awaiting"
assert x["runs"][-1]["outcome"] is None
assert x["runs"][-1]["ended_at"] is None
' || fail "awaiting run changed before confirmation"
  hrpc pane.get "{\"pane_id\":\"$PANE_ID\"}" >/dev/null 2>&1 \
    || fail "awaiting pane disappeared before confirmation"
  NOW_MS="$(monotonic_ms)"
  [ "$NOW_MS" -gt "$PAUSE_PROOF_DEADLINE_MS" ] && break
  sleep .1
done
RUN_OBSERVED_ELAPSED_MS=$((NOW_MS - RUN_STARTED_OBSERVED_MS))
AWAITING_ELAPSED_MS=$((NOW_MS - AWAITING_OBSERVED_MS))
ok "card stayed awaiting with an open run/pane for ${AWAITING_ELAPSED_MS}ms after awaiting was observed (${RUN_OBSERVED_ELAPSED_MS}ms since confirmed run start)"

step "Confirm through the supported board run.done CLI contract"
e2e_board_herdr_mutate -- done "$CARD_ID" --outcome ok --json >/dev/null

"$BOARD_BIN" card show "$CARD_ID" --json | python3 -c '
import json, sys
expected_column = int(sys.argv[1])
x = json.load(sys.stdin)
card, run = x["card"], x["runs"][-1]
assert card["status"] == "done"
assert card["column_id"] == expected_column
assert card["awaiting_reason"] is None
assert run["outcome"] == "ok"
assert run["ended_at"] is not None
print(f"  done in column {expected_column}; run outcome=ok and ended_at set")
' "$EXEC_ID" || fail "done/same-column/ended-run assertions failed"

hrpc pane.get "{\"pane_id\":\"$PANE_ID\"}" >/dev/null \
  || fail "held pane did not remain alive through confirmation"
ok "board done ok is terminal truth; card is done without moving columns"

step "15-awaiting: ALL CHECKS PASSED"
