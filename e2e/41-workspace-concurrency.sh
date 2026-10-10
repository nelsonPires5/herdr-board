#!/usr/bin/env bash
# 41-workspace-concurrency.sh — overlapping OPEN managed runs in one workspace,
# global FIFO/cap, exact per-task placement, and promoted-run restart recovery.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_enable_fake_pi
# Hold BEFORE board done, unlike the post-completion hold in scenario 02.
# The checked-in shim reports working after receiving the exact task prompt.
export FAKE_PI_SLEEP=300
e2e_init
e2e_build
e2e_isolate
python3 - "$HERDR_BOARD_CONFIG" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
p.write_text("max_concurrent = 2\nidle_grace_seconds = 300\n" + p.read_text())
PY
e2e_daemon_start
e2e_ws_create concurrent-workers; SHARED_WS="$E2E_WS"
e2e_ws_create later-worker; OTHER_WS="$E2E_WS"
mkdir -p "$E2E_TMP/task-a" "$E2E_TMP/task-b" "$E2E_TMP/task-c"
EXEC_ID="$(col_create '{"name":"Parallel","trigger":"auto","timeout_minutes":10}')"

new_card() {
  "$BOARD_BIN" card new --title "$1" --description 'parallel placement contract' \
    --harness pi --model p17/pi-model --effort low \
    --space-kind workspace --space-ref "$2" --space-cwd "$3" --json | jget id
}
A="$(new_card first "$SHARED_WS" "$E2E_TMP/task-a")"
B="$(new_card second "$SHARED_WS" "$E2E_TMP/task-b")"
C="$(new_card third "$OTHER_WS" "$E2E_TMP/task-c")"
for card in "$A" "$B" "$C"; do
  e2e_board_herdr_mutate -- move "$card" Parallel --json >/dev/null
done

wait_parallel() {
  local i
  for i in $(seq 1 150); do
    if python3 - "$BOARD_DB" "$A" "$B" "$C" "$E2E_TMP" <<'PY'
import json, sqlite3, sys
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
rows = db.execute("""SELECT c.id,c.status,r.started_at,r.ended_at,r.herdr_pane_id
    FROM cards c JOIN runs r ON r.card_id=c.id ORDER BY r.id""").fetchall()
a,b,c = map(int, sys.argv[2:5])
good = (len(rows) == 3 and [r[0] for r in rows] == [a,b,c]
        and all(r[1] == 'running' and r[2] and r[3] is None and r[4] for r in rows[:2])
        and rows[2][1] == 'queued' and rows[2][2] is None and rows[2][3] is None)
if good:
    # Promotion follows prompt delivery, but the fixture records its tty read
    # asynchronously. Wait for that observation too before inspecting it.
    for card in (a,b):
        run_id = db.execute('SELECT id FROM runs WHERE card_id=?', (card,)).fetchone()[0]
        try:
            record = json.load(open(f'{sys.argv[5]}/fake-pi-run-{run_id}.json'))
            good = good and record.get('prompt_received_via_stdin', False)
        except (OSError, ValueError):
            good = False
raise SystemExit(0 if good else 1)
PY
    then return 0; fi
    sleep .1
  done
  for card in "$A" "$B" "$C"; do e2e_card_failure_diag "$card"; done
  fail 'expected first+second OPEN/running and third queued at the global cap'
}

step "Both same-workspace workers are promoted and open; the later workspace waits"
wait_parallel
PANES="$(hrpc pane.list "{\"workspace_id\":\"$SHARED_WS\"}")"
EVIDENCE="${E2E_SCENARIO_ARTIFACT_DIR:-$E2E_TMP}/concurrency.json"
python3 - "$BOARD_DB" "$PANES" "$A" "$B" "$SHARED_WS" "$E2E_TMP" "$BOARD_SOCKET" \
  "$HERDR_SOCKET_PATH" "$EVIDENCE" <<'PY'
