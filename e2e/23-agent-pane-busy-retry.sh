#!/usr/bin/env bash
# 23-agent-pane-busy-retry.sh — agent.start busy retry and bounded cleanup.
#
# A transparent, owned proxy injects the typed agent_pane_busy response. The
# transient case must retry agent.start on the already-owned child pane (not
# split again); the persistent case must eventually fail and close only that
# child. Pi is the checked-in
# provider-free managed fixture; no provider credentials or calls are involved.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Fake-managed setup creates roots before e2e_init, so arm cleanup first.
trap e2e_cleanup EXIT
e2e_enable_fake_pi
e2e_init
e2e_build
e2e_isolate

# Create disposable workspaces. New v11 cards allocate their own `card-<id>`
# tabs; no user tab is treated as an ownership anchor.
e2e_ws_create busy-transient
TRANSIENT_WS="$E2E_WS"
e2e_ws_create busy-persistent
PERSISTENT_WS="$E2E_WS"

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
  local layout_json="$1" pane_id="$2" expected_count="$3" expected_splits="$4" child="${5:-}"
  python3 - "$layout_json" "$pane_id" "$expected_count" "$expected_splits" "$child" <<'PY'
import json, sys
layout = json.loads(sys.argv[1]).get("layout", {})
panes = layout.get("panes", [])
splits = layout.get("splits", [])
ids = {pane.get("pane_id") for pane in panes}
pane_id, count, split_count, child = sys.argv[2:]
if pane_id:
    assert pane_id in ids
assert len(panes) == int(count)
assert len(splits) == int(split_count)
if child:
    assert child in ids
else:
    assert not child
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
python3 - "$TRANSIENT_STATUS" "$TRANSIENT_PANE" <<'PY'
import json, sys
status = json.loads(sys.argv[1])
starts = status["agent_start_panes"]
assert status["busy_injections"] == 1
assert len(starts) == 2
assert starts == [sys.argv[2], sys.argv[2]]
assert len(status["pane_splits"]) == 1
# A successful fresh managed launch closes its anchor, leaving exactly the
# harness pane; only that anchor may have been closed.
assert len(status["pane_closes"]) == 1
assert status["pane_closes"][0] != sys.argv[2]
PY
TRANSIENT_LAYOUT="$(hrpc pane.layout "{\"pane_id\":\"$TRANSIENT_PANE\"}")"
layout_check "$TRANSIENT_LAYOUT" "$TRANSIENT_PANE" 1 0 "$TRANSIENT_PANE"
ok "transient busy retried agent.start on one newly allocated child"

step "Persistent agent_pane_busy must clean the owned child"
# AGENT_START_BUSY_RETRIES=5: persistent busy therefore produces exactly
# five delayed retries plus the initial start, i.e. six attempts/injections.
EXPECTED_PERSISTENT_ATTEMPTS=6
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
python3 - "$BEFORE_PERSISTENT" "$PERSISTENT_STATUS" "$EXPECTED_PERSISTENT_ATTEMPTS" <<'PY'
import json, sys
before, after = (json.loads(value) for value in sys.argv[1:3])
expected_attempts = int(sys.argv[3])
starts = after["agent_start_panes"][len(before["agent_start_panes"]):]
closes = after["pane_closes"][len(before["pane_closes"]):]
assert len(starts) == expected_attempts
assert after["busy_injections"] - before["busy_injections"] == expected_attempts
assert len(set(starts)) == 1
assert len(after["pane_splits"][len(before["pane_splits"]):]) == 1
assert closes == [starts[0]]
PY
PERSISTENT_PANES="$(hrpc pane.list "{\"workspace_id\":\"$PERSISTENT_WS\"}")"
python3 - "$PERSISTENT_PANES" "$PERSISTENT_CARD" <<'PY'
import json, sys
panes=json.loads(sys.argv[1]).get("panes",[])
card=sys.argv[2]
anchors=[p for p in panes if p.get("label") == f"card-{card}-anchor" and not p.get("agent")]
assert len(anchors) == 1
assert not any(card in (p.get("label") or "") and p not in anchors for p in panes)
PY
[ "$(card_field "$PERSISTENT_CARD" runs[-1].outcome)" = fail ] \
  || fail "persistent run did not have fail outcome"
[ "$(card_field "$PERSISTENT_CARD" card.status)" = failed ] \
  || fail "persistent card did not remain failed"
ok "persistent busy closed only its newly allocated owned child"

step "23-agent-pane-busy-retry: SAME PANE + SAFE CLEANUP PASSED"
