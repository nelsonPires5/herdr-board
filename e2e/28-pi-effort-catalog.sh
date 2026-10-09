#!/usr/bin/env bash
# 28-pi-effort-catalog.sh — LIVE SMOKE: the daemon's live Pi catalog reaches
# the real TUI effort selector without starting Pi or a provider.
#
# The ordering matrix lives hermetically (no live infra):
# - authenticated-provider filtering + thinkingLevelMap tri-state semantics in
#   canonical order: board-core pi_catalog tests.
# - the catalog model's corrected ordering reaching the form selector (full
#   ladder and sparse-map narrowing): board-tui forms tests.
#
# What stays live: the daemon resolving the fixture files at startup into
# harness.capabilities, and the real TUI surfacing the catalog model with its
# efforts (first rung only — the full ordering is pinned hermetically).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# The Pi files must exist before boardd starts because its live catalog path is
# resolved once at daemon startup. Keep the normal safety order around this
# interleaving: init first, then build/isolate, fixture files, daemon last.
e2e_init
e2e_build
e2e_isolate
PI_AGENT_DIR="$HOME/.pi/agent"
mkdir -m 700 -p "$PI_AGENT_DIR"
cat >"$PI_AGENT_DIR/auth.json" <<'JSON'
{"openai-codex":{"type":"oauth"}}
JSON
cat >"$PI_AGENT_DIR/models-store.json" <<'JSON'
{
  "openai-codex": {
    "models": [
      {
        "id": "gpt-effort-e2e",
        "reasoning": true,
        "thinkingLevelMap": {
          "minimal": "low",
          "xhigh": "xhigh",
          "max": "max"
        }
      }
    ]
  },
  "unauthenticated": {
    "models": [{"id":"must-not-appear","reasoning":true}]
  }
}
JSON
chmod 600 "$PI_AGENT_DIR/auth.json" "$PI_AGENT_DIR/models-store.json"

# CI's ephemeral Herdr has no builtin integration installed, so a live
# `integration.list` would filter pi out of harness.list and the new-card
# default would become the config-defined `fake`; the pi catalog model would
# then never reach the form. Pin discovery to pi through the owned proxy
# (every other request is forwarded). Keep boardd last: its live catalog path
# is resolved once at startup.
e2e_proxy_start "$E2E_TMP/herdr-proxy.sock" "$E2E_TMP/proxy-control.sock" \
  "$E2E_SESSION_SOCKET"
e2e_proxy_command integration_list_available pi >/dev/null
export HERDR_SOCKET_PATH="$E2E_PROXY_SOCKET"
e2e_daemon_start
# boardd captured the proxy socket as its default at startup; this shell's own
# herdr CLI mutations (workspace/tab/pane) must keep targeting the real
# ephemeral session, exactly as before the proxy was introduced.
export HERDR_SOCKET_PATH="$E2E_SESSION_SOCKET"

step "Board RPC exposes the authenticated provider-prefixed Pi model and exact efforts"
CAPS="$(brpc harness.capabilities '{"harness":"pi"}')"
printf '%s\n' "$CAPS" | python3 -c '
import json,sys
caps=json.load(sys.stdin)
assert [m["id"] for m in caps["models"]] == ["openai-codex/gpt-effort-e2e"], caps
assert caps["models"][0]["efforts"] == ["off","minimal","low","medium","high","xhigh","max"], caps
'
ok "harness.capabilities returned the exact canonical Pi effort list"

step "Launch the real TUI in an owned disposable Herdr workspace/tab"
e2e_ws_create pi-efforts; WS_ID="$E2E_WS"
TAB_JSON="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label pi-efforts --no-focus)"
TUI_PANE="$(printf '%s' "$TAB_JSON" | jget pane_id)"
[ -n "$TUI_PANE" ] || fail "could not find pane for pi-efforts tab"
e2e_launch_tui "$TUI_PANE" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH"

read_pane() {
  "$HERDR_BIN" pane read "$TUI_PANE" --source recent-unwrapped --lines 200 2>/dev/null || true
}

wait_for() {
  local needle="$1" screen i
  for (( i=0; i<100; i++ )); do
    screen="$(read_pane)"
    printf '%s\n' "$screen" | grep -Fqi "$needle" && return 0
    sleep 0.1
  done
  return 1
}

ARTIFACT_DIR="${E2E_SCENARIO_ARTIFACT_DIR:-$E2E_TMP}"
PANE_EVIDENCE="$ARTIFACT_DIR/pi-effort-pane-read.txt"
: >"$PANE_EVIDENCE"
chmod 600 "$PANE_EVIDENCE"

wait_for "Todo" || fail "real TUI did not render"
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" n >/dev/null
wait_for "New card" || fail "new-card form did not render"

# Fixed visible-field order: title -> description -> harness -> model -> effort.
# Pi is the selected default because the proxy reports it installed; three Tabs
# focus model, Right picks the sole catalog model, and one more Tab focuses
# effort. No Enter: the card is never submitted.
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" tab tab tab >/dev/null
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" right >/dev/null
wait_for "gpt-effort-e2e" || {
  read_pane >>"$PANE_EVIDENCE"
  fail "Pi catalog model did not appear in the real TUI"
}
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" tab >/dev/null
wait_for "[ ‹ ]  default effort  [ › ]" || fail "effort field did not start at default effort"

step "The real TUI surfaces the catalog model with its first effort rung"
# One step right from default effort must land on `off` (canonical order,
# first rung): proof the catalog efforts reached the live selector. The full
# off..max ordering is pinned by the pi_catalog + forms hermetic tests.
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" right >/dev/null
wait_for "[ ‹ ]  off  [ › ]" || {
  read_pane >>"$PANE_EVIDENCE"
  fail "effort selector did not expose catalog efforts starting at 'off'"
}
{
  printf '\n===== effort=off =====\n'
  read_pane
} >>"$PANE_EVIDENCE"
ok "real TUI exposed the Pi catalog model with its effort ladder"
echo "  pane evidence: $PANE_EVIDENCE"

step "28-pi-effort-catalog: ALL CHECKS PASSED"
