#!/usr/bin/env bash
# 19-daemon-before-herdr.sh — always-on watcher connects after Herdr appears.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init_late_session
e2e_build
e2e_isolate
export E2E_FAKE_ENV="FAKE_AGENT_SLEEP=300"
e2e_write_config "$HERDR_BOARD_CONFIG"

# Boardd starts against an owned, currently absent proxy path. No personal or
# provider state is consulted.
LATE_SOCKET="$E2E_TMP/late-herdr.sock"
export HERDR_SOCKET_PATH="$LATE_SOCKET"
e2e_daemon_start
STATUS="$($BOARD_BIN status --json)"
printf '%s' "$STATUS" | python3 -c 'import json,sys; assert json.load(sys.stdin)["herdr_connected"] is False' \
  || fail "boardd unexpectedly connected before owned Herdr existed"

step "Boot the reserved session and publish it through the late socket"
e2e_start_reserved_session
e2e_proxy_start "$LATE_SOCKET" "$E2E_TMP/proxy-control.sock" "$E2E_SESSION_SOCKET"
e2e_ws_create daemon-before-herdr
WS_ID="$E2E_WS"

EXEC_ID="$(col_create '{"name":"Late Execute","trigger":"auto"}')"
CARD_JSON="$($BOARD_BIN card new --title 'Late Herdr watcher' --description 'provider-free late connect' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jget id)"
e2e_board_herdr_mutate -- move "$CARD_ID" "Late Execute" --json >/dev/null

PANE_ID=""
for _ in $(seq 1 100); do
  PANE_ID="$(card_field "$CARD_ID" runs[-1].herdr_pane_id 2>/dev/null || true)"
  [ -n "$PANE_ID" ] && break
  sleep 0.1
done
[ -n "$PANE_ID" ] || fail "late-connected daemon never dispatched a pane"

step "Close the exact owned pane and require watcher finalization"
e2e_herdr_mutate -- pane close "$PANE_ID" >/dev/null
for _ in $(seq 1 100); do
  outcome="$(card_field "$CARD_ID" runs[-1].outcome 2>/dev/null || true)"
  [ "$outcome" = fail ] && break
  sleep 0.1
done
[ "${outcome:-}" = fail ] || { e2e_card_failure_diag "$CARD_ID"; fail "late watcher did not finalize pane exit"; }
SHOW="$($BOARD_BIN card show "$CARD_ID" --json)"
python3 - "$SHOW" "$EXEC_ID" <<'PY'
import json,sys
x=json.loads(sys.argv[1]); expected=int(sys.argv[2])
assert x["card"]["status"] == "failed"
assert x["card"]["column_id"] == expected
matches=[c for c in x["comments"] if c["body"] == "pane exited without board done"]
assert len(matches) == 1
print("  late stream observed exact pane exit once")
PY
$BOARD_BIN status >/dev/null
step "19-daemon-before-herdr: ALL CHECKS PASSED"