import json, os, sqlite3, sys, time
db_path, panes_json, a, b, workspace, root, board_socket, herdr_socket, out = sys.argv[1:]
db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
db.row_factory = sqlite3.Row
panes = json.loads(panes_json)['panes']
proof = []
for card, task in [(int(a), 'task-a'), (int(b), 'task-b')]:
    run = dict(db.execute('SELECT * FROM runs WHERE card_id=?', (card,)).fetchone())
    assert run['started_at'] and run['ended_at'] is None
    assert run['timeout_deadline_at_ms'] is not None
    assert run['herdr_workspace_id'] == workspace
    pane = next(p for p in panes if p['pane_id'] == run['herdr_pane_id'])
    assert pane['agent'] == 'pi'
    assert os.path.realpath(pane['cwd']) == os.path.realpath(f'{root}/{task}')
    assert run['herdr_anchor_pane_id'] is None
    assert len([p for p in panes if p['tab_id'] == pane['tab_id']]) == 1
    record = json.load(open(f"{root}/fake-pi-run-{run['id']}.json"))
    assert record['card_id'] == card and record['run_id'] == run['id']
    assert record['board_socket'] == board_socket and record['herdr_socket'] == herdr_socket
    assert record['cwd'] == f'{root}/{task}'
    assert record['prompt_received_via_stdin'] and record['prompt_matches_run_snapshot']
    proof.append({k:run[k] for k in ('id','card_id','started_at','ended_at',
        'herdr_workspace_id','herdr_pane_id','session_id','timeout_deadline_at_ms')}
        | {'tab_id':pane['tab_id'], 'cwd':record['cwd']})
assert len({r['herdr_pane_id'] for r in proof}) == 2
assert len({r['tab_id'] for r in proof}) == 2
json.dump({'observed_open_at_ns':time.time_ns(), 'runs':proof}, open(out,'w'), indent=2)
print('  two simultaneous OPEN runs, distinct exact tabs/panes, cwd and callback identities verified')
PY

step "Restart boardd with both promoted workers open: adopt without relaunch"
e2e_daemon_stop
e2e_daemon_start
wait_parallel
PANES="$(hrpc pane.list "{\"workspace_id\":\"$SHARED_WS\"}")"
python3 - "$BOARD_DB" "$EVIDENCE" "$PANES" "$E2E_TMP" <<'PY'
import json, pathlib, sqlite3, sys
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
db.row_factory = sqlite3.Row
proof = json.load(open(sys.argv[2]))
panes = json.loads(sys.argv[3])['panes']
for before in proof['runs']:
    rows = db.execute('SELECT * FROM runs WHERE card_id=?', (before['card_id'],)).fetchall()
    assert len(rows) == 1
    after = dict(rows[0])
    for key, value in before.items():
        if key not in ('tab_id', 'cwd'): assert after[key] == value
    pane = next(p for p in panes if p['pane_id'] == before['herdr_pane_id'])
    assert pane['tab_id'] == before['tab_id']
    # Every actual fake launch exclusively creates a transcript with its PID.
    assert len(list(pathlib.Path(sys.argv[4], 'fake-pi-sessions').glob(
        f"*-{before['id']}-*.jsonl"))) == 1
proof['restart_preserved_identities'] = True
json.dump(proof, open(sys.argv[2], 'w'), indent=2)
print('  restart preserved both durable/live identities and exactly one launch per run')
PY

step "Finish the oldest run and admit the globally next queued card"
e2e_board_herdr_mutate -- done "$A" --outcome ok >/dev/null
for _ in $(seq 1 100); do
  [ "$(card_field "$C" card.status)" = running ] && break
  sleep .1
done
[ "$(card_field "$C" card.status)" = running ] || fail 'next queued card was not admitted'
[ "$(card_field "$B" card.status)" = running ] || fail 'sibling worker did not remain running'
e2e_board_herdr_mutate -- done "$B" --outcome ok >/dev/null
e2e_board_herdr_mutate -- done "$C" --outcome ok >/dev/null
for card in "$A" "$B" "$C"; do
  [ "$(card_field "$card" runs[-1].outcome)" = ok ] || fail "card $card did not finish ok"
done
step "41-workspace-concurrency: ALL CHECKS PASSED"
