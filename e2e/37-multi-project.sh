#!/usr/bin/env bash
# 37-multi-project.sh — LIVE SMOKE for multi-project selection durability.
#
# The matrices used to live here; they now live hermetically (no live infra):
# - creation selects project+main, per-project board selection, recency capped
#   at three, open/create/select side-effect rules, deterministic picker-ready
#   listing (alphabetical, Global last): board-core projects_contract tests.
# - file-reopen durability of selection+recency: projects_contract
#   `selection_and_recency_survive_a_file_reopen`.
# - missing-directory refusal, board auto-select, per-project board listing,
#   cross-project move keeping selection AND recency: board-cli projects tests.
# - per-board column/card isolation: board-core db crud/scoped tests.
#
# What stays live: a real daemon restart keeps serving the newest selection,
# and a real CLI cross-project move lands the card without touching the
# persisted selection or recency.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

step "Create two project directories (existing dirs only; create never mkdirs)"
P_A="$E2E_TMP/proj-alpha"
P_B="$E2E_TMP/proj-beta"
mkdir -p "$P_A" "$P_B"

step "project.create selects the newest project and its first board 'main'"
A_CREATE="$($BOARD_BIN project create "$P_A" --json)"
A_PROJECT_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["project"]["name"]=="proj-alpha"; assert d["board"]["board"]["name"]=="main"; print(d["project"]["id"])')"
B_CREATE="$($BOARD_BIN project create "$P_B" --json)"
B_PROJECT_ID="$(printf '%s' "$B_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["project"]["id"])')"
SELECTED="$(brpc project.selected '{}')"
printf '%s' "$SELECTED" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["project"]["id"] == int(sys.argv[1]), "creation must select the newest project (beta)"
assert d["board"]["board"]["name"] == "main"
' "$B_PROJECT_ID"
ok "beta selected with its main board"

step "Selection and recency persist across a daemon restart"
e2e_daemon_stop
e2e_daemon_start
SELECTED="$(brpc project.selected '{}')"
printf '%s' "$SELECTED" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["project"]["id"] == int(sys.argv[1]), "selection must survive a daemon restart"
' "$B_PROJECT_ID"
ok "selected project survives the restart"

step "card.move --to-project/--to-board transfers across projects without touching selection or recency"
$BOARD_BIN project select "$P_A" >/dev/null
ALPHA_MAIN_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
ALPHA_COL="$(brpc column.create "{\"board_id\":$ALPHA_MAIN_ID,\"name\":\"Alpha-Only\",\"trigger\":\"manual\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
CARD_IN_ALPHA="$(brpc card.create "{\"board_id\":$ALPHA_MAIN_ID,\"column_id\":$ALPHA_COL,\"title\":\"alpha card\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
LIST_BEFORE="$(brpc project.list '{}')"
BETA_MAIN_ID="$(printf '%s' "$B_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
MOVED="$($BOARD_BIN card move "$CARD_IN_ALPHA" Todo --to-project "$P_B" --to-board main --json)"
printf '%s' "$MOVED" | python3 -c '
import json, sys
c = json.load(sys.stdin)
assert c["board_id"] == int(sys.argv[1]), "card must land on beta main"
' "$BETA_MAIN_ID"
ok "cross-project move landed on beta main"
LIST_AFTER="$(brpc project.list '{}')"
printf '%s' "$LIST_AFTER" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["selected_project_id"] == int(sys.argv[1]), "selection must still be alpha"
assert d["recent_project_ids"] == [int(sys.argv[2])], d["recent_project_ids"]
' "$A_PROJECT_ID" "$B_PROJECT_ID"
[ "$LIST_AFTER" = "$LIST_BEFORE" ] \
  || fail "project.list changed across the move: selection or recency was touched"
ok "selection and recency unchanged by the move"

step "37-multi-project: ALL CHECKS PASSED"
