#!/usr/bin/env bash
# 34-duplicate.sh — `board card duplicate` (CLI) and `C` (TUI) create a fresh
# idle copy directly below the original.
#
# Asserts (all provider-free; the only dispatch is the auto-column control
# card running the fake harness):
#   - the copy's title gains the ` (copy)` suffix; description, harness, model,
#     effort, session, and space configuration are copied verbatim,
#   - the copy is born idle with no runs, no comments, no conversation id, and
#     no archive flag,
#   - the copy lands immediately below the original and the column is
#     recompacted (followers shift down), while the original row stays
#     byte-identical,
#   - duplicating a card in an AUTO column never enqueues a run,
#   - the TUI `C` shortcut (board surface) duplicates the focused card and
#     shows the confirmation toast.
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
step "CLI PATH"
# ----------------------------------------------------------------------------
step "Create a fully configured source card + a follower in Todo (manual)"
SRC_ID="$("$BOARD_BIN" card new --title "Dupe Me" \
  -d "base prompt for the duplicate" --harness fake --model free-model \
  --session default --space-kind workspace --space-ref "$WS_ID" --json \
  | jget id)"
[ -n "$SRC_ID" ] || fail "could not parse source card id"
FOL_ID="$("$BOARD_BIN" card new --title "Follower" --json | jget id)"
wait_titles "Dupe Me|Follower"
ok "Todo order is [Dupe Me, Follower]"

step "Duplicate the source card via the CLI"
copy_json="$("$BOARD_BIN" card duplicate "$SRC_ID" --json)"
COPY_ID="$(printf '%s' "$copy_json" | jget id)"
[ -n "$COPY_ID" ] && [ "$COPY_ID" != "$SRC_ID" ] \
  || fail "duplicate returned no distinct card id: $copy_json"
echo "  copy: $COPY_ID"

step "The copy carries the title suffix and the full configuration"
[ "$(card_field "$COPY_ID" card.title)" = "Dupe Me (copy)" ] \
  || fail "copy title is not 'Dupe Me (copy)'"
for field in description harness model effort session space_kind space_ref; do
  [ "$(card_field "$COPY_ID" "card.$field")" = "$(card_field "$SRC_ID" "card.$field")" ] \
    || fail "copy card.$field differs from the source"
done
ok "description/harness/model/effort/session/space copied verbatim"

step "The copy is born clean: idle, no runs, no comments, no conversation/archive"
[ "$(card_field "$COPY_ID" card.status)" = "idle" ] || fail "copy is not idle"
[ "$(card_field "$COPY_ID" card.archived_at 2>/dev/null || echo null)" = "null" ] \
  || fail "copy has an archive flag"
python3 - "$COPY_ID" "$BOARD_BIN" <<'PY' || fail "copy state is not clean"
import json, subprocess, sys
show = json.loads(subprocess.run(
    [sys.argv[2], "card", "show", sys.argv[1], "--json"],
    check=True, capture_output=True, text=True).stdout)
card = show["card"]
assert card["status"] == "idle"
assert card["awaiting_reason"] is None
assert card["session_id"] is None
assert card["archived_at"] is None
assert show["runs"] == []
assert show["comments"] == []
PY
ok "copy is idle with no runs/comments/session_id/archive"

step "The copy sits directly below the original; the follower shifted down"
wait_titles "Dupe Me|Dupe Me (copy)|Follower"
ok "Todo order is [Dupe Me, Dupe Me (copy), Follower]"

step "A second duplication of the same source inserts below it again"
"$BOARD_BIN" card duplicate "$SRC_ID" >/dev/null
wait_titles "Dupe Me|Dupe Me (copy)|Dupe Me (copy)|Follower"
ok "Todo order is [Dupe Me, (copy), (copy), Follower] — positions compacted"

step "The original card is untouched"
[ "$(card_field "$SRC_ID" card.title)" = "Dupe Me" ] || fail "source title changed"
[ "$(card_field "$SRC_ID" card.description)" = "base prompt for the duplicate" ] \
  || fail "source description changed"
[ "$(card_field "$SRC_ID" card.harness)" = "fake" ] || fail "source harness changed"
[ "$(card_field "$SRC_ID" card.status)" = "idle" ] || fail "source status changed"

# ============================================================================
step "AUTO-COLUMN PATH"
# ----------------------------------------------------------------------------
step "Create an auto column 'Execute' and a control card that really runs"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
echo "  Execute=$EXEC_ID"
# Creating directly in an auto column dispatches immediately, like a move.
AUTO_ID="$("$BOARD_BIN" card new --title "Auto Src" --column Execute \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json | jget id)"
oc="$(wait_ok "$AUTO_ID" 80)" || {
  e2e_card_failure_diag "$AUTO_ID"
  fail "auto control card outcome '$oc', expected ok"
}
echo "  control card ran with outcome: $oc"

step "Duplicate the auto-column card; the copy must never dispatch"
AUTO_COPY="$("$BOARD_BIN" card duplicate "$AUTO_ID" --json | jget id)"
[ -n "$AUTO_COPY" ] || fail "could not duplicate auto-column card"
# The copy is idle with no run row even after the dispatcher would have had
# ample time to pick it up; the source keeps exactly its one finished run.
sleep 2
python3 - "$AUTO_COPY" "$AUTO_ID" "$BOARD_BIN" <<'PY' || fail "auto-column duplicate dispatched"
import json, subprocess, sys
copy = json.loads(subprocess.run(
    [sys.argv[3], "card", "show", sys.argv[1], "--json"],
    check=True, capture_output=True, text=True).stdout)
src = json.loads(subprocess.run(
    [sys.argv[3], "card", "show", sys.argv[2], "--json"],
    check=True, capture_output=True, text=True).stdout)
assert copy["card"]["status"] == "idle", copy["card"]["status"]
assert copy["runs"] == [], f"copy has run rows: {copy['runs']}"
assert copy["card"]["session_id"] is None
assert len(src["runs"]) == 1, f"source run count changed: {len(src['runs'])}"
PY
ok "auto-column duplicate stayed idle with zero runs (no dispatch)"

# ============================================================================
step "TUI PATH"
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
wait_titles "Dupe Me|Dupe Me (copy)|Dupe Me (copy)|Dupe Me (copy)|Follower"
ok "TUI C duplicated the focused card (toast + persisted order)"

step "34-duplicate: ALL CHECKS PASSED"
