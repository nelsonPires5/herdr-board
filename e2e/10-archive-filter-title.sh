#!/usr/bin/env bash
# 10-archive-filter-title.sh — the TUI archive filter renames its Herdr pane
# border and keeps the board chrome minimal (no footer hint row or redundant
# header labels).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

step "HERDR MUTATION: create disposable workspace for the archive-filter TUI"
e2e_ws_create archive-filter; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "HERDR MUTATION: create a tab and launch the real TUI with plugin pane context"
tab_json="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label archive-filter --no-focus)"
PANE_ID="$(printf '%s' "$tab_json" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find pane for archive-filter tab"

# Verified against Herdr 0.8.2 / protocol 20: `pane rename <pane_id> <label>`.
# The plugin variables reproduce the real pane context without linking a plugin
# into anything except this disposable session/workspace.
E2E_TUI_COLS=52 e2e_launch_tui "$PANE_ID" \
  "HERDR_PLUGIN_ID=herdr-board HERDR_PANE_ID=$PANE_ID HERDR_BIN_PATH=$HERDR_BIN HERDR_SOCKET_PATH=$HERDR_SOCKET_PATH BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH"

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

step "Assert board chrome has direct filters and no persistent footer hint"
screen="$("$HERDR_BIN" pane read "$PANE_ID" --source visible --lines 200 || true)"
printf '%s\n' "$screen" | grep -q "? help" && fail "legacy '? help' footer label still visible"
printf '%s\n' "$screen" | grep -Eq '\[ (Active|Act|A) \].*\[ All \].*\[ (Archived|Arc|R) \]' \
  || fail "direct visibility filter chips not rendered"
# The multi-project header shows the Project and Board selectors as separate
# rows in Compact mode ('Project:' on row one, 'Board:' on row two).
printf '%s\n' "$screen" | grep -q 'Project:' \
  || fail "Compact header did not show the Project selector -- got: $screen"
printf '%s\n' "$screen" | grep -q 'Board:' \
  || fail "Compact header did not show the Board selector -- got: $screen"
printf '%s\n' "$screen" | grep -Eq 'Visible:' && fail "legacy Visible: header label still visible"
printf '%s\n' "$screen" | grep -q "shown" && fail "legacy shown count still visible"
printf '%s\n' "$screen" | grep -q "archived ·" && fail "legacy archived count still visible"
printf '%s\n' "$screen" | grep -q "column [0-9]" && fail "legacy column counter still visible"
ok "board chrome has direct filters and no persistent footer hint"

step "10-archive-filter-title: ALL CHECKS PASSED"
