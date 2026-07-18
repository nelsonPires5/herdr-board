#!/usr/bin/env bash
# 11-pi-harness.sh — built-in Pi dispatch + retry through real Herdr, fake Pi.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Standalone runs boot their disposable Herdr server here; run-all already
# enabled the same PATH before booting its shared ephemeral server.
e2e_enable_fake_pi

# Safety assertion before any card can dispatch: never let this scenario resolve
# the user's real Pi executable.
resolved_pi="$(command -v pi)"
[ "$resolved_pi" = "$E2E_FAKE_PI_BIN_DIR/pi" ] \
  || fail "fake Pi shadowing failed: resolved $resolved_pi"

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace for built-in Pi"
e2e_ws_create board-pi-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create auto Pi column with an appended system prompt"
EXEC_ID="$(col_create '{"name":"Pi Execute","trigger":"auto","system_prompt":"PI E2E SYSTEM"}')"
[ -n "$EXEC_ID" ] || fail "could not create Pi Execute column"

step "Create explicit-model/low-thinking Pi card with a leading-dash prompt"
card_json="$("$BOARD_BIN" card new --title "Pi Harness Card" \
  "--description=--leading-dash card prompt" --harness pi \
  --model test-provider/test-model --effort low \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"

step "Dispatch Pi through real Herdr (the executable is hermetically fake)"
mut "board move $CARD_ID 'Pi Execute' -> herdr agent.start argv[0]=pi"
"$BOARD_BIN" move "$CARD_ID" "Pi Execute" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 80)" || {
  tail -60 "$E2E_TMP/daemon.log" || true
  "$BOARD_BIN" card show "$CARD_ID" --json || true
  fail "first Pi run outcome '$outcome', expected ok"
}
[ "$outcome" = "ok" ] || fail "first Pi run outcome '$outcome', expected ok"

show1="$E2E_TMP/show-first.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$show1"
read -r RUN1 SESSION1 < <(python3 - "$show1" <<'PY'
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
card, runs, comments = x["card"], x["runs"], x["comments"]
assert card["harness"] == "pi", card
assert card["model"] == "test-provider/test-model", card
assert card["effort"] == "low", card
assert len(runs) == 1, runs
run = runs[0]
assert run["harness"] == "pi", run
argv = json.loads(run["argv_json"])
assert argv[0] == "pi", argv
assert "--" not in argv, argv
for pair in [
    ("--model", "test-provider/test-model"),
    ("--thinking", "low"),
]:
    assert any(argv[i:i+2] == list(pair) for i in range(len(argv)-1)), (pair, argv)
system = argv[argv.index("--append-system-prompt") + 1]
assert "PI E2E SYSTEM" in system, system
assert "## herdr-board protocol" in system, system
session = argv[argv.index("--session-id") + 1]
assert run["session_id"] == session, (run, argv)
assert argv[-1].startswith("Card task:\n--leading-dash card prompt"), argv
assert any(c["author"] == f"agent:{run['id']}" and "fake pi: argv validated" in c["body"]
           for c in comments), comments
print(run["id"], session)
PY
)
[ -f "$E2E_TMP/fake-pi-run-$RUN1.json" ] \
  || fail "fake Pi did not record argv for run $RUN1"
ok "first run used Pi model/thinking/system/session/prompt argv and agent comment"

step "Retry the finished Pi card; fork old session into a new exact id"
"$BOARD_BIN" retry "$CARD_ID" --json >/dev/null
outcome2="$(wait_runs "$CARD_ID" 2 100)" || {
  tail -60 "$E2E_TMP/daemon.log" || true
  "$BOARD_BIN" card show "$CARD_ID" --json || true
  fail "second Pi run did not finish"
}
[ "$outcome2" = "ok" ] || fail "retry Pi run outcome '$outcome2', expected ok"

show2="$E2E_TMP/show-second.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$show2"
read -r RUN2 SESSION2 < <(python3 - "$show2" "$SESSION1" <<'PY'
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
old = sys.argv[2]
card, runs, comments = x["card"], x["runs"], x["comments"]
assert len(runs) == 2, runs
run = runs[-1]
argv = json.loads(run["argv_json"])
assert argv[0] == "pi", argv
assert argv[argv.index("--fork") + 1] == old, argv
new = argv[argv.index("--session-id") + 1]
assert new != old, (old, new)
assert run["session_id"] == new, run
assert card["session_id"] == new, card
assert any(c["author"] == f"agent:{run['id']}" and "fake pi: argv validated" in c["body"]
           for c in comments), comments
print(run["id"], new)
PY
)
[ "$SESSION2" != "$SESSION1" ] || fail "Pi retry reused old session id"
[ -f "$E2E_TMP/fake-pi-run-$RUN2.json" ] \
  || fail "fake Pi did not record argv for retry run $RUN2"
ok "retry forked $SESSION1 -> $SESSION2 and persisted the new Pi session"

step "11-pi-harness: ALL CHECKS PASSED"
