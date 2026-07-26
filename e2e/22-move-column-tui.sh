#!/usr/bin/env bash
# 22-move-column-tui.sh — the `M` (Shift+m) TUI mini-mode reorders the focused
# column through the same column.reorder path as a mouse drag, with a true
# Esc cancel.
#
# Asserts (all provider-free; manual columns, no agent dispatch):
#   - entering `M` shows the centered "Move column" banner,
#   - Esc after a staged move restores the original column order (0 RPCs),
#   - Enter commits exactly one column.reorder and the persisted order flips.
#
# The real TUI runs in a disposable Herdr pane; column order is read back
# straight from the isolated boardd via board.get (the post-Enter truth source),
# and the in-mode banner is read from the rendered pane.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace for the move-column TUI"
e2e_ws_create move-column; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create two extra manual columns: [Todo, Alpha, Beta]"
ALPHA_ID="$(col_create '{"name":"Alpha","trigger":"manual"}')"
BETA_ID="$(col_create '{"name":"Beta","trigger":"manual"}')"
echo "  Alpha=$ALPHA_ID Beta=$BETA_ID"

# column_names — space-joined column names in persisted (position) order.
column_names() {
  brpc board.get "$(printf '{\"board_id\":%s}' "$E2E_BOARD_ID")" \
    | python3 -c 'import json,sys; print(" ".join(c["name"] for c in json.load(sys.stdin)["columns"]))'
}

# wait_columns <expected-names> — poll board.get until the persisted column
# order matches (the post-Enter truth source, independent of the TUI's refetch).
wait_columns() {
  local expected="$1" got="" i
  for (( i=0; i<100; i++ )); do
    got="$(column_names 2>/dev/null || true)"
    [ "$got" = "$expected" ] && return 0
    sleep 0.1
  done
  fail "column order '$got' (expected '$expected')"
}

# wait_screen <pane> <substring> — poll the rendered pane until <substring> shows.
wait_screen() {
  local pane="$1" needle="$2" screen="" i
  for (( i=0; i<100; i++ )); do
    screen="$("$HERDR_BIN" pane read "$pane" --source recent-unwrapped --lines 200 2>/dev/null || true)"
    printf '%s\n' "$screen" | grep -Fq "$needle" && return 0
    sleep 0.1
  done
  fail "pane did not render '$needle'"
}

step "Assert the seed order before driving the TUI"
wait_columns "Todo Alpha Beta"
ok "initial column order is Todo Alpha Beta"

step "Launch the real TUI in a disposable pane against the isolated boardd"
TAB_JSON="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label move-column --no-focus)"
PANE_ID="$(printf '%s' "$TAB_JSON" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find pane for move-column tab"
e2e_launch_tui "$PANE_ID" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH"

step "Wait for the TUI to render its first column header"
wait_screen "$PANE_ID" "Todo"
ok "real TUI is up"

step "HERDR MUTATION: stage a column move with M then cancel with Esc"
# Focus starts on column 0 (Todo); right focuses Alpha (index 1).
e2e_herdr_mutate -- pane send-keys "$PANE_ID" right >/dev/null
# Enter the M mini-mode on the focused column. send-text delivers the literal
# capital-M byte (Shift+m) that crossterm reads as Char('M'); send-keys tokenizes
# named keys (right/enter/esc) instead.
e2e_herdr_mutate -- pane send-text "$PANE_ID" M >/dev/null
wait_screen "$PANE_ID" "Move column"
ok "M shows the centered 'Move column' banner"
# Slide Alpha one slot right (local only): [Todo, Beta, Alpha], then cancel.
e2e_herdr_mutate -- pane send-keys "$PANE_ID" right >/dev/null
e2e_herdr_mutate -- pane send-keys "$PANE_ID" esc >/dev/null

step "Esc must restore the original order without persisting anything"
wait_columns "Todo Alpha Beta"
ok "Esc cancelled: order unchanged (Todo Alpha Beta)"

step "HERDR MUTATION: stage + commit a column move with M then Enter"
# After Esc the focus is back on Alpha (its restored position, index 1).
e2e_herdr_mutate -- pane send-text "$PANE_ID" M >/dev/null
e2e_herdr_mutate -- pane send-keys "$PANE_ID" right >/dev/null
e2e_herdr_mutate -- pane send-keys "$PANE_ID" enter >/dev/null

step "Enter commits exactly one column.reorder; persisted order must flip"
wait_columns "Todo Beta Alpha"
ok "committed column order is Todo Beta Alpha"

step "22-move-column-tui: ALL CHECKS PASSED"
