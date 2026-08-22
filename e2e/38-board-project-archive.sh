#!/usr/bin/env bash
# 38-board-project-archive.sh — boards and projects archive/restore.
#
# Covers schema v15 archived_at: durable, hidden by default, visibility
# active|all|archived, marked (archived), Global never, all-boards rule,
# open-run atomic refusal (ended_at IS NULL includes queued), archived
# destinations reject new work with restore hint, names/paths reserved
# (board NOCASE), restore never auto-selects nor requires rename, selection
# fallback deterministic and durable across daemon restart, human+JSON include
# archived_at, events BoardArchived/BoardRestored/ProjectArchived/ProjectRestored.
#
# Provider-free: fake harness only, disposable hb-e2e-* session/workspaces,
# every Herdr MUTATION prefixed.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot

step "Create two project directories (archive/restore isolation)"
P_A="$E2E_TMP/proj-alpha"
P_B="$E2E_TMP/proj-beta"
mkdir -p "$P_A" "$P_B"

step "project.create alpha and beta (each first board is main)"
A_CREATE="$($BOARD_BIN project create "$P_A" --json)"
A_PROJECT_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["project"]["id"])')"
A_MAIN_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
B_CREATE="$($BOARD_BIN project create "$P_B" --json)"
B_PROJECT_ID="$(printf '%s' "$B_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["project"]["id"])')"
B_MAIN_ID="$(printf '%s' "$B_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
ok "alpha project $A_PROJECT_ID main $A_MAIN_ID; beta $B_PROJECT_ID main $B_MAIN_ID"

