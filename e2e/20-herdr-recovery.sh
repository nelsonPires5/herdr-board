#!/usr/bin/env bash
# 20-herdr-recovery.sh — proxy outage, restart, durable timeout, and event gap.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate
export E2E_FAKE_ENV="FAKE_AGENT_SLEEP=300"
e2e_write_config "$HERDR_BOARD_CONFIG"

REAL_HERDR_SOCKET="$E2E_SESSION_SOCKET"
e2e_proxy_start "$E2E_TMP/herdr-proxy.sock" "$E2E_TMP/proxy-control.sock" "$REAL_HERDR_SOCKET"
export HERDR_SOCKET_PATH="$E2E_PROXY_SOCKET"
e2e_daemon_start
e2e_ws_create herdr-recovery "$REAL_HERDR_SOCKET" "$E2E_SESSION_PID" "$E2E_SESSION_IDENTITY"
WS_ID="$E2E_WS"

wait_for_pane() {
  local card="$1" pane=""
  for _ in $(seq 1 120); do
    pane="$(card_field "$card" runs[-1].herdr_pane_id 2>/dev/null || true)"
    [ -n "$pane" ] && { printf '%s' "$pane"; return 0; }
    sleep 0.1
  done
  return 1
}

wait_for_fail() {
  local card="$1" outcome=""
  for _ in $(seq 1 240); do
    outcome="$(card_field "$card" runs[-1].outcome 2>/dev/null || true)"
    [ "$outcome" = fail ] && return 0
    sleep 0.1
  done
  return 1
}

step "Phase A: outage + daemon crash preserves the original timeout deadline"
BACKLOG_ID="$(col_create '{"name":"Recovery Backlog","trigger":"manual"}')"
TIMEOUT_PARAMS="$(python3 - "$BACKLOG_ID" <<'PY'
import json,sys
print(json.dumps({"name":"Recovery Timeout","trigger":"auto","timeout_minutes":8,
                  "on_fail_column_id":int(sys.argv[1])}))
PY
)"
TIMEOUT_ID="$(col_create "$TIMEOUT_PARAMS")"
CARD_A_JSON="$($BOARD_BIN card new --title 'Recovery timeout' --description 'provider-free outage' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_A="$(printf '%s' "$CARD_A_JSON" | jget id)"
e2e_board_herdr_mutate -- move "$CARD_A" "Recovery Timeout" --json >/dev/null
PANE_A="$(wait_for_pane "$CARD_A")" || fail "phase A pane did not start"
sleep 3

e2e_proxy_command offline >/dev/null
e2e_daemon_kill_owned
e2e_daemon_start
sleep 1
OPEN="$($BOARD_BIN card show "$CARD_A" --json)"
python3 - "$OPEN" "$TIMEOUT_ID" <<'PY'
import json,sys
x=json.loads(sys.argv[1]); expected=int(sys.argv[2]); run=x["runs"][-1]
assert run["ended_at"] is None and run["outcome"] is None
assert x["card"]["column_id"] == expected
assert x["card"]["status"] in ("running","blocked","awaiting")
assert not any("pane exited" in c["body"] for c in x["comments"])
print("  unavailable snapshot remained Unknown and open")
PY

e2e_proxy_command online >/dev/null
wait_for_fail "$CARD_A" || { e2e_card_failure_diag "$CARD_A"; fail "original timeout deadline was not enforced"; }
FINAL_A="$($BOARD_BIN card show "$CARD_A" --json)"
python3 - "$FINAL_A" "$BACKLOG_ID" <<'PY'
import json,sys
x=json.loads(sys.argv[1]); expected=int(sys.argv[2])
assert x["card"]["column_id"] == expected
assert x["runs"][-1]["outcome"] == "fail"
comments=[c for c in x["comments"] if c["body"].startswith("run timed out after ")]
assert len(comments) == 1
print("  durable timeout finalized and transitioned exactly once")
PY

step "Phase B: dropped stream + rejected retry is repaired by reconnect snapshot"
GAP_ID="$(col_create '{"name":"Recovery Gap","trigger":"auto"}')"
CARD_B_JSON="$($BOARD_BIN card new --title 'Recovery event gap' --description 'provider-free stream gap' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_B="$(printf '%s' "$CARD_B_JSON" | jget id)"
e2e_board_herdr_mutate -- move "$CARD_B" "Recovery Gap" --json >/dev/null
PANE_B="$(wait_for_pane "$CARD_B")" || fail "phase B pane did not start"
for _ in $(seq 1 50); do
  PROXY_BEFORE="$(e2e_proxy_command status)"
  SUBS_BEFORE="$(printf '%s' "$PROXY_BEFORE" | jget subscriptions)"
  [ "$SUBS_BEFORE" -gt 0 ] && break
  sleep 0.1
done
[ "${SUBS_BEFORE:-0}" -gt 0 ] || fail "watcher never subscribed through proxy"

e2e_proxy_command reject_events >/dev/null
e2e_herdr_mutate "$E2E_SESSION_PID" "$E2E_SESSION_IDENTITY" "$REAL_HERDR_SOCKET" -- pane close "$PANE_B" >/dev/null
sleep 1
MID_B="$($BOARD_BIN card show "$CARD_B" --json)"
python3 - "$MID_B" <<'PY'
import json,sys
x=json.loads(sys.argv[1])
assert x["runs"][-1]["ended_at"] is None
assert not any(c["body"] in ("pane exited without board done", "daemon restart: pane exited") for c in x["comments"])
print("  dropped event did not invent a terminal observation")
PY

e2e_proxy_command allow_events >/dev/null
wait_for_fail "$CARD_B" || { e2e_card_failure_diag "$CARD_B"; fail "reconnect snapshot did not close event gap"; }
PROXY_AFTER="$(e2e_proxy_command status)"
SUBS_AFTER="$(printf '%s' "$PROXY_AFTER" | jget subscriptions)"
[ "$SUBS_AFTER" -gt "$SUBS_BEFORE" ] || fail "watcher did not create a new subscription generation"
FINAL_B="$($BOARD_BIN card show "$CARD_B" --json)"
python3 - "$FINAL_B" "$GAP_ID" <<'PY'
import json,sys
x=json.loads(sys.argv[1]); expected=int(sys.argv[2])
assert x["card"]["column_id"] == expected and x["card"]["status"] == "failed"
assert x["runs"][-1]["outcome"] == "fail"
comments=[c for c in x["comments"] if c["body"] in
          ("pane exited without board done", "daemon restart: pane exited")]
assert len(comments) == 1
print("  reconnect snapshot finalized the missing pane exactly once")
PY
$BOARD_BIN status >/dev/null
step "20-herdr-recovery: ALL CHECKS PASSED"
