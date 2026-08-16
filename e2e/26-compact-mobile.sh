#!/usr/bin/env bash
# 26-compact-mobile.sh — explicit mobile coverage: `LayoutMode::Compact`
# (< 60 cols) against the real TUI in a disposable pane forced to 40 columns.
#
# Asserts the Compact contract that 12-cwd-boards and 21-active-run-timer
# used to check by accident (at whatever width the host window happened to
# have):
#   - row one shows `Project: <name>` and the direct visibility controls;
#   - row two shows `Board: <name>`;
#   - row three renders the focused column + `(M/A)` + `n/N`;
#   - `b` opens the board picker directly (a known board name appears);
#   - the picker sheet shows the `[ X ]` close affordance;
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

step "Compact header renders project/board lines, filters, and focused column trigger"
wait_for 'Todo \(M\) · 1/1' || fail "Compact header did not show the focused column, trigger, and position at 40 cols -- got: $(read_pane)"
screen="$(read_pane)"
grep -Fq 'Project:' <<<"$screen" \
  || fail "Compact row one did not show the Project selector -- got: $screen"
grep -Fq 'Board:' <<<"$screen" \
  || fail "Compact row two did not show the Board selector -- got: $screen"
grep -Eq '\[ (Act|Active) \].*\[ All \].*\[ (Arc|Archived) \]' <<<"$screen" \
  || fail "Compact header did not show all visibility controls -- got: $screen"
! grep -Eq 'Visible:' <<<"$screen" \
  || fail "Compact header retained a redundant Visible: label -- got: $screen"
ok "Compact header shows separate Project/Board selectors, direct filters, trigger, and position"

step "The focused column's card title is visible"
screen="$(read_pane)"
grep -Fq 'Compact Mobile Card' <<<"$screen" \
  || fail "focused column's card title not visible in the Compact board view"
ok "card title visible in the focused (only) column"

step "'b' opens the board picker directly (Compact 'b' => board picker, not the columns switcher)"
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" b >/dev/null
wait_for 'Switch board' || fail "board picker did not open after 'b'"
wait_for 'main' || fail "board picker did not show the known board name 'main' -- got: $(read_pane)"
ok "'b' opened the board picker directly"

step "The picker sheet shows the [ X ] close affordance (not [ Close ])"
screen="$(read_pane)"
grep -Fq '[ X ]' <<<"$screen" \
  || fail "picker sheet did not show the [ X ] close affordance"
! grep -Fq '[ Close ]' <<<"$screen" \
  || fail "legacy [ Close ] button is still visible"
ok "[ X ] close affordance present"

step "Esc closes the picker outright (opened directly via 'b', nothing to back out to)"
e2e_herdr_mutate -- pane send-keys "$TUI_PANE" esc >/dev/null
wait_for 'Todo \(M\) · 1/1' || fail "board view did not return after Esc"
screen="$(read_pane)"
grep -Fq 'Compact Mobile Card' <<<"$screen" \
  || fail "board view lost its card after closing the picker"
ok "Esc closed the picker and returned to the Compact board view"

step "26-compact-mobile: ALL CHECKS PASSED"
