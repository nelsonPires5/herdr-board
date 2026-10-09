#!/usr/bin/env bash
# 34-duplicate.sh — the TUI `C` shortcut duplicates the focused card live.
#
# One live trip: seed a source + follower in Todo (manual), press `C` on the
# focused source, assert the confirmation toast and the persisted order
# [Dupe Me, Dupe Me (copy), Follower]. Field-level copy correctness,
# positioning/compaction, original-untouched, and auto-column no-dispatch
# live ONLY here as final-state order — the matrices live hermetically:
# copy config/state/position in board-core db crud.rs (duplicate_card_*) and
# board-daemon ops cards.rs (card_duplicate_copies_config_*, card_duplicate_
# in_auto_column_never_enqueues, no dispatch wake), CLI --position compaction
# in board-cli integration cards.rs.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

e2e_ws_standard board-e2e   # step + e2e_ws_create + WS_ID + echo

# card_titles — pipe-separated titles of the Todo (default) column cards in
# persisted (position) order. The seed board's first column is always Todo.
card_titles() {
  brpc board.get "$(printf '{\"board_id\":%s}' "$E2E_BOARD_ID")" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); col=d["columns"][0]["id"]; print("|".join(c["title"] for c in d["cards"] if c["column_id"]==col))'
}

wait_titles() {
  local expected="$1" got="" i
  for (( i=0; i<100; i++ )); do
    got="$(card_titles 2>/dev/null || true)"
    [ "$got" = "$expected" ] && return 0
    sleep 0.1
  done
  fail "card order '$got' (expected '$expected')"
}

# ============================================================================
step "SEED"
# ----------------------------------------------------------------------------
step "Create the source card + a follower in Todo (manual); field matrices live hermetically"
SRC_ID="$("$BOARD_BIN" card new --title "Dupe Me" \
  -d "base prompt for the duplicate" --harness fake --model free-model \
  --session default --space-kind workspace --space-ref "$WS_ID" --json \
  | jget id)"
[ -n "$SRC_ID" ] || fail "could not parse source card id"
FOL_ID="$("$BOARD_BIN" card new --title "Follower" --json | jget id)"
wait_titles "Dupe Me|Follower"
ok "Todo order is [Dupe Me, Follower]"

# ============================================================================
step "TUI PATH (the one live duplication trip)"
# ----------------------------------------------------------------------------
step "HERDR MUTATION: open a tab in the workspace and launch 'board tui' in it"
tab_json="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label board-tui --no-focus)"
echo "  -> $tab_json"
PANE_ID="$(printf '%s' "$tab_json" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find pane for the TUI tab"
e2e_launch_tui "$PANE_ID" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH"

step "Wait for the TUI to render the focused card, then press C (duplicate)"
screen=""
for (( i=0; i<100; i++ )); do
  screen="$("$HERDR_BIN" pane read "$PANE_ID" --source recent-unwrapped --lines 200 2>/dev/null || true)"
  printf '%s\n' "$screen" | grep -Fq "Dupe Me" && break
  sleep 0.1
done
printf '%s\n' "$screen" | grep -Fq "Dupe Me" || fail "TUI did not render 'Dupe Me'"
# send-text delivers the literal capital-C byte (Shift+c) that crossterm reads
# as Char('C'); the focused card is Todo's first card (the source).
e2e_herdr_mutate -- pane send-text "$PANE_ID" C >/dev/null

step "The confirmation toast appears and the copy lands on the board"
toast=""
for (( i=0; i<100; i++ )); do
  toast="$("$HERDR_BIN" pane read "$PANE_ID" --source recent-unwrapped --lines 200 2>/dev/null || true)"
  printf '%s\n' "$toast" | grep -Fq "card duplicated as #" && break
  sleep 0.1
done
printf '%s\n' "$toast" | grep -Fq "card duplicated as #" \
  || fail "no 'card duplicated as #' toast in the TUI pane"
wait_titles "Dupe Me|Dupe Me (copy)|Follower"
ok "TUI C duplicated the focused card (toast + persisted order)"

step "34-duplicate: ALL CHECKS PASSED"
