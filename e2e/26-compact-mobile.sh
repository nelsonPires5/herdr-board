#!/usr/bin/env bash
# 26-compact-mobile.sh — explicit mobile coverage: `LayoutMode::Compact`
# (< 60 cols) against the real TUI in a disposable pane forced to 40 columns.
#
# Asserts the Compact contract that 12-cwd-boards and 21-active-run-timer
# used to check by accident (at whatever width the host window happened to
# have) and that the `b` regression fix (b => Boards level directly, not
# Columns) depends on:
#   - row one shows the global running count and row three renders the focused
#     column + `(M/A)` + `n/N` (the count is not repeated per column);
#   - row two has the board dropdown and direct visibility controls with no
#     `Board:`/`Visible:` prose;
#   - `b` reaches the board list directly (a known board name appears);
#   - the header/switcher sheet shows the `[ X ]` close affordance;
#   - a card title in the focused column is visible.
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

step "Compact header renders identity/count, board filters, and focused column trigger"
wait_for 'Todo \(M\) · 1/1' || fail "Compact header did not show the focused column, trigger, and position at 40 cols -- got: $(read_pane)"
screen="$(read_pane)"
grep -Fq '● 0 running' <<<"$screen" \
  || fail "Compact identity did not show the global running count (0) -- got: $screen"
grep -Eq '\[ (Act|Active) \].*\[ All \].*\[ (Arc|Archived) \]' <<<"$screen" \
  || fail "Compact row two did not show all visibility controls -- got: $screen"
! grep -Eq 'Board:|Visible:' <<<"$screen" \
  || fail "Compact header retained a redundant Board:/Visible: label -- got: $screen"
ok "Compact header shows global count, direct filters, trigger, and position"

step "The focused column's card title is visible"
screen="$(read_pane)"
grep -Fq 'Compact Mobile Card' <<<"$screen" \
  || fail "focused column's card title not visible in the Compact board view"
ok "card title visible in the focused (only) column"

step "'b' reaches the board list directly (Compact 'b' => Boards level, not Columns)"
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" b >/dev/null
wait_for 'Boards' || fail "switcher sheet did not open after 'b'"
wait_for 'Global' || fail "board list did not show the known board name 'Global' -- got: $(read_pane)"
ok "'b' opened the switcher sheet directly at the board list"

step "The switcher sheet shows the [ X ] close affordance (not [ Close ])"
screen="$(read_pane)"
grep -Fq '[ X ]' <<<"$screen" \
  || fail "switcher sheet did not show the [ X ] close affordance"
! grep -Fq '[ Close ]' <<<"$screen" \
  || fail "legacy [ Close ] button is still visible"
ok "[ X ] close affordance present"

step "Esc closes the sheet outright (opened directly via 'b', nothing to back out to)"
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" esc >/dev/null
wait_for 'Todo \(M\) · 1/1' || fail "board view did not return after Esc"
screen="$(read_pane)"
grep -Fq 'Compact Mobile Card' <<<"$screen" \
  || fail "board view lost its card after closing the switcher"
ok "Esc closed the sheet and returned to the Compact board view"

step "26-compact-mobile: ALL CHECKS PASSED"
