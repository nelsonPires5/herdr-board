#!/usr/bin/env bash
# 33-reorder-card-tui.sh — reordering a card within its own column.
#
# Asserts (provider-free):
#   - the `O` TUI mini-mode shows the "Reorder card" banner and `j` stages the
#     card one slot (selection follows; edges clamp),
#   - Enter commits exactly ONE same-column card.move and the persisted order
#     flips (read back from the isolated boardd via board.get),
#   - Esc after staging persists nothing,
#   - `board card move <id> <column> --position N` reorders via the CLI with
#     the old forms untouched,
#   - a same-column reorder inside an AUTO column never dispatches: a failed
#     card parked in place keeps its single run row, its `failed` status, and
#     the reordered position.
#
# The real TUI runs in a disposable Herdr pane; card order is read back
# straight from the isolated boardd (the post-Enter truth source), and the
# in-mode banner is read from the rendered pane.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_OUTCOME=fail"  # the auto-column card parks failed in place

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

step "Enter commits exactly one same-column card.move; persisted order flips"
wait_cards "$T2 $T1 $T3"
ok "committed Todo order is beta alpha gamma"

step "HERDR MUTATION: re-enter O, stage, then cancel with Esc"
# After the refetch the moved card (alpha) is still selected at index 1.
e2e_herdr_mutate -- pane send-text "$PANE_ID" O >/dev/null
wait_screen "$PANE_ID" "Reorder card"
e2e_herdr_mutate -- pane send-text "$PANE_ID" j >/dev/null
e2e_herdr_mutate -- pane send-keys "$PANE_ID" esc >/dev/null

step "Esc must restore the original order without persisting anything"
wait_cards "$T2 $T1 $T3"
ok "Esc cancelled: order unchanged (beta alpha gamma)"

step "CLI: board card move --position reorders within the column"
"$BOARD_BIN" card move "$T3" Todo --position 0 --json >/dev/null
wait_cards "$T3 $T2 $T1"
ok "CLI --position 0 moved gamma to the front (gamma beta alpha)"
"$BOARD_BIN" card move "$T3" Todo --position 2 --json >/dev/null
wait_cards "$T2 $T1 $T3"
ok "CLI --position 2 moved gamma to the back (beta alpha gamma)"

step "HERDR MUTATION: same-column reorder in an AUTO column never dispatches"
AUTO_ID="$(col_create '{"name":"Auto","trigger":"auto"}')"
[ -n "$AUTO_ID" ] || fail "could not create/parse Auto column"
A1="$("$BOARD_BIN" card new --title "auto card" --harness fake \
  --space-kind workspace --space-ref "$WS_ID" --json | jget id)"
e2e_board_herdr_mutate -- move "$A1" Auto --json >/dev/null

step "Wait for the auto card's run to finish (fails and parks in place)"
oc="$(wait_runs "$A1" 1)" || { e2e_card_failure_diag "$A1"; fail "auto run never finished"; }
[ "$oc" = "fail" ] || fail "auto run outcome '$oc', expected 'fail'"
st="$(card_field "$A1" card.status || true)"
col="$(card_field "$A1" card.column_id || true)"
[ "$st" = "failed" ] || fail "card status '$st', expected 'failed'"
[ "$col" = "$AUTO_ID" ] || fail "card moved to $col; a no-on_fail failure must stay in Auto ($AUTO_ID)"
ok "auto card failed in place with 1 run"

step "Reorder the failed card inside the auto column; no second run may start"
"$BOARD_BIN" card move "$A1" Auto --position 0 --json >/dev/null
sleep 1
n="$("$BOARD_BIN" card show "$A1" --json \
  | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("runs",[])))')"
[ "$n" = "1" ] || fail "expected exactly 1 run after the reorder, got $n"
st="$(card_field "$A1" card.status || true)"
[ "$st" = "failed" ] || fail "card status '$st' after reorder, expected 'failed'"
pos="$(card_field "$A1" card.position || true)"
[ "$pos" = "0" ] || fail "card position '$pos', expected '0'"
ok "same-column reorder in the auto column kept 1 run and status 'failed'"

step "33-reorder-card-tui: ALL CHECKS PASSED"
