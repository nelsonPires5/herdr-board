#!/usr/bin/env bash
# 01-core.sh — the original end-to-end smoke test, rebased onto lib.sh.
#
# Behavior-identical to the pre-split scripts/e2e.sh (suite now lives at e2e/):
#   CLI PATH  — create an auto column, create a card on the fake harness targeting
#               a disposable workspace, move it into the auto column (dispatches a
#               real herdr agent pane), poll until the run ends, assert outcome=ok
#               and a "fake:" comment.
#   TUI PATH  — open a tab, launch `board tui` in it, drive the new-card form via
#               send-keys, and assert the new card shows (pane + CLI).
#
# Runnable standalone (`bash e2e/01-core.sh`) or via run-all.sh.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

# ============================================================================
step "CLI PATH"
# ----------------------------------------------------------------------------
step "Create an auto column 'Execute' (raw protocol — no CLI verb for columns)"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
echo "  -> column $EXEC_ID on board $E2E_BOARD_ID"

step "Create a card on the fake harness targeting the workspace"
card_json="$("$BOARD_BIN" card new --title "E2E CLI Card" \
  -d "e2e cli card" --harness fake \
  --space-kind workspace --space-ref "$WS_ID" --json)"
echo "  -> $card_json"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"

step "Move card into 'Execute' (auto) — this dispatches a real herdr agent pane"
mut "board move $CARD_ID Execute -> daemon calls herdr agent.start in $WS_ID"
"$BOARD_BIN" move "$CARD_ID" Execute --json
echo "  moved; polling for the run to finish (fake harness sleeps then reports)..."

outcome="$(wait_ok "$CARD_ID")" || {
  echo "  run outcome: $outcome"
  echo "--- card state:"; "$BOARD_BIN" card show "$CARD_ID" --json || true
  echo "--- daemon log:"; tail -30 "$E2E_TMP/daemon.log" || true
  fail "expected run outcome 'ok', got '$outcome'"
}
echo "  run outcome: $outcome"

step "Assert a 'fake:' comment landed"
show="$("$BOARD_BIN" card show "$CARD_ID" --json)"
printf '%s' "$show" | grep -q "fake:" || fail "no 'fake:' comment on card $CARD_ID"
ok "card $CARD_ID ran the fake harness (outcome ok, 'fake:' comment present)"

# ============================================================================
step "TUI PATH"
# ----------------------------------------------------------------------------
step "HERDR MUTATION: open a tab in the workspace and launch 'board tui' in it"
mut "tab create --workspace $WS_ID --label board-tui --no-focus"
tab_json="$("$HERDR_BIN" tab create --workspace "$WS_ID" --label board-tui --no-focus)"
echo "  -> $tab_json"
TAB_ID="$(printf '%s' "$tab_json" | jget tab_id)" || fail "could not parse tab_id"
PANE_ID="$(printf '%s' "$tab_json" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find pane for tab $TAB_ID"
echo "  tui pane: $PANE_ID"

# Pane shells do NOT inherit workspace --env; pass the isolated env inline so the
# TUI talks to THIS test's daemon, not the default socket.
mut "pane run $PANE_ID '<board> tui' (isolated BOARD_SOCKET/BOARD_DB)"
"$HERDR_BIN" pane run "$PANE_ID" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH $BOARD_BIN tui"
echo "  waiting for the TUI to come up..."
sleep 3

step "Drive the new-card form via send-keys (n, type title, Enter)"
mut "pane send-keys $PANE_ID n"
"$HERDR_BIN" pane send-keys "$PANE_ID" n
sleep 0.5
mut "pane send-text $PANE_ID 'E2E TUI Card'"
"$HERDR_BIN" pane send-text "$PANE_ID" "E2E TUI Card"
sleep 0.5
mut "pane send-keys $PANE_ID Enter (submit)"
"$HERDR_BIN" pane send-keys "$PANE_ID" enter
sleep 2

step "Read the TUI pane and assert the new card appears"
screen="$("$HERDR_BIN" pane read "$PANE_ID" --source recent-unwrapped --lines 200 || true)"
printf '%s\n' "$screen" | grep -q "E2E TUI Card" \
  || fail "new card 'E2E TUI Card' not visible in the TUI pane"
ok "card created through the TUI is visible on the board"

# Confirm it also exists via the CLI (belt and suspenders).
"$BOARD_BIN" card list --json | grep -q "E2E TUI Card" \
  || fail "TUI-created card not found via CLI card list"

step "01-core: ALL CHECKS PASSED"
