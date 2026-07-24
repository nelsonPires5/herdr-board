#!/usr/bin/env bash
# 23-agent-pane-busy-retry.sh — agent.start busy retry and bounded cleanup.
#
# A transparent, owned proxy injects the typed agent_pane_busy response. The
# transient case must retry agent.start on the already-owned child pane (not
# split again); the persistent case must eventually fail, close only that child,
# and leave the pre-existing anchor pane untouched. Pi is the checked-in
# provider-free managed fixture; no provider credentials or calls are involved.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Fake-managed setup creates roots before e2e_init, so arm cleanup first.
trap e2e_cleanup EXIT
e2e_enable_fake_pi
e2e_init
e2e_build
e2e_isolate

# Create both disposable workspaces and their pre-existing kanban anchors on
# the real session socket. Once the proxy is installed, scenario-side Herdr
# mutations remain identity-gated against the real socket; boardd/read probes
# use the proxy.
e2e_ws_create busy-transient
TRANSIENT_WS="$E2E_WS"
e2e_ws_create busy-persistent
PERSISTENT_WS="$E2E_WS"

TRANSIENT_TAB="$(e2e_herdr_mutate -- tab create --workspace "$TRANSIENT_WS" \
  --label kanban --no-focus)"
TRANSIENT_ANCHOR="$(printf '%s' "$TRANSIENT_TAB" | jget pane_id)"
[ -n "$TRANSIENT_ANCHOR" ] || fail "transient anchor pane was not created"
PERSISTENT_TAB="$(e2e_herdr_mutate -- tab create --workspace "$PERSISTENT_WS" \
  --label kanban --no-focus)"
PERSISTENT_ANCHOR="$(printf '%s' "$PERSISTENT_TAB" | jget pane_id)"
[ -n "$PERSISTENT_ANCHOR" ] || fail "persistent anchor pane was not created"

REAL_HERDR_SOCKET="$E2E_SESSION_SOCKET"
e2e_proxy_start "$E2E_TMP/herdr-proxy.sock" "$E2E_TMP/proxy-control.sock" \
  "$REAL_HERDR_SOCKET"
export HERDR_SOCKET_PATH="$E2E_PROXY_SOCKET"
e2e_daemon_start

BUSY_COLUMN_ID="$(col_create '{"name":"Busy Execute","trigger":"auto"}')"
[ -n "$BUSY_COLUMN_ID" ] || fail "busy test column was not created"

wait_fail() {
  local card="$1" outcome="" i
  for (( i=0; i<160; i++ )); do
    outcome="$(card_field "$card" runs[-1].outcome 2>/dev/null || true)"
    [ "$outcome" = fail ] && { printf '%s' "$outcome"; return 0; }
    sleep 0.5
  done
  printf '%s' "${outcome:-<none>}"
  return 1
}

layout_check() {
  local layout_json="$1" anchor="$2" expected_count="$3" expected_splits="$4" child="${5:-}"
  python3 - "$layout_json" "$anchor" "$expected_count" "$expected_splits" "$child" <<'PY'
import json, sys
layout = json.loads(sys.argv[1]).get("layout", {})
panes = layout.get("panes", [])
splits = layout.get("splits", [])
ids = {pane.get("pane_id") for pane in panes}
anchor, count, split_count, child = sys.argv[2:]
assert anchor in ids
assert len(panes) == int(count)
assert len(splits) == int(split_count)
if child:
    assert child in ids
else:
    assert ids == {anchor}
PY
}

step "Transient agent_pane_busy must retry on the same owned child pane"
e2e_proxy_command agent_pane_busy_transient >/dev/null
TRANSIENT_CARD_JSON="$($BOARD_BIN card new --title 'Transient pane busy' \
  --description 'provider-free transient agent start race' --harness pi \
  --model p17/busy-transient --space-kind workspace --space-ref "$TRANSIENT_WS" --json)"
