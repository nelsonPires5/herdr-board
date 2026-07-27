#!/usr/bin/env bash
# 24-cross-board-move.sh — a card can be moved to a column of another board.
#
# card.move with a board_id that differs from the card's board transfers the
# card atomically: cards.board_id/column_id are moved in one transaction and
# both the source and destination columns are recompacted. The daemon's
# cross-board sanity check resolves the card's Herdr session against the live
# ephemeral session, and a destination column that belongs to a different board
# than the declared board_id is rejected with nothing written.
#
# Destination columns are manual so the move is dispatch-free (no agent run is
# enqueued); the read-only workspace preflight for auto columns is covered by
# the board-daemon unit tests (validate_space_resolvable_*).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

# Board A is the daemon's initial board (E2E_BOARD_ID). Open a second board B.
B_OPEN="$(brpc board.open "$(python3 -c 'import json,sys; print(json.dumps({"scope_path":sys.argv[1]}))' "$E2E_TMP/board-b")")"
B_ID="$(printf '%s' "$B_OPEN" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["id"])')"

# Manual columns on both boards (dispatch-free destinations).
A_SRC="$(col_create '{"name":"A-Source","trigger":"manual"}')"
A_KEEP="$(col_create '{"name":"A-Keep","trigger":"manual"}')"
B_DST="$(brpc column.create "{\"board_id\":$B_ID,\"name\":\"B-Dest\",\"trigger\":\"manual\"}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

# Two cards in A's source column so we can observe source-column recompaction.
C1_ID="$(brpc card.create "{\"board_id\":$E2E_BOARD_ID,\"column_id\":$A_SRC,\"title\":\"cross-1\"}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
C2_ID="$(brpc card.create "{\"board_id\":$E2E_BOARD_ID,\"column_id\":$A_SRC,\"title\":\"cross-2\"}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

step "Cross-board transfer moves board_id/column_id and recompacts both columns"
moved="$(brpc card.move "{\"id\":$C1_ID,\"column_id\":$B_DST,\"board_id\":$B_ID}")"
printf '%s' "$moved" | python3 -c '
import json, sys
c = json.load(sys.stdin)
b_id, b_dst = int(sys.argv[1]), int(sys.argv[2])
assert c["board_id"] == b_id
assert c["column_id"] == b_dst
assert c["position"] == 0
' "$B_ID" "$B_DST"
ok "transferred card now on board B in B-Dest at position 0"

# Board A lost C1 and C2 is recompacted to position 0; board B gained C1.
brpc board.get "{\"board_id\":$E2E_BOARD_ID}" | python3 -c '
import json, sys
cards = {c["id"]: c for c in json.load(sys.stdin)["cards"]}
c1, c2 = int(sys.argv[1]), int(sys.argv[2])
assert c1 not in cards
assert cards[c2]["position"] == 0
' "$C1_ID" "$C2_ID"
brpc board.get "{\"board_id\":$B_ID}" | python3 -c '
import json, sys
cards = {c["id"] for c in json.load(sys.stdin)["cards"]}
assert int(sys.argv[1]) in cards
' "$C1_ID"
ok "source column recompacted; card present on destination board"

step "Mismatched destination board/column is rejected with nothing written"
# Declare board_id=B but a column that belongs to A -> must be rejected before
# any write (the cross-board sanity check guards board/column agreement).
err="$(brpc card.move "{\"id\":$C2_ID,\"column_id\":$A_KEEP,\"board_id\":$B_ID}" 2>/dev/null || true)"
printf '%s' "$err" | python3 -c '
import json, sys
assert "error" in json.load(sys.stdin)
'
ok "mismatched destination rejected"
# C2 must still be on board A, in the source column, untouched.
[ "$(card_field "$C2_ID" card.board_id)" = "$E2E_BOARD_ID" ] \
  || fail "C2 board changed after rejected move"
[ "$(card_field "$C2_ID" card.column_id)" = "$A_SRC" ] \
  || fail "C2 column changed after rejected move"
ok "rejected move left the card untouched"

step "24-cross-board-move: ALL CHECKS PASSED"
