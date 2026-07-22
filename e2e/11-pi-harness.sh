#!/usr/bin/env bash
# 11-pi-harness.sh — built-in Pi dispatch + retry through real Herdr, fake Pi.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Standalone runs boot their disposable Herdr server here; run-all already
# enabled the same PATH before booting its shared ephemeral server.
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
assert "--append-system-prompt" not in argv, argv
assert not any("PI E2E SYSTEM" in arg or "leading-dash card prompt" in arg for arg in argv), argv
session = argv[argv.index("--session-id") + 1]
assert run["session_id"] == session, (run, argv)
assert any(c["author"] == f"agent:{run['id']}" and "system file and agent.prompt validated" in c["body"]
           for c in comments), comments
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
assert x["argv"][-2:] == ["--append-system-prompt", x["system_prompt_file"]], x
assert x["system_prompt"] == "PI E2E SYSTEM\n\n" + protocol, x["system_prompt"]
assert x["system_prompt_exists_at_read"] is True and x["system_prompt_mode"] == 0o600, x
assert not os.path.exists(x["system_prompt_file"]), x["system_prompt_file"]
assert x["readiness_report"] == "ok" and x["stdin_isatty"] is True, x
reports = x["reports"]
assert [r["phase"] for r in reports] == ["session_identity", "idle_lifecycle"], reports
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports), reports
identity, idle = (r["request"] for r in reports)
assert identity["method"] == "pane.report_agent_session", identity
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle", idle
assert identity["params"]["source"] == idle["params"]["source"] == "herdr:pi", reports
assert identity["params"]["session_start_source"] == "startup", identity
assert identity["params"]["seq"] > 10**15 and idle["params"]["seq"] > identity["params"]["seq"], reports
assert x["agent_session_id"] is None and os.path.isfile(x["agent_session_path"]), x
assert x["session_id"] in os.path.basename(x["agent_session_path"]), x
assert x["prompt_received_via_stdin"] is True and x["prompt_matches_run_snapshot"] is True, x
assert x["prompt"] == show["runs"][0]["prompt_snapshot"], x["prompt"]
PY
ok "first run split the exact system file from the exact stdin/agent.prompt card task"

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
assert "--append-system-prompt" not in argv, argv
assert not any("PI E2E SYSTEM" in arg or "leading-dash card prompt" in arg for arg in argv), argv
assert any(c["author"] == f"agent:{run['id']}" and "system file and agent.prompt validated" in c["body"]
           for c in comments), comments
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
assert x["argv"][-2:] == ["--append-system-prompt", x["system_prompt_file"]], x
assert x["system_prompt"].startswith("PI E2E SYSTEM\n\n## herdr-board protocol\n"), x
assert x["system_prompt_exists_at_read"] is True and x["system_prompt_mode"] == 0o600, x
assert not os.path.exists(x["system_prompt_file"]), x["system_prompt_file"]
assert x["readiness_report"] == "ok" and x["stdin_isatty"] is True, x
reports = x["reports"]
assert [r["phase"] for r in reports] == ["session_identity", "idle_lifecycle"], reports
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports), reports
identity, idle = (r["request"] for r in reports)
assert identity["method"] == "pane.report_agent_session", identity
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle", idle
assert identity["params"]["source"] == idle["params"]["source"] == "herdr:pi", reports
assert identity["params"]["session_start_source"] == "startup", identity
assert identity["params"]["seq"] > 10**15 and idle["params"]["seq"] > identity["params"]["seq"], reports
assert x["agent_session_id"] is None and os.path.isfile(x["agent_session_path"]), x
assert x["session_id"] in os.path.basename(x["agent_session_path"]), x
assert x["prompt_received_via_stdin"] is True and x["prompt_matches_run_snapshot"] is True, x
assert x["prompt"] == show["runs"][-1]["prompt_snapshot"], (x["prompt"], show["runs"][-1])
PY
ok "retry forked $SESSION1 -> $SESSION2 with the same P17 system-file/task split"

step "11-pi-harness: ALL CHECKS PASSED"
