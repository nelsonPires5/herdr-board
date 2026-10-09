#!/usr/bin/env bash
# 38-board-project-archive.sh — one live open-run refusal trip.
#
# A real active run blocks `board archive` atomically: the refusal names the
# open run, the board stays active (archived_at IS NULL), and the pane/run are
# untouched (same open run row still unended, same live pane). After the run
# finishes, the same archive succeeds.
#
# Everything else lives hermetically and is NOT repeated here:
#   - archive policy (open-run / all-boards / Global / idempotent restore /
#     restore hints) in board-core engine archive.rs unit tests:
#     board_archive_allows_finished_runs_and_idempotent (:121),
#     board_archive_refuses_open_run (:129), project_archive_rules (:135),
#     archived_destinations_reject_new_work_with_restore_hints (:156);
#   - board visibility active|all|archived at the DB layer in board-core
#     tests/projects_contract.rs:269, project visibility in :306, and both
#     plus the human `(archived)` markers through the CLI in
#     board-cli tests/integration/projects.rs:282
#     (board_and_project_archive_visibility_and_human_markers);
#   - NOCASE name reservation while archived (UNIQUE still holds; restore
#     needs no rename) plus archived project path reservation in
#     board-core tests/projects_contract.rs:341;
#   - archived-destination rejections with restore hints at the RPC layer in
#     board-daemon ops tests/cards.rs:852 (card.create / card.move source +
#     destination / duplicate / run.retry / template.apply on an archived
#     board) and :950 (board.create / board.select / project.select against
#     an archived project);
#   - board selection fallback + restore-never-auto-selects in board-core
#     tests/projects_contract.rs:377; project all-boards rule + board open-run
#     wiring + deterministic beta fallback + project restore (boards stay
#     archived, no auto-select, explicit select works) in :423;
#   - archive round-trip preserves columns/cards/runs/comments in
#     board-core tests/projects_contract.rs:546; board + project archived_at
#     + exact beta fallback survive a file reopen (daemon-restart durability)
#     in :609.
# Deliberately NOT asserted live: a Global-archive attempt (the Global project
# has no archivable path; the guard is unit-tested as GlobalProject and live
# never attempts it), and a "no active project" fixture (Global/scope is
# always active, so a truly empty state cannot be established live — renamed
# away rather than faked).
#
# Provider-free: fake harness only, disposable hb-e2e-* session/workspaces,
# every Herdr MUTATION prefixed.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_boot

step "Create one project directory (open-run refusal isolation)"
P_A="$E2E_TMP/proj-alpha"
mkdir -p "$P_A"

step "project.create alpha (first board is main)"
A_CREATE="$($BOARD_BIN project create "$P_A" --json)"
A_PROJECT_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["project"]["id"])')"
A_MAIN_ID="$(printf '%s' "$A_CREATE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["board"]["id"])')"
ok "alpha project $A_PROJECT_ID main $A_MAIN_ID"

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
# Capture the open run + pane BEFORE the refusal, so untouched-ness is checkable.
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/guard-before.json"
read -r GUARD_RUN_ID GUARD_PANE <<<"$(python3 - "$E2E_TMP/guard-before.json" <<'PY'
import json, sys
show = json.load(open(sys.argv[1], encoding="utf-8"))
runs = show.get("runs", [])
if not runs:
    sys.exit("no run row yet (dispatcher did not enqueue)")
run = runs[-1]
if run.get("ended_at") is not None:
    sys.exit("run already ended before the refusal")
print(run["id"], run.get("herdr_pane_id") or "")
PY
)"
[ -n "$GUARD_RUN_ID" ] || fail "could not capture the open run id"
echo "  open run: $GUARD_RUN_ID pane: ${GUARD_PANE:-<queued, no pane yet>}"
# Immediate archive attempt must be refused atomically (open run).
set +e
ARCHIVE_ERR="$($BOARD_BIN board archive "$ARCHIVE_BOARD_ID" --json 2>&1)"
ARCHIVE_RC=$?
set -e
[ $ARCHIVE_RC -ne 0 ] || fail "board archive should have been refused with open run"
printf '%s' "$ARCHIVE_ERR" | grep -q "open run" || fail "expected 'open run' in board archive refusal, got: $ARCHIVE_ERR"
ok "board archive refused with open run: $ARCHIVE_ERR"
# Atomic: board still active (archived_at IS NULL), same run still open, same pane still live.
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/guard-after.json"
python3 - "$E2E_TMP/guard-after.json" "$GUARD_RUN_ID" <<'PY' || fail "refusal was not atomic — the run moved"
import json, sys
show = json.load(open(sys.argv[1], encoding="utf-8"))
want = int(sys.argv[2])
runs = show.get("runs", [])
if not (runs and runs[-1]["id"] == want):
    sys.exit("run row changed across refusal")
if runs[-1].get("ended_at") is not None:
    sys.exit("refused archive ended the run")
print(f"[ok] run {want} still open after refusal", file=sys.stderr)
PY
printf '%s' "$(brpc board.get "{\"board_id\":$ARCHIVE_BOARD_ID}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
if d["board"]["archived_at"] is not None:
    sys.exit("board archived_at changed on refusal")
' || fail "board archive was not atomic — archived_at changed on refusal"
ok "atomicity verified: archived_at still null"
if [ -n "$GUARD_PANE" ]; then
  hrpc pane.get "{\"pane_id\":\"$GUARD_PANE\"}" >/dev/null 2>&1 \
    || { e2e_card_failure_diag "$CARD_ID"; fail "refused archive killed the open run pane $GUARD_PANE"; }
  ok "open run pane $GUARD_PANE untouched by the refusal"
else
  echo "  (run still queued: no pane to check)"
fi
# Wait for fake run to finish, then archive must succeed.
wait_ok "$CARD_ID" >/dev/null || fail "fake run did not finish"
$BOARD_BIN board archive "$ARCHIVE_BOARD_ID" --json >/dev/null
printf '%s' "$(brpc board.get "{\"board_id\":$ARCHIVE_BOARD_ID}")" | python3 -c '
import json,sys
d=json.load(sys.stdin)
if d["board"]["archived_at"] is None:
    sys.exit("board archived_at still null after archive")
' || fail "board archive should succeed after run finished"
ok "board archive succeeded after run finished"

step "38-board-project-archive: ALL CHECKS PASSED"
