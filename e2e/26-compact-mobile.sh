#!/usr/bin/env bash
# 26-compact-mobile.sh — minimal Compact smoke against the real TUI in a
# disposable pane forced to 40 columns.
#
# Live scope is startup/input/return only: the TUI boots in Compact mode, a
# keypress reaches it, and it returns to the board view. Every detailed
# label/geometry contract this scenario used to assert — the Project:/Board:
# header rows, the `(M/A)` trigger + position, the visibility filters, the
# board-picker rows, the `[ X ]` (never `[ Close ]`) affordance, the visible
# card title — is pinned hermetically against the TestBackend in
# `crates/board-tui/tests/snapshots.rs`
# (`responsive_header_and_minimal_footer_in_every_layout`,
# `compact_visibility_filters_fit_without_dropping_archived`,
# `compact_board_picker_lists_main_with_icon_close`) plus the picker reducer
# in `crates/board-tui/tests/update/switcher.rs`
# (`b_in_compact_opens_the_board_picker`,
# `esc_from_direct_board_picker_returns_to_board`).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

step "Create a card in the default (only, focused) column"
CARD_JSON="$($BOARD_BIN card new --title 'Compact Mobile Card' --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"

step "HERDR MUTATION: open a tab and launch 'board tui' in it, forced to a 40-col Compact pane"
e2e_ws_create compact-mobile; WS_ID="$E2E_WS"
TAB_JSON="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label compact-mobile --no-focus)"
TUI_PANE="$(printf '%s' "$TAB_JSON" | jget pane_id)"
[ -n "$TUI_PANE" ] || fail "could not find pane for compact-mobile tab"
echo "  tui pane: $TUI_PANE"
E2E_TUI_COLS=40 e2e_launch_tui "$TUI_PANE" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH"

read_pane() {
  "$HERDR_BIN" pane read "$TUI_PANE" --source recent-unwrapped --lines 200 2>/dev/null || true
}

wait_for() {
  local pattern="$1" tries="${2:-100}" screen
  for _ in $(seq 1 "$tries"); do
    screen="$(read_pane)"
    printf '%s\n' "$screen" | grep -Eq "$pattern" && return 0
    sleep 0.1
  done
  return 1
}

step "Compact startup: the forced-width board view renders the focused column"
wait_for 'Todo \(M\) · 1/1' \
  || fail "Compact board view did not start at 40 cols -- got: $(read_pane)"
ok "Compact startup renders at 40 columns"

step "Input reaches the TUI: 'b' opens the board picker"
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" b >/dev/null
wait_for 'Switch board' || fail "board picker did not open after 'b' -- got: $(read_pane)"
ok "input reached the TUI and opened the picker"

step "Return: Esc closes the picker back to the Compact board view"
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" esc >/dev/null
wait_for 'Todo \(M\) · 1/1' || fail "board view did not return after Esc -- got: $(read_pane)"
ok "Esc returned to the Compact board view"

step "26-compact-mobile: ALL CHECKS PASSED"