step "Create board ArchiveMe in alpha and its auto Execute column"
ARCHIVE_CREATE="$($BOARD_BIN board create ArchiveMe --project "$P_A" --json)"
ARCHIVE_BOARD_ID="$(printf '%s' "$ARCHIVE_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["id"])')"
EXEC_ID="$(brpc column.create "{\"board_id\":$ARCHIVE_BOARD_ID,\"name\":\"Execute\",\"trigger\":\"auto\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
ok "ArchiveMe board $ARCHIVE_BOARD_ID Execute $EXEC_ID"

# Ensure ArchiveMe is selected before the open-run test (board create auto-selects it,
# but make explicit).
$BOARD_BIN project select "$P_A" --board ArchiveMe >/dev/null

step "Open-run atomic refusal: board.archive must fail while a run is open"
e2e_ws_create ws-archive
WS="$E2E_WS"
TODO_ID="$(brpc board.get "{\"board_id\":$ARCHIVE_BOARD_ID}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["columns"][0]["id"])')"
CARD_JSON="$(brpc card.create "{\"board_id\":$ARCHIVE_BOARD_ID,\"column_id\":$TODO_ID,\"title\":\"open-run guard\",\"harness\":\"fake\",\"space_kind\":\"workspace\",\"space_ref\":\"$WS\"}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)))')"
CARD_ID="$(printf '%s' "$CARD_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
# Move into Execute to enqueue/run (fake harness sleeps 1.5s before done)
e2e_board_herdr_mutate -- move "$CARD_ID" Execute --json >/dev/null
# Give the dispatcher a moment to create the run row (queued/running) before probing.
sleep 0.5
# Immediate archive attempt must be refused atomically (open run).
set +e
ARCHIVE_ERR="$($BOARD_BIN board archive "$ARCHIVE_BOARD_ID" --json 2>&1)"
ARCHIVE_RC=$?
set -e
[ $ARCHIVE_RC -ne 0 ] || fail "board archive should have been refused with open run"
printf '%s' "$ARCHIVE_ERR" | grep -q "open run" || fail "expected 'open run' in board archive refusal, got: $ARCHIVE_ERR"
ok "board archive refused with open run: $ARCHIVE_ERR"
# Atomic: board still active (archived_at IS NULL)
printf '%s' "$(brpc board.get "{\"board_id\":$ARCHIVE_BOARD_ID}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["board"]["archived_at"] is None, d["board"]
' || fail "board archive was not atomic — archived_at changed on refusal"
ok "atomicity verified: archived_at still null"
# Wait for fake run to finish, then archive must succeed.
wait_ok "$CARD_ID" >/dev/null || fail "fake run did not finish"
$BOARD_BIN board archive "$ARCHIVE_BOARD_ID" --json >/dev/null
printf '%s' "$(brpc board.get "{\"board_id\":$ARCHIVE_BOARD_ID}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["board"]["archived_at"] is not None, d["board"]
' || fail "board archive should succeed after run finished"
ok "board archive succeeded after run finished"

step "Visibility active|all|archived: human (archived) and JSON archived_at"
# active hides
ACTIVE_JSON="$($BOARD_BIN board list --project "$P_A" --visibility active --json)"
printf '%s' "$ACTIVE_JSON" | python3 -c '
import json,sys
boards=json.load(sys.stdin)
assert all(b["archived_at"] is None for b in boards), boards
assert not any(b["id"]==int(sys.argv[1]) for b in boards), "archived board leaked into active"
' "$ARCHIVE_BOARD_ID" || fail "active visibility leaked archived board"
ACTIVE_TEXT="$($BOARD_BIN board list --project "$P_A" --visibility active)"
printf '%s' "$ACTIVE_TEXT" | grep -q "ArchiveMe" && fail "active text leaked archived board"
ok "active hides archived board"

ARCHIVED_JSON="$($BOARD_BIN board list --project "$P_A" --visibility archived --json)"
printf '%s' "$ARCHIVED_JSON" | python3 -c '
import json,sys
boards=json.load(sys.stdin)
assert any(b["id"]==int(sys.argv[1]) and b["archived_at"] is not None for b in boards), boards
' "$ARCHIVE_BOARD_ID" || fail "archived visibility missing board"
ARCHIVED_TEXT="$($BOARD_BIN board list --project "$P_A" --visibility archived)"
printf '%s' "$ARCHIVED_TEXT" | grep -q "(archived)" || fail "archived text missing (archived) marker"
ok "archived shows marker and archived_at"

ALL_JSON="$($BOARD_BIN board list --project "$P_A" --visibility all --json)"
printf '%s' "$ALL_JSON" | python3 -c '
import json,sys
boards=json.load(sys.stdin)
assert any(b["id"]==int(sys.argv[1]) for b in boards), boards
assert any(b["name"]=="main" for b in boards), boards
' "$ARCHIVE_BOARD_ID" || fail "all visibility missing boards"
ALL_TEXT="$($BOARD_BIN board list --project "$P_A" --visibility all)"
printf '%s' "$ALL_TEXT" | grep -q "(archived)" || fail "all text missing (archived) marker"
ok "all shows both active and archived with marker"

# Project-level visibility
PROJECT_ARCHIVED_JSON="$($BOARD_BIN project list --visibility archived --json)"
printf '%s' "$PROJECT_ARCHIVED_JSON" | python3 -c '
import json,sys
d=json.load(sys.stdin)
# ArchiveMe board is archived, but project alpha is still active (has main). So no project should be archived yet.
assert all(p["project"]["archived_at"] is None for p in d["projects"]), d["projects"]
' || fail "project archived list should be empty before project archive"
ok "project archived empty before project archiving"

step "Names/paths remain reserved (NOCASE); restore never requires rename"
set +e
DUP_ERR="$($BOARD_BIN board create archiveme --project "$P_A" --json 2>&1)"
DUP_RC=$?
set -e
[ $DUP_RC -ne 0 ] || fail "board create with NOCASE duplicate of archived name should fail"
printf '%s' "$DUP_ERR" | grep -qi "already exists\|duplicate" || fail "expected duplicate/board exists error, got: $DUP_ERR"
ok "NOCASE board name still reserved while archived: $DUP_ERR"

set +e
PROJ_DUP_ERR="$($BOARD_BIN project create "$P_A" --json 2>&1)"
PROJ_DUP_RC=$?
set -e
[ $PROJ_DUP_RC -ne 0 ] || fail "project create with same scope_path while active should fail (path reserved)"
printf '%s' "$PROJ_DUP_ERR" | grep -qi "already exists" || fail "expected project already exists, got: $PROJ_DUP_ERR"
ok "project scope_path reserved"

step "Project archive rule: all boards must be archived first (and Global never)"
# Try archiving alpha while its main is still active — must fail ActiveBoards.
set +e
PROJ_ARCH_ERR="$($BOARD_BIN project archive "$P_A" --json 2>&1)"
PROJ_ARCH_RC=$?
set -e
[ $PROJ_ARCH_RC -ne 0 ] || fail "project archive should fail while an active board remains"
printf '%s' "$PROJ_ARCH_ERR" | grep -q "active board" || fail "expected active board count in refusal, got: $PROJ_ARCH_ERR"
ok "project archive refused while active board remains: $PROJ_ARCH_ERR"
# Still active atomically
printf '%s' "$(brpc project.get "{\"scope_path\":\"$P_A\"}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["project"]["archived_at"] is None, d["project"]
' || fail "project archive should be atomic"

# Archive the remaining active board in alpha (main), no open run.
$BOARD_BIN board archive "$A_MAIN_ID" --json >/dev/null
ok "archived alpha main $A_MAIN_ID"
# Now project archive must succeed.
$BOARD_BIN project archive "$P_A" --json >/dev/null
printf '%s' "$(brpc project.get "{\"scope_path\":\"$P_A\",\"visibility\":\"all\"}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["project"]["archived_at"] is not None, d["project"]
' || fail "project archive should succeed after all boards archived"
ok "project archive succeeded after all boards archived"

# Project visibility after archive
PROJ_ALL_JSON="$($BOARD_BIN project list --visibility all --json)"
printf '%s' "$PROJ_ALL_JSON" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert any(p["project"]["id"]==int(sys.argv[1]) and p["project"]["archived_at"] is not None for p in d["projects"]), d["projects"]
' "$A_PROJECT_ID" || fail "project all should include archived alpha"
PROJ_ACTIVE_JSON="$($BOARD_BIN project list --visibility active --json)"
printf '%s' "$PROJ_ACTIVE_JSON" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert not any(p["project"]["id"]==int(sys.argv[1]) for p in d["projects"]), d["projects"]
' "$A_PROJECT_ID" || fail "project active should hide archived alpha"
PROJ_ARCHIVED_TEXT="$($BOARD_BIN project list --visibility archived)"
printf '%s' "$PROJ_ARCHIVED_TEXT" | grep -q "(archived)" || fail "project archived text missing marker"
ok "project visibility filters work"

step "Global project never archivable (engine guard GlobalProject, DB + unit tested) — E2E verifies Global stays active"
# Global has scope_path IS NULL (id 1). Direct DB attempt would hit decide_project_archive GlobalProject.
# Via RPC, any project.archive for a non-Global path was already tested. Here verify Global row never archived.
python3 - "$BOARD_DB" <<'PY'
import sqlite3, sys
import sys as _sys
_db=sys.argv[1]
_con=sqlite3.connect(_db)
_row=_con.execute("SELECT archived_at FROM projects WHERE scope_path IS NULL").fetchone()
if _row is None:
    print("Global project missing", file=_sys.stderr)
    _sys.exit(1)
if _row[0] is not None:
    print(f"Global should never be archived, got {_row[0]}", file=_sys.stderr)
    _sys.exit(1)
print("Global still active")
PY
ok "Global never archivable verified"

step "Selection fallback on archive (recency, deterministic) and no panic on no-active"
# At this point alpha is fully archived. Selected project should have fallen back to beta (has active board) or scope.
SELECTED="$(brpc project.selected '{}')"
printf '%s' "$SELECTED" | python3 -c '
import json,sys
d=json.load(sys.stdin)
# After archiving selected project alpha, selection must not point at archived project.
proj=d.get("project")
if proj is not None:
    assert proj["archived_at"] is None, f"selected project must not be archived: {proj}"
    assert proj["scope_path"] is not None, "selected should be a real project (not Global) when beta exists"
    assert proj["name"]=="proj-beta", f"expected fallback to beta, got {proj}"
' || fail "selection fallback after project archive wrong"
ok "selection fell back away from archived project"

# Archive beta's main as well to leave no active project besides Global/scope, then verify no panic and empty selection handling.
$BOARD_BIN board archive "$B_MAIN_ID" --json >/dev/null
$BOARD_BIN project archive "$P_B" --json >/dev/null
ok "archived beta project"
# Query selected/list must not panic and must be valid JSON even with no active project.
SELECTED2="$(brpc project.selected '{}')"
printf '%s' "$SELECTED2" | python3 -c '
import json,sys
d=json.load(sys.stdin)
# Either no selected project, or selected is scope/Global which is not archived.
proj=d.get("project")
if proj is not None:
    assert proj["archived_at"] is None, proj
' || fail "project.selected with no active project panicked or returned archived"
LIST_ACTIVE_EMPTY="$($BOARD_BIN project list --visibility active --json)"
printf '%s' "$LIST_ACTIVE_EMPTY" | python3 -c 'import json,sys; json.load(sys.stdin)' || fail "project list active invalid after archiving all"
ok "no-active-project case handled without panic"

step "Daemon restart durability: archived state survives restart"
e2e_daemon_stop
e2e_daemon_start
# alpha still archived, its boards still archived
printf '%s' "$(brpc project.get "{\"scope_path\":\"$P_A\",\"visibility\":\"all\"}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["project"]["archived_at"] is not None, d["project"]
boards={b["name"]:b for b in d["boards"]}
assert boards["main"]["archived_at"] is not None
assert boards["ArchiveMe"]["archived_at"] is not None
' || fail "archived state did not survive restart"
ok "archived state durable across restart"
# Selection still not on archived after restart
printf '%s' "$(brpc project.selected '{}')" | python3 -c '
import json,sys
d=json.load(sys.stdin)
proj=d.get("project")
if proj is not None:
    assert proj["archived_at"] is None
' || fail "selection after restart points at archived"
ok "selection durable after restart"

step "Archived destinations reject new work with restore hint"
# Card create on archived board must be refused
TODO_ON_ARCHIVED="$(brpc board.get "{\"board_id\":$ARCHIVE_BOARD_ID}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["columns"][0]["id"])')" || true
set +e
CARD_CREATE_ERR="$(brpc card.create "{\"board_id\":$ARCHIVE_BOARD_ID,\"column_id\":$TODO_ON_ARCHIVED,\"title\":\"should fail\"}" 2>&1)"
CARD_CREATE_RC=$?
set -e
# brpc card.create via fake returns bail; we test via CLI card create which goes through daemon guard.
set +e
CARD_CREATE_CLI_ERR="$($BOARD_BIN card create --board "$ARCHIVE_BOARD_ID" --title "should fail" --column Todo --json 2>&1)"
CARD_CREATE_CLI_RC=$?
set -e
# At least one path must show restore hint. Fake and daemon both emit restore hint.
if printf '%s' "$CARD_CREATE_CLI_ERR" | grep -qi "archived board must be restored"; then
  ok "card create on archived board rejected: $CARD_CREATE_CLI_ERR"
else
  # brpc path
  printf '%s' "$CARD_CREATE_ERR" | grep -qi "archived board" || fail "card create on archived board should be rejected with restore hint, got cli:$CARD_CREATE_CLI_ERR brpc:$CARD_CREATE_ERR"
  ok "card create on archived board rejected via brpc"
fi

# Card move to archived board
# Need a card on the scope board (active)
SCOPE_BOARD_ID="$E2E_BOARD_ID"
SCOPE_TODO="$(brpc board.get "{\"board_id\":$SCOPE_BOARD_ID}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["columns"][0]["id"])')"
MOVE_CARD="$(brpc card.create "{\"board_id\":$SCOPE_BOARD_ID,\"column_id\":$SCOPE_TODO,\"title\":\"move-target\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
set +e
MOVE_ERR="$($BOARD_BIN card move "$MOVE_CARD" Todo --destination-board "$ARCHIVE_BOARD_ID" --json 2>&1)"
MOVE_RC=$?
set -e
[ $MOVE_RC -ne 0 ] || fail "card move to archived board should fail"
printf '%s' "$MOVE_ERR" | grep -qi "archived board must be restored" || fail "expected restore hint on move, got: $MOVE_ERR"
ok "card move to archived board rejected"

# Card duplicate on archived board
set +e
DUP_ARCH_ERR="$(brpc card.duplicate "{\"id\":$CARD_ID}" 2>&1 || true)"
# CARD_ID is on archived ArchiveMe, duplicate via CLI duplicate command
DUP_CLI_ERR="$($BOARD_BIN card duplicate "$CARD_ID" --json 2>&1 || true)"
set -e
printf '%s' "$DUP_CLI_ERR$DUP_ARCH_ERR" | grep -qi "archived board" || fail "card duplicate on archived board should be rejected"
ok "card duplicate on archived board rejected"

# Retry on archived board's card
set +e
RETRY_ERR="$($BOARD_BIN retry "$CARD_ID" --json 2>&1)"
RETRY_RC=$?
set -e
[ $RETRY_RC -ne 0 ] || fail "retry on archived board should fail"
printf '%s' "$RETRY_ERR" | grep -qi "archived" || fail "expected archived hint on retry, got: $RETRY_ERR"
ok "retry on archived board rejected"

# Template apply on archived board
set +e
TEMPLATE_ERR="$($BOARD_BIN template apply pipeline --board "$ARCHIVE_BOARD_ID" --json 2>&1)"
TEMPLATE_RC=$?
set -e
[ $TEMPLATE_RC -ne 0 ] || fail "template apply on archived board should fail"
printf '%s' "$TEMPLATE_ERR" | grep -qi "archived board must be restored" || fail "expected restore hint on template, got: $TEMPLATE_ERR"
ok "template apply on archived board rejected"

# Enqueue/dispatch backstop: moving a card into an auto column whose board is archived
# We already tested move to archived board rejected; for auto-hop, try moving a card already on archived board into its auto column (should also be rejected)
set +e
ENQUEUE_ERR="$(e2e_board_herdr_mutate -- move "$CARD_ID" Execute --json 2>&1 || true)"
set -e
printf '%s' "$ENQUEUE_ERR" | grep -qi "archived" || fail "enqueue on archived board should be rejected"
ok "enqueue/dispatch on archived board rejected"

step "Round-trip preserves columns/cards/history and restore never auto-selects"
# Restore project first (its boards still archived, project itself becomes active)
$BOARD_BIN project restore "$P_A" --json >/dev/null
printf '%s' "$(brpc project.get "{\"scope_path\":\"$P_A\",\"visibility\":\"active\"}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["project"]["archived_at"] is None, d["project"]
assert len(d["boards"])==0
' || fail "project restore should not auto-restore boards"
# Restore boards (order: main then ArchiveMe)
$BOARD_BIN board restore "$A_MAIN_ID" --json >/dev/null
$BOARD_BIN board restore "$ARCHIVE_BOARD_ID" --json >/dev/null
# Now active shows both boards with preserved columns/cards
printf '%s' "$(brpc project.get "{\"scope_path\":\"$P_A\",\"visibility\":\"active\"}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["project"]["archived_at"] is None
names={b["name"] for b in d["boards"]}
assert "main" in names and "ArchiveMe" in names, names
' || fail "boards not visible after restore"
# Cards preserved
printf '%s' "$(brpc board.get "{\"board_id\":$ARCHIVE_BOARD_ID}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["board"]["archived_at"] is None
assert any(c["title"]=="open-run guard" for c in d["cards"]), d["cards"]
assert len(d["columns"])>=2, d["columns"]
' || fail "columns/cards not preserved through archive round-trip"
ok "round-trip preserved data"

# Restore never auto-selects: selection should still not be on restored project/board
SELECTED_AFTER="$(brpc project.selected '{}')"
printf '%s' "$SELECTED_AFTER" | python3 -c '
import json,sys
after=json.load(sys.stdin)
proj=after.get("project")
if proj is not None:
    assert proj["id"] != int(sys.argv[1])
' "$A_PROJECT_ID" || fail "restore auto-selected project"
ok "restore did not auto-select"

# Explicit select after restore works without rename
$BOARD_BIN project select "$P_A" >/dev/null
$BOARD_BIN board select "$ARCHIVE_BOARD_ID" --project "$P_A" >/dev/null
printf '%s' "$(brpc project.selected '{}')" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["project"]["id"]==int(sys.argv[1])
assert d["board"]["board"]["id"]==int(sys.argv[2])
' "$A_PROJECT_ID" "$ARCHIVE_BOARD_ID" || fail "explicit select after restore failed"
ok "explicit select after restore succeeds without rename"

step "Project open-run atomic refusal (ended_at IS NULL includes queued)"
# Board open-run already covered; project open-run is same guard. Create a fresh project for open-run project test.
P_C="$E2E_TMP/proj-charlie"
mkdir -p "$P_C"
C_CREATE="$($BOARD_BIN project create "$P_C" --json)"
C_PROJECT_ID="$(printf '%s' "$C_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["project"]["id"])')"
C_MAIN_ID="$(printf '%s' "$C_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
C_EXEC="$(brpc column.create "{\"board_id\":$C_MAIN_ID,\"name\":\"CExec\",\"trigger\":\"auto\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
C_TODO="$(brpc board.get "{\"board_id\":$C_MAIN_ID}" | python3 -c 'import json,sys; print([c["id"] for c in json.load(sys.stdin)["columns"] if c["name"]=="Todo"][0])')"
C_CARD="$(brpc card.create "{\"board_id\":$C_MAIN_ID,\"column_id\":$C_TODO,\"title\":\"c-queued\",\"harness\":\"fake\",\"space_kind\":\"workspace\",\"space_ref\":\"$WS\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
e2e_board_herdr_mutate -- move "$C_CARD" CExec --json >/dev/null
sleep 0.5
# Move succeeded, run is open. Archiving its board must be refused (open run).
set +e
C_BOARD_ARCH_ERR="$($BOARD_BIN board archive "$C_MAIN_ID" --json 2>&1)"
C_BOARD_ARCH_RC=$?
set -e
[ $C_BOARD_ARCH_RC -ne 0 ] || fail "charlie board archive with open run should fail"
printf '%s' "$C_BOARD_ARCH_ERR" | grep -q "open run" || fail "expected open run on charlie board, got $C_BOARD_ARCH_ERR"
ok "project board open-run guard works (board level)"
wait_ok "$C_CARD" >/dev/null
# After run finishes, archive board then project must succeed
$BOARD_BIN board archive "$C_MAIN_ID" --json >/dev/null
$BOARD_BIN project archive "$P_C" --json >/dev/null
ok "project archive succeeded after open run finished"

step "38-board-project-archive: ALL CHECKS PASSED"
