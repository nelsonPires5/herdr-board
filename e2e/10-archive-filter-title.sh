#!/usr/bin/env bash
# 10-archive-filter-title.sh — the TUI archive filter renames its Herdr pane
# border and keeps the board footer minimal.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace for the archive-filter TUI"
e2e_ws_create archive-filter; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "HERDR MUTATION: create a tab and launch the real TUI with plugin pane context"
tab_json="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label archive-filter --no-focus)"
PANE_ID="$(printf '%s' "$tab_json" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find pane for archive-filter tab"

# Verified against Herdr 0.7.5 / protocol 17: `pane rename <pane_id> <label>`.
# The plugin variables reproduce the real pane context without linking a plugin
# into anything except this disposable session/workspace.
e2e_herdr_mutate -- pane run "$PANE_ID" \
  "HERDR_PLUGIN_ID=herdr-board HERDR_PANE_ID=$PANE_ID HERDR_BIN_PATH=$HERDR_BIN HERDR_SOCKET_PATH=$HERDR_SOCKET_PATH BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH $BOARD_BIN tui"

pane_label() {
  hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json, sys
pane_id = sys.argv[1]
for pane in json.load(sys.stdin).get("panes", []):
    if pane.get("pane_id") == pane_id:
        print(pane.get("label") or "")
        sys.exit(0)
sys.exit(1)
' "$PANE_ID"
}

wait_label() {
  local expected="$1" label="" i
  for (( i=0; i<50; i++ )); do
    label="$(pane_label 2>/dev/null || true)"
    [ "$label" = "$expected" ] && return 0
    sleep 0.1
  done
  fail "pane label '$label' (expected '$expected')"
}

SCOPE_LABEL="$(basename "$BOARD_SCOPE_PATH")"
step "Assert startup scope + filter are rendered in the Herdr pane title"
wait_label "Board [$SCOPE_LABEL · ACTIVE]"
ok "startup pane title is Board [$SCOPE_LABEL · ACTIVE]"

step "Cycle ACTIVE -> ALL -> ARCHIVED and assert each scoped title"
e2e_herdr_mutate -- pane send-keys "$PANE_ID" v
wait_label "Board [$SCOPE_LABEL · ALL]"
e2e_herdr_mutate -- pane send-keys "$PANE_ID" v
wait_label "Board [$SCOPE_LABEL · ARCHIVED]"
ok "archive filter stays synchronized with the Herdr pane title"

step "Assert the board footer is minimal"
screen="$("$HERDR_BIN" pane read "$PANE_ID" --source recent-unwrapped --lines 200 || true)"
printf '%s\n' "$screen" | grep -q "? help" || fail "minimal '? help' footer missing"
printf '%s\n' "$screen" | grep -q "shown" && fail "legacy shown count still visible"
printf '%s\n' "$screen" | grep -q "archived ·" && fail "legacy archived count still visible"
printf '%s\n' "$screen" | grep -q "column [0-9]" && fail "legacy column counter still visible"
ok "footer contains only the help affordance"

step "10-archive-filter-title: ALL CHECKS PASSED"
