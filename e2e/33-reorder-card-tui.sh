#!/usr/bin/env bash
# 33-reorder-card-tui.sh — reordering a card within its own column (live TUI trip).
#
# Asserts (provider-free) as final-state checks against the persisted board:
#   - the `O` TUI mini-mode shows the "Reorder card" banner and `j` stages the
#     card one slot (selection follows; edges clamp),
#   - Enter commits the staged move and the persisted order flips (read back
#     from the isolated boardd via board.get),
#   - Esc after staging leaves the persisted order unchanged.
# Exact RPC counts (zero on Esc, one same-column card.move on Enter) are NOT
# proven here — only the final persisted order is read back. Counts belong to
# the reducer/driver hermetic tests (board-tui update/scope.rs: single-move
# and esc-emits-nothing cases).
# Positioning/no-redispatch matrices live hermetically and are NOT repeated
# here: CLI --position in board-cli integration cards.rs, same-column reorder
# + auto-column no-dispatch in board-daemon ops cards.rs (seeded 3-card order
# flips, never enqueues, never wakes dispatch), compaction in board-core db
# crud.rs. This scenario keeps one live O-reorder trip: three Todo cards
# (alpha/beta/gamma) prove a real position change, not a single-card no-op.
#
# The real TUI runs in a disposable Herdr pane; card order is read back
# straight from the isolated boardd (the post-Enter truth source), and the
# in-mode banner is read from the rendered pane.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

step "HERDR MUTATION: create disposable workspace for the reorder-card TUI"
e2e_ws_create reorder; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Seed three cards in the default Todo column (creation order = order)"
T1="$("$BOARD_BIN" card new --title "alpha" --harness fake --json | jget id)"
T2="$("$BOARD_BIN" card new --title "beta" --harness fake --json | jget id)"
T3="$("$BOARD_BIN" card new --title "gamma" --harness fake --json | jget id)"
echo "  Todo cards: $T1 $T2 $T3"

# card_names — space-joined card ids in persisted (position) order for a column.
card_names() {
  brpc board.get "$(printf '{\"board_id\":%s}' "$E2E_BOARD_ID")" \
    | python3 -c '
import json,sys
board=json.load(sys.stdin)
col=[c for c in board["columns"] if c["name"]=="Todo"][0]
print(" ".join(str(c["id"]) for c in sorted((c for c in board["cards"] if c["column_id"]==col["id"]), key=lambda c:(c["position"], c["id"]))))'
}

# wait_cards <expected-ids...> — poll board.get until the persisted card order
# matches (the post-Enter truth source, independent of the TUI's refetch).
wait_cards() {
  local expected="$*" got="" i
  for (( i=0; i<100; i++ )); do
    got="$(card_names 2>/dev/null || true)"
    [ "$got" = "$expected" ] && return 0
    sleep 0.1
  done
  fail "card order '$got' (expected '$expected')"
}

# wait_screen <pane> <substring> — poll the rendered pane until <substring> shows.
wait_screen() {
  local pane="$1" needle="$2" screen="" i
  for (( i=0; i<100; i++ )); do
    screen="$("$HERDR_BIN" pane read "$pane" --source recent-unwrapped --lines 200 2>/dev/null || true)"
    printf '%s\n' "$screen" | grep -Fqi "$needle" && return 0
    sleep 0.1
  done
  fail "pane did not render '$needle'"
}

step "Assert the seed order before driving the TUI"
wait_cards "$T1 $T2 $T3"
ok "initial Todo order is alpha beta gamma"

step "Launch the real TUI in a disposable pane against the isolated boardd"
TAB_JSON="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label reorder-card --no-focus)"
PANE_ID="$(printf '%s' "$TAB_JSON" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find pane for reorder-card tab"
e2e_launch_tui "$PANE_ID" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH"

step "Wait for the TUI to render its first card"
wait_screen "$PANE_ID" "alpha"
ok "real TUI is up"

step "HERDR MUTATION: enter the O reorder mini-mode and stage alpha one slot"
# Focus starts on Todo's first card (alpha). send-text delivers the literal
# capital-O byte; send-keys tokenizes named keys (enter/esc) instead.
e2e_herdr_mutate -- pane send-text "$PANE_ID" O >/dev/null
wait_screen "$PANE_ID" "Reorder card"
ok "O shows the 'Reorder card' banner"
e2e_herdr_mutate -- pane send-text "$PANE_ID" j >/dev/null
e2e_herdr_mutate -- pane send-keys "$PANE_ID" enter >/dev/null

step "Enter commits the staged move; persisted order flips (final-state check)"
wait_cards "$T2 $T1 $T3"
ok "committed Todo order is beta alpha gamma"

step "HERDR MUTATION: re-enter O, stage, then cancel with Esc"
# After the refetch the moved card (alpha) is still selected at index 1.
e2e_herdr_mutate -- pane send-text "$PANE_ID" O >/dev/null
wait_screen "$PANE_ID" "Reorder card"
e2e_herdr_mutate -- pane send-text "$PANE_ID" j >/dev/null
e2e_herdr_mutate -- pane send-keys "$PANE_ID" esc >/dev/null

step "Esc leaves the persisted order unchanged (final-state check)"
wait_cards "$T2 $T1 $T3"
ok "Esc cancelled: order unchanged (beta alpha gamma)"

step "33-reorder-card-tui: ALL CHECKS PASSED"
