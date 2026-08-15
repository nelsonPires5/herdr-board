#!/usr/bin/env bash
# 36-multi-project.sh — projects: creation, selection persistence, recency,
# per-project board isolation, and card movement across projects.
#
# A project is a named board collection identified by a canonical path; every
# project's first board is 'main'; Global is the special project. Creating a
# project requires an existing directory (never creates one), selects project
# + main, and updates recency. Only explicit open/create/select touch
# selection and recency — moving a card between projects must leave the
# selection exactly as it was. Selection and recency are durable (SQLite), so
# they survive a daemon restart.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

step "Create two project directories (existing dirs only; create never mkdirs)"
P_A="$E2E_TMP/proj-alpha"
P_B="$E2E_TMP/proj-beta"
mkdir -p "$P_A" "$P_B"

step "project.create selects the project and its first board 'main'"
A_CREATE="$($BOARD_BIN project create "$P_A" --json)"
A_PROJECT_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["project"]["name"]=="proj-alpha"; assert d["project"]["scope_path"].endswith("proj-alpha"), d["project"]["scope_path"]; assert d["board"]["board"]["name"]=="main"; print(d["project"]["id"])')"
ok "project alpha created, named after its folder, board 'main', selected"

# Creating a project for a nonexistent directory is refused.
step "project.create of a missing directory is refused"
err="$($BOARD_BIN project create "$E2E_TMP/proj-nope" --json 2>&1 >/dev/null || true)"
[ -n "$err" ] || fail "project create for a missing dir unexpectedly succeeded"
ok "missing directory refused"

step "A second project exists but the selection is still alpha (creation recency)"
B_CREATE="$($BOARD_BIN project create "$P_B" --json)"
B_PROJECT_ID="$(printf '%s' "$B_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["project"]["id"])')"
[ -n "$B_PROJECT_ID" ] || fail "project beta did not create"
SELECTED="$(brpc project.selected '{}')"
printf '%s' "$SELECTED" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["project"]["id"] == int(sys.argv[1]), "creation must select the newest project"
assert d["board"]["board"]["name"] == "main"
' "$B_PROJECT_ID"
ok "beta is now selected with its main board"

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

step "project.list serves picker-ready data: names, recency, Global last"
LIST="$(brpc project.list '{}')"
printf '%s' "$LIST" | python3 -c '
import json, sys
d = json.load(sys.stdin)
names = [p["project"]["name"] for p in d["projects"]]
# The harness daemon owns an isolated 'scope' project (e2e_isolate); the
# created projects sort before it and the special Global project is last.
assert names == ["proj-alpha", "proj-beta", "scope", "Global"], names
assert d["selected_project_id"] == int(sys.argv[1])
# The served recents exclude the selected project (the picker shows it
# separately); the harness scope project was opened with board.open, which is
# resolution-only and never touches recency.
assert d["recent_project_ids"] == [int(sys.argv[2])], d["recent_project_ids"]
# Every project has exactly one board for now: main. The per-project board
# selection is persisted only once the project was used (the harness scope
# project was only resolved with board.open, so its key is absent).
for p in d["projects"]:
    assert [b["name"] for b in p["boards"]] == ["main"], p
    selected = p.get("selected_board_id")
    if selected is not None:
        assert selected == p["boards"][0]["id"], p
' "$B_PROJECT_ID" "$A_PROJECT_ID"
ok "project.list deterministic: alphabetical, Global last, recency capped"

step "Boards in one project keep columns and cards isolated"
# Two boards in alpha: 'main' and 'Backlog'; each gets its own column.
BOARD_CREATE="$($BOARD_BIN board create Backlog --project "$P_A" --json)"
BACKLOG_ID="$(printf '%s' "$BOARD_CREATE" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["board"]["name"]=="Backlog"; print(d["board"]["id"])')"
ALPHA_MAIN_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
ALPHA_COL="$(brpc column.create "{\"board_id\":$ALPHA_MAIN_ID,\"name\":\"Alpha-Only\",\"trigger\":\"manual\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
BACKLOG_COL="$(brpc column.create "{\"board_id\":$BACKLOG_ID,\"name\":\"Backlog-Only\",\"trigger\":\"manual\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
CARD_IN_ALPHA="$(brpc card.create "{\"board_id\":$ALPHA_MAIN_ID,\"column_id\":$ALPHA_COL,\"title\":\"alpha card\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
# The Backlog board must not see alpha's column or card.
printf '%s' "$(brpc board.get "{\"board_id\":$BACKLOG_ID}")" | python3 -c '
import json, sys
d = json.load(sys.stdin)
cols = [c["name"] for c in d["columns"]]
assert "Alpha-Only" not in cols and "Backlog-Only" in cols, cols
assert d["cards"] == [], d["cards"]
'
ok "same-project boards have isolated columns and cards"

step "Creating a board auto-selects it (per-project board selection)"
LIST="$(brpc project.list '{}')"
printf '%s' "$LIST" | python3 -c '
import json, sys
by_name = {p["project"]["name"]: p for p in json.load(sys.stdin)["projects"]}
alpha = by_name["proj-alpha"]
assert alpha["selected_board_id"] == int(sys.argv[1]), "board create must auto-select the new board"
assert [b["name"] for b in alpha["boards"]] == ["Backlog", "main"]
' "$BACKLOG_ID"
ok "new board auto-selected and listed alphabetically"

step "card.move --to-project/--to-board transfers across projects without touching the selection"
# Re-select alpha (beta was selected by its creation), then move alpha's card
# to beta's main board.
$BOARD_BIN project select "$P_A" >/dev/null
LIST_BEFORE="$(brpc project.list '{}')"
BETA_MAIN_ID="$(printf '%s' "$B_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
# The column reference resolves in the DESTINATION board (beta main's Todo),
# exactly like --destination-board today; the card leaves alpha's Alpha-Only.
MOVED="$($BOARD_BIN card move "$CARD_IN_ALPHA" Todo --to-project "$P_B" --to-board main --json)"
printf '%s' "$MOVED" | python3 -c '
import json, sys
c = json.load(sys.stdin)
assert c["board_id"] == int(sys.argv[1]), "card must land on beta main"
assert c["column_id"] == int(sys.argv[2]), "column must be resolved in the destination board"
' "$BETA_MAIN_ID" "$(brpc board.get "{\"board_id\":$BETA_MAIN_ID}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["columns"][0]["id"])')"
# The card is gone from alpha's Alpha-Only column.
printf '%s' "$(brpc board.get "{\"board_id\":$ALPHA_MAIN_ID}")" | python3 -c '
import json, sys
cards = json.load(sys.stdin)["cards"]
assert all(c["column_id"] != int(sys.argv[1]) for c in cards), cards
' "$ALPHA_COL"
ok "cross-project move landed on beta main; selection still alpha"

step "Recency: only open/create/select touch it — the move did not"
LIST_AFTER="$(brpc project.list '{}')"
printf '%s' "$LIST_AFTER" | python3 -c '
import json, sys
d = json.load(sys.stdin)
assert d["selected_project_id"] == int(sys.argv[1])
# The served recents exclude the selected project; only alpha is left.
assert d["recent_project_ids"] == [int(sys.argv[2])], d["recent_project_ids"]
' "$A_PROJECT_ID" "$B_PROJECT_ID"
[ "$LIST_AFTER" = "$LIST_BEFORE" ] \
  || fail "project.list changed across the move: selection or recency was touched"
ok "recent projects unchanged by the move"

step "36-multi-project: ALL CHECKS PASSED"