TRANSIENT_CARD="$(printf '%s' "$TRANSIENT_CARD_JSON" | jget id)"
e2e_board_herdr_mutate -- move "$TRANSIENT_CARD" "Busy Execute" --json >/dev/null
TRANSIENT_OUTCOME="$(wait_ok "$TRANSIENT_CARD" 160)" || {
  e2e_card_failure_diag "$TRANSIENT_CARD"
  fail "transient agent_pane_busy outcome '$TRANSIENT_OUTCOME'"
}
[ "$TRANSIENT_OUTCOME" = ok ] || fail "transient run outcome was '$TRANSIENT_OUTCOME'"
TRANSIENT_PANE="$(card_field "$TRANSIENT_CARD" runs[-1].herdr_pane_id)"
[ -n "$TRANSIENT_PANE" ] || fail "transient run did not retain a pane id"
TRANSIENT_STATUS="$(e2e_proxy_command status)"
python3 - "$TRANSIENT_STATUS" "$TRANSIENT_PANE" "$TRANSIENT_ANCHOR" <<'PY'
import json, sys
status = json.loads(sys.argv[1])
starts = status["agent_start_panes"]
assert status["busy_injections"] == 1
assert len(starts) == 2
assert starts == [sys.argv[2], sys.argv[2]]
assert status["pane_splits"] == [sys.argv[3]]
assert not status["pane_closes"]
PY
TRANSIENT_LAYOUT="$(hrpc pane.layout "{\"pane_id\":\"$TRANSIENT_ANCHOR\"}")"
layout_check "$TRANSIENT_LAYOUT" "$TRANSIENT_ANCHOR" 2 1 "$TRANSIENT_PANE"
ok "transient busy retried agent.start on one child after one split"

step "Persistent agent_pane_busy must clean the owned child and preserve the anchor"
# AGENT_START_BUSY_RETRIES=2: persistent busy therefore produces exactly
# two delayed retries plus the initial start, i.e. three attempts/injections.
EXPECTED_PERSISTENT_ATTEMPTS=3
BEFORE_PERSISTENT="$(e2e_proxy_command status)"
e2e_proxy_command agent_pane_busy_persistent >/dev/null
PERSISTENT_CARD_JSON="$($BOARD_BIN card new --title 'Persistent pane busy' \
  --description 'provider-free persistent agent start race' --harness pi \
  --model p17/busy-persistent --space-kind workspace --space-ref "$PERSISTENT_WS" --json)"
PERSISTENT_CARD="$(printf '%s' "$PERSISTENT_CARD_JSON" | jget id)"
e2e_board_herdr_mutate -- move "$PERSISTENT_CARD" "Busy Execute" --json >/dev/null
PERSISTENT_OUTCOME="$(wait_fail "$PERSISTENT_CARD")" || {
  e2e_card_failure_diag "$PERSISTENT_CARD"
  fail "persistent agent_pane_busy did not fail the run (outcome '$PERSISTENT_OUTCOME')"
}
PERSISTENT_PANE="$(card_field "$PERSISTENT_CARD" runs[-1].herdr_pane_id 2>/dev/null || true)"
PERSISTENT_STATUS="$(e2e_proxy_command status)"
python3 - "$BEFORE_PERSISTENT" "$PERSISTENT_STATUS" "$PERSISTENT_ANCHOR" "$EXPECTED_PERSISTENT_ATTEMPTS" <<'PY'
import json, sys
before, after = (json.loads(value) for value in sys.argv[1:3])
anchor = sys.argv[3]
expected_attempts = int(sys.argv[4])
starts = after["agent_start_panes"][len(before["agent_start_panes"]):]
splits = after["pane_splits"][len(before["pane_splits"]):]
closes = after["pane_closes"][len(before["pane_closes"]):]
assert len(starts) == expected_attempts
assert after["busy_injections"] - before["busy_injections"] == expected_attempts
assert len(set(starts)) == 1
assert splits == [anchor]
assert len(closes) == 1
assert closes[0] == starts[0]
assert closes[0] != anchor
PY
PERSISTENT_LAYOUT="$(hrpc pane.layout "{\"pane_id\":\"$PERSISTENT_ANCHOR\"}")"
layout_check "$PERSISTENT_LAYOUT" "$PERSISTENT_ANCHOR" 1 0
[ "$(card_field "$PERSISTENT_CARD" runs[-1].outcome)" = fail ] \
  || fail "persistent run did not have fail outcome"
[ "$(card_field "$PERSISTENT_CARD" card.status)" = failed ] \
  || fail "persistent card did not remain failed"
ok "persistent busy closed only its owned child; pre-existing anchor survived"

step "23-agent-pane-busy-retry: SAME PANE + ONE SPLIT + SAFE CLEANUP PASSED"
