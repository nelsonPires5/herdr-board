#!/usr/bin/env bash
# 11-pi-harness.sh — built-in Pi dispatch + retry through real Herdr, fake Pi.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Install cleanup before fake-managed setup creates either owned root.
trap e2e_cleanup EXIT
e2e_enable_fake_pi

# Safety assertion before any card can dispatch: never let this scenario resolve
# the user's real Pi executable.
resolved_pi="$(type -P pi)"
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
e2e_board_herdr_mutate -- move "$CARD_ID" "Pi Execute" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 80)" || {

  e2e_card_failure_diag "$CARD_ID"
  fail "first Pi run outcome '$outcome', expected ok"
}
[ "$outcome" = "ok" ] || fail "first Pi run outcome '$outcome', expected ok"

show1="$E2E_TMP/show-first.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$show1"
read -r RUN1 SESSION1 < <(python3 - "$show1" <<'PY'
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
card, runs, comments = x["card"], x["runs"], x["comments"]
assert card["harness"] == "pi"
assert card["model"] == "test-provider/test-model"
assert card["effort"] == "low"
assert len(runs) == 1
run = runs[0]
assert run["harness"] == "pi"
argv = json.loads(run["argv_json"])
assert argv[0] == "pi"
assert "--" not in argv
for pair in [
    ("--model", "test-provider/test-model"),
    ("--thinking", "low"),
]:
        assert any(argv[i:i+2] == list(pair) for i in range(len(argv)-1))
assert "--append-system-prompt" not in argv
assert not any("PI E2E SYSTEM" in arg or "leading-dash card prompt" in arg for arg in argv)
session = argv[argv.index("--session-id") + 1]
assert run["session_id"] == session
assert any(c["author"] == f"agent:{run['id']}" and "system file and agent.prompt validated" in c["body"]
           for c in comments)
print(run["id"], session)
PY
)
PI_RECORD1="$E2E_TMP/fake-pi-run-$RUN1.json"
[ -f "$PI_RECORD1" ] || fail "fake Pi did not record argv for run $RUN1"
python3 - "$PI_RECORD1" "$show1" <<'PY'
import json, os, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
show = json.load(open(sys.argv[2], encoding="utf-8"))
protocol = """## herdr-board protocol
You are running a herdr-board card ($BOARD_CARD_ID is preset). When this stage's goal is met you MUST finish with exactly two commands: first `board comment \"<your results, files touched, findings>\"`, then `board done --outcome ok`. If the stage goal was NOT met — something failed or you got lost — use `board done --outcome fail --summary \"<why>\"` instead. Always comment before done. Never use `board move`/`cancel`/`retry` on your own card. Finishing or going idle WITHOUT `board done` leaves the card in `awaiting` for human review — a run is never auto-completed."""
assert x["argv"][-2:] == ["--append-system-prompt", x["system_prompt_file"]]
assert x["system_prompt"] == "PI E2E SYSTEM\n\n" + protocol
assert x["system_prompt_exists_at_read"] is True and x["system_prompt_mode"] == 0o600
assert not os.path.exists(x["system_prompt_file"])
assert x["readiness_report"] == "ok" and x["stdin_isatty"] is True
reports = x["reports"]
assert [r["phase"] for r in reports] == ["session_identity", "idle_lifecycle"]
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports)
identity, idle = (r["request"] for r in reports)
assert identity["method"] == "pane.report_agent_session"
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle"
assert identity["params"]["source"] == idle["params"]["source"] == "herdr:pi"
assert identity["params"]["session_start_source"] == "startup"
assert identity["params"]["seq"] > 10**15 and idle["params"]["seq"] > identity["params"]["seq"]
assert x["agent_session_id"] is None and os.path.isfile(x["agent_session_path"])
assert x["session_id"] in os.path.basename(x["agent_session_path"])
assert x["prompt_received_via_stdin"] is True and x["prompt_matches_run_snapshot"] is True
assert x["prompt"] == show["runs"][0]["prompt_snapshot"]
PY
ok "first run split the exact system file from the exact stdin/agent.prompt card task"

step "Retry the finished Pi card; fork old session into a new exact id"
e2e_board_herdr_mutate -- retry "$CARD_ID" --json >/dev/null
outcome2="$(wait_runs "$CARD_ID" 2 100)" || {

  e2e_card_failure_diag "$CARD_ID"
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
assert len(runs) == 2
run = runs[-1]
argv = json.loads(run["argv_json"])
assert argv[0] == "pi"
assert argv[argv.index("--fork") + 1] == old
new = argv[argv.index("--session-id") + 1]
assert new != old
assert run["session_id"] == new
assert card["session_id"] == new
assert "--append-system-prompt" not in argv
assert not any("PI E2E SYSTEM" in arg or "leading-dash card prompt" in arg for arg in argv)
assert any(c["author"] == f"agent:{run['id']}" and "system file and agent.prompt validated" in c["body"]
           for c in comments)
print(run["id"], new)
PY
)
[ "$SESSION2" != "$SESSION1" ] || fail "Pi retry reused old session id"
PI_RECORD2="$E2E_TMP/fake-pi-run-$RUN2.json"
[ -f "$PI_RECORD2" ] || fail "fake Pi did not record argv for retry run $RUN2"
python3 - "$PI_RECORD2" "$show2" <<'PY'
import json, os, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
show = json.load(open(sys.argv[2], encoding="utf-8"))
assert x["argv"][-2:] == ["--append-system-prompt", x["system_prompt_file"]]
assert x["system_prompt"].startswith("PI E2E SYSTEM\n\n## herdr-board protocol\n")
assert x["system_prompt_exists_at_read"] is True and x["system_prompt_mode"] == 0o600
assert not os.path.exists(x["system_prompt_file"])
assert x["readiness_report"] == "ok" and x["stdin_isatty"] is True
reports = x["reports"]
assert [r["phase"] for r in reports] == ["session_identity", "idle_lifecycle"]
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports)
identity, idle = (r["request"] for r in reports)
assert identity["method"] == "pane.report_agent_session"
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle"
assert identity["params"]["source"] == idle["params"]["source"] == "herdr:pi"
assert identity["params"]["session_start_source"] == "startup"
assert identity["params"]["seq"] > 10**15 and idle["params"]["seq"] > identity["params"]["seq"]
assert x["agent_session_id"] is None and os.path.isfile(x["agent_session_path"])
assert x["session_id"] in os.path.basename(x["agent_session_path"])
assert x["prompt_received_via_stdin"] is True and x["prompt_matches_run_snapshot"] is True
assert x["prompt"] == show["runs"][-1]["prompt_snapshot"]
PY
ok "retry forked $SESSION1 -> $SESSION2 with the same P17 system-file/task split"

step "11-pi-harness: ALL CHECKS PASSED"
