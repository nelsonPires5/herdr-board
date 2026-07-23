#!/usr/bin/env bash
# 21-active-run-timer.sh — the live TUI timer uses the open run start, not card activity.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Keep the configured fake harness alive so the run remains started/open while
# the real TUI is refreshed by a board event. It never receives a provider call
# or a prompt body in this scenario.
e2e_init
e2e_build
export E2E_FAKE_ENV="FAKE_AGENT_SLEEP=300"
e2e_isolate
e2e_write_config "$HERDR_BOARD_CONFIG"
e2e_daemon_start

e2e_ws_create active-run-timer
WS_ID="$E2E_WS"
col_create '{"name":"Timer","trigger":"auto"}' >/dev/null
CARD_JSON="$($BOARD_BIN card new --title 'Active run timer' --description 'timer fixture' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jget id)"
e2e_board_herdr_mutate -- move "$CARD_ID" Timer --json >/dev/null

wait_for_pane() {
  local pane
  for _ in $(seq 1 120); do
    pane="$(card_field "$CARD_ID" runs[-1].herdr_pane_id 2>/dev/null || true)"
    [ -n "$pane" ] && printf '%s' "$pane" && return 0
    sleep 0.1
  done
  return 1
}

PANE_ID="$(wait_for_pane)" || fail "active timer run did not register a pane"
sleep 2
RUN_ID="$(card_field "$CARD_ID" runs[-1].id)"
STARTED_AT="$(card_field "$CARD_ID" runs[-1].started_at)"
UPDATED_BEFORE="$(card_field "$CARD_ID" card.updated_at)"
[ -n "$RUN_ID" ] || fail "active run has no id"
[ -n "$STARTED_AT" ] || fail "active run has no started_at"

assert_active_summary() {
  local snapshot="$1"
  python3 - "$snapshot" "$CARD_ID" "$RUN_ID" "$STARTED_AT" "$E2E_BOARD_ID" <<'PY'
import json, sys
snapshot, card_id, run_id, started_at, board_id = sys.argv[1:]
data = json.loads(snapshot)
active_runs = data.get("active_runs")
assert isinstance(active_runs, list)
summary = [item for item in active_runs if str(item.get("card_id")) == card_id]
assert len(summary) == 1
assert summary[0]["started_at"] == started_at
# ActiveRunSummary intentionally carries card_id/start; the durable run id is
# checked alongside it from card.get below so a refresh cannot swap the run.
assert str(data["board"]["id"]) == board_id
PY
}

assert_run_identity() {
  local observed_id observed_start
  observed_id="$(card_field "$CARD_ID" runs[-1].id)"
  observed_start="$(card_field "$CARD_ID" runs[-1].started_at)"
  [ "$observed_id" = "$RUN_ID" ] || fail "active run id changed across refresh"
  [ "$observed_start" = "$STARTED_AT" ] || fail "active run start changed across refresh"
}

BOARD_BEFORE="$(brpc board.get "$(printf '{\"board_id\":%s}' "$E2E_BOARD_ID")")"
assert_active_summary "$BOARD_BEFORE"
assert_run_identity

step "Open the real TUI against the isolated board daemon"
TAB_JSON="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label board-timer --no-focus)"
TUI_PANE="$(printf '%s' "$TAB_JSON" | jget pane_id)"
[ -n "$TUI_PANE" ] || fail "TUI pane was not created"
e2e_herdr_mutate -- pane run "$TUI_PANE" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH $BOARD_BIN tui"

wait_for_tui_card() {
  local screen
  for _ in $(seq 1 100); do
    screen="$("$HERDR_BIN" pane read "$TUI_PANE" --source recent-unwrapped --lines 200 2>/dev/null || true)"
    printf '%s\n' "$screen" | grep -q 'Active run timer' && return 0
    sleep 0.1
  done
  return 1
}
wait_for_tui_card || fail "real TUI did not render the active card"

timer_seconds() {
  "$HERDR_BIN" pane read "$TUI_PANE" --source recent-unwrapped --lines 200 2>/dev/null \
    | python3 -c '
import re, sys
screen = sys.stdin.read()
match = re.search(r"running · (\d+)([smhd])", screen)
if not match:
    raise SystemExit(1)
print(int(match.group(1)) * {"s": 1, "m": 60, "h": 3600, "d": 86400}[match.group(2)])
'
}

wait_for_timer() {
  local sample
  for _ in $(seq 1 100); do
    sample="$(timer_seconds 2>/dev/null || true)"
    if [[ "$sample" =~ ^[0-9]+$ ]]; then
      printf '%s' "$sample"
      return 0
    fi
    sleep 0.1
  done
  return 1
}

step "Capture the displayed timer before the card refresh"
TIMER_BEFORE="$(wait_for_timer)" || fail "active card timer was not rendered before edit"
# Broad bounds catch a missing/bogus display without assuming scheduler timing;
# the comparison below is against the displayed, floored value, not wall time.
[ "$TIMER_BEFORE" -ge 0 ] && [ "$TIMER_BEFORE" -le 120 ] \
  || fail "pre-edit timer sample outside broad bound: ${TIMER_BEFORE}s"

step "Refresh the board through a title+description event without changing the run start"
# This updates cards.updated_at while the run stays open. The event causes the
# TUI to fetch a new board snapshot, so this checks both the event path and the
# additive active_runs payload. Fixture text is non-sensitive and is never
# printed as a prompt/log payload.
UPDATE_PARAMS="$(python3 - "$CARD_ID" <<'PY'
import json, sys
print(json.dumps({"id": int(sys.argv[1]),
                  "title": "Active run timer refreshed",
                  "description": "timer fixture touched"}))
PY
)"
brpc card.update "$UPDATE_PARAMS" >/dev/null
UPDATED_AFTER="$(card_field "$CARD_ID" card.updated_at)"
[ "$UPDATED_AFTER" != "$UPDATED_BEFORE" ] \
  || fail "card activity did not change updated_at"
[ "$(card_field "$CARD_ID" card.title)" = "Active run timer refreshed" ] \
  || fail "title edit was not persisted"
[ "$(card_field "$CARD_ID" card.description)" = "timer fixture touched" ] \
  || fail "description edit was not persisted"
assert_run_identity

# The API mutation emits the live board event; press the TUI's supported refresh
# key as a deterministic fallback/force so this assertion never depends on an
# event arriving in the same 200ms terminal-poll window.
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" r >/dev/null

wait_for_tui_title() {
  local screen
  for _ in $(seq 1 100); do
    screen="$("$HERDR_BIN" pane read "$TUI_PANE" --source recent-unwrapped --lines 200 2>/dev/null || true)"
    if printf '%s\n' "$screen" | grep -Fq 'Active run timer refre'; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}
wait_for_tui_title || fail "real TUI did not observe the card refresh event"

BOARD_AFTER_EDIT="$(brpc board.get "$(printf '{\"board_id\":%s}' "$E2E_BOARD_ID")")"
assert_active_summary "$BOARD_AFTER_EDIT"
assert_run_identity
TIMER_AFTER="$(wait_for_timer)" || fail "active card timer was not rendered after edit"
[ "$TIMER_AFTER" -ge "$TIMER_BEFORE" ] \
  || fail "displayed timer reset across refresh (${TIMER_BEFORE}s -> ${TIMER_AFTER}s)"
[ "$TIMER_AFTER" -le 180 ] \
  || fail "post-edit timer sample outside broad bound: ${TIMER_AFTER}s"

step "Complete the run through the identity-gated board done channel"
e2e_board_herdr_mutate -- done "$CARD_ID" --outcome ok --summary 'timer lifecycle complete' --json >/dev/null

BOARD_AFTER_DONE="$(brpc board.get "$(printf '{\"board_id\":%s}' "$E2E_BOARD_ID")")"
python3 - "$BOARD_AFTER_DONE" "$CARD_ID" <<'PY'
import json, sys
data, card_id = sys.argv[1:]
data = json.loads(data)
active_runs = data.get("active_runs")
assert isinstance(active_runs, list)
assert all(str(item.get("card_id")) != card_id for item in active_runs)
PY

step "21-active-run-timer: ALL CHECKS PASSED"
