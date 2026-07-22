#!/usr/bin/env bash
# 16-managed-p17.sh — protocol-17 managed Pi/Claude launch contract.
#
# The provider-free terminal fixtures validate the authoritative 0600 system
# file, report session identity then idle lifecycle against HERDR_PANE_ID, emit
# each client's idle terminal markers, and stay attached to an interactive tty.
# They refuse to finish until agent.prompt's exact card prompt arrives on stdin;
# readiness or delivery failure therefore remains a hard failure, never a
# startup-only pass or an early board-done bypass.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_enable_fake_pi
[ "$(type -P pi)" = "$E2E_FAKE_PI_BIN_DIR/pi" ] || fail "fake Pi shadowing failed"
[ "$(type -P claude)" = "$E2E_FAKE_PI_BIN_DIR/claude" ] || fail "fake Claude shadowing failed"
[ "$(type -t pi)" = function ] || fail "fake Pi exec function was not exported"
[ "$(type -t claude)" = function ] || fail "fake Claude exec function was not exported"
e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

managed_failure_diag() {
  local kind="$1" card="$2" panes target record
  printf '\n--- managed %s failure diagnostics (disposable session only) ---\n' "$kind" >&2
  "$BOARD_BIN" card show "$card" --json >&2 2>/dev/null || true
  printf '%s\n' 'isolated temp contents:' >&2
  find "$E2E_TMP" -maxdepth 2 -type f -printf '  %p\n' >&2 2>/dev/null || true
  while IFS= read -r record; do
    [ -f "$record" ] || continue
    printf '%s\n' "record: $record" >&2
    python3 -m json.tool "$record" >&2 2>/dev/null || true
  done < <(find "$E2E_TMP" -maxdepth 1 -type f -name "fake-$kind-run-*.json" -print 2>/dev/null)
  panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
  printf '%s\n' "pane.list: ${panes:-<unavailable>}" >&2
  target="$(printf '%s' "$panes" | python3 -c '
import json, sys
try: panes=json.load(sys.stdin).get("panes", [])
except Exception: panes=[]
kind=sys.argv[1]
for pane in panes:
    if pane.get("agent") == kind:
        print(pane["pane_id"]); break
' "$kind" 2>/dev/null || true)"
  if [ -n "$target" ]; then
    printf '%s\n' "agent target pane: $target" >&2
    HERDR_SOCKET_PATH="$HERDR_SOCKET_PATH" "$HERDR_BIN" agent get "$target" >&2 2>&1 || true
    HERDR_SOCKET_PATH="$HERDR_SOCKET_PATH" "$HERDR_BIN" agent explain "$target" --json >&2 2>&1 || true
  else
    printf '%s\n' 'agent target: <not found>' >&2
  fi
  printf '%s\n' '--- end managed diagnostics ---' >&2
}

e2e_ws_create p17-managed; WS_ID="$E2E_WS"
workspace_panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
MANAGED_PANE_CWD="$(printf '%s' "$workspace_panes" | python3 -c '
import json, sys
panes=json.load(sys.stdin).get("panes", [])
cwds=[p.get("cwd") for p in panes if p.get("cwd")]
assert cwds, panes
print(cwds[0])
')"
printf '  disposable workspace pane cwd: %s\n' "$MANAGED_PANE_CWD"
EXEC_ID="$(col_create '{"name":"P17 Execute","trigger":"auto"}')"

step "Dispatch fake Pi through managed protocol-17 launch"
pi_json="$("$BOARD_BIN" card new --title 'P17 Pi' --description $'description with spaces\nand a newline' \
  --harness pi --model p17/pi-model --effort low --space-kind workspace --space-ref "$WS_ID" --json)"
PI_ID="$(printf '%s' "$pi_json" | jget id)"
mut "board move $PI_ID 'P17 Execute' -> managed agent.start kind=pi"
"$BOARD_BIN" move "$PI_ID" "P17 Execute" --json >/dev/null
pi_outcome="$(wait_ok "$PI_ID" 100)" || {
  tail -80 "$E2E_TMP/daemon.log" || true
  managed_failure_diag pi "$PI_ID"
  fail "managed Pi outcome '$pi_outcome' (readiness/agent.prompt did not complete)"
}
[ "$pi_outcome" = ok ] || fail "managed Pi did not complete ok (got '$pi_outcome')"
PI_RUN_ID="$(card_field "$PI_ID" runs[-1].id)"
PI_PANE_ID="$(card_field "$PI_ID" runs[-1].herdr_pane_id)"
PI_RECORD="$E2E_TMP/fake-pi-run-$PI_RUN_ID.json"
PI_SHOW="$E2E_TMP/pi-show.json"
"$BOARD_BIN" card show "$PI_ID" --json >"$PI_SHOW"
[ -f "$PI_RECORD" ] || fail "fake Pi did not record run $PI_RUN_ID"
python3 - "$PI_RECORD" "$PI_SHOW" "$PI_ID" "$PI_RUN_ID" "$BOARD_SOCKET" \
  "$HERDR_SOCKET_PATH" "$MANAGED_PANE_CWD" <<'PY'
import json, os, sys
record, show_path, card, run, board, herdr, cwd = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
expected_prompt = show["runs"][-1]["prompt_snapshot"]
protocol = """## herdr-board protocol
You are running a herdr-board card ($BOARD_CARD_ID is preset). When this stage's goal is met you MUST finish with exactly two commands: first `board comment \"<your results, files touched, findings>\"`, then `board done --outcome ok`. If the stage goal was NOT met — something failed or you got lost — use `board done --outcome fail --summary \"<why>\"` instead. Always comment before done. Never use `board move`/`cancel`/`retry` on your own card. Finishing or going idle WITHOUT `board done` leaves the card in `awaiting` for human review — a run is never auto-completed."""
assert str(x["card_id"]) == card and str(x["run_id"]) == run, x
assert x["board_socket"] == board and x["herdr_socket"] == herdr and x["cwd"] == cwd, x
assert x["model"] == "p17/pi-model" and x["thinking"] == "low", x
assert x["argv"][:-2] == ["--model", "p17/pi-model", "--thinking", "low", "--session-id", x["session_id"]], x
assert x["argv"][-2:] == ["--append-system-prompt", x["system_prompt_file"]], x
assert x["system_prompt_exists_at_read"] is True and x["system_prompt_mode"] == 0o600, x
assert x["system_prompt"] == protocol, x["system_prompt"]
assert not os.path.exists(x["system_prompt_file"]), x["system_prompt_file"]
assert x["readiness_report"] == "ok" and x["herdr_pane_id"], x
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
assert x["stdin_isatty"] is True and x["prompt_received_via_stdin"] is True, x
assert x["prompt_matches_run_snapshot"] is True, x
assert x["prompt"] == expected_prompt, (x["prompt"], expected_prompt)
assert not any("description with spaces" in arg or "herdr-board protocol" in arg for arg in x["argv"]), x
print("  Pi: 0600 system file exact; readiness reported; exact agent.prompt captured on tty")
PY

step "Dispatch fake Claude through managed protocol-17 launch"
claude_json="$("$BOARD_BIN" card new --title 'P17 Claude' --description $'claude description with spaces\nand a newline' \
  --harness claude --model p17/claude-model --effort low --permission acceptEdits \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CLAUDE_ID="$(printf '%s' "$claude_json" | jget id)"
mut "board move $CLAUDE_ID 'P17 Execute' -> managed agent.start kind=claude"
"$BOARD_BIN" move "$CLAUDE_ID" "P17 Execute" --json >/dev/null
claude_outcome="$(wait_ok "$CLAUDE_ID" 100)" || {
  tail -80 "$E2E_TMP/daemon.log" || true
  managed_failure_diag claude "$CLAUDE_ID"
  fail "managed Claude outcome '$claude_outcome' (readiness/agent.prompt did not complete)"
}
[ "$claude_outcome" = ok ] || fail "managed Claude did not complete ok (got '$claude_outcome')"
CLAUDE_RUN_ID="$(card_field "$CLAUDE_ID" runs[-1].id)"
CLAUDE_PANE_ID="$(card_field "$CLAUDE_ID" runs[-1].herdr_pane_id)"
CLAUDE_RECORD="$E2E_TMP/fake-claude-run-$CLAUDE_RUN_ID.json"
CLAUDE_SHOW="$E2E_TMP/claude-show.json"
"$BOARD_BIN" card show "$CLAUDE_ID" --json >"$CLAUDE_SHOW"
[ -f "$CLAUDE_RECORD" ] || fail "fake Claude did not record run $CLAUDE_RUN_ID"
python3 - "$CLAUDE_RECORD" "$CLAUDE_SHOW" "$CLAUDE_ID" "$CLAUDE_RUN_ID" \
  "$BOARD_SOCKET" "$HERDR_SOCKET_PATH" "$MANAGED_PANE_CWD" <<'PY'
import json, os, sys
record, show_path, card, run, board, herdr, cwd = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
expected_prompt = show["runs"][-1]["prompt_snapshot"]
protocol = """## herdr-board protocol
You are running a herdr-board card ($BOARD_CARD_ID is preset). When this stage's goal is met you MUST finish with exactly two commands: first `board comment \"<your results, files touched, findings>\"`, then `board done --outcome ok`. If the stage goal was NOT met — something failed or you got lost — use `board done --outcome fail --summary \"<why>\"` instead. Always comment before done. Never use `board move`/`cancel`/`retry` on your own card. Finishing or going idle WITHOUT `board done` leaves the card in `awaiting` for human review — a run is never auto-completed."""
assert str(x["card_id"]) == card and str(x["run_id"]) == run, x
assert x["board_socket"] == board and x["herdr_socket"] == herdr and x["cwd"] == cwd, x
base = ["--model", "p17/claude-model", "--effort", "low", "--permission-mode", "acceptEdits",
        "--allowedTools", "Bash(board:*)", "--session-id", x["session_id"]]
assert x["argv"][:-2] == base, x
assert x["argv"][-2:] == ["--append-system-prompt-file", x["system_prompt_file"]], x
assert x["system_prompt_exists_at_read"] is True and x["system_prompt_mode"] == 0o600, x
assert x["system_prompt"] == protocol, x["system_prompt"]
assert not os.path.exists(x["system_prompt_file"]), x["system_prompt_file"]
assert x["readiness_report"] == "ok" and x["herdr_pane_id"], x
reports = x["reports"]
assert [r["phase"] for r in reports] == ["session_identity", "idle_lifecycle"], reports
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports), reports
identity, idle = (r["request"] for r in reports)
assert identity["method"] == "pane.report_agent_session", identity
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle", idle
assert identity["params"]["source"] == idle["params"]["source"] == "herdr:claude", reports
assert identity["params"]["session_start_source"] == "startup", identity
assert identity["params"]["seq"] > 10**15 and idle["params"]["seq"] > identity["params"]["seq"], reports
assert x["agent_session_id"] == x["session_id"] and os.path.isfile(x["agent_session_path"]), x
assert x["session_id"] in os.path.basename(x["agent_session_path"]), x
assert x["stdin_isatty"] is True and x["prompt_received_via_stdin"] is True, x
assert x["prompt_matches_run_snapshot"] is True, x
assert x["prompt"] == expected_prompt, (x["prompt"], expected_prompt)
assert not any("claude description" in arg or "herdr-board protocol" in arg for arg in x["argv"]), x
print("  Claude: 0600 system file exact; readiness reported; exact agent.prompt captured on tty")
PY

step "Assert held managed panes have the expected tab/pane/layout structure"
tabs_json="$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")"
panes_json="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
LAYOUT_PANE="$(python3 - "$tabs_json" "$panes_json" "$PI_PANE_ID" "$CLAUDE_PANE_ID" <<'PY'
import json, sys
tabs=json.loads(sys.argv[1]).get("tabs",[]); panes=json.loads(sys.argv[2]).get("panes",[])
pi_id, claude_id = sys.argv[3:]
kanban=[t for t in tabs if t.get("label")=="kanban"]
assert len(kanban)==1, tabs
kp=[p for p in panes if p.get("tab_id")==kanban[0]["tab_id"]]
by_id={p["pane_id"]: p for p in kp}
assert by_id[pi_id].get("agent") == "pi", (pi_id, kp)
assert by_id[claude_id].get("agent") == "claude", (claude_id, kp)
print(pi_id)
PY
)"
layout_json="$(hrpc pane.layout "{\"pane_id\":\"$LAYOUT_PANE\"}")"
python3 - "$layout_json" "$PI_PANE_ID" "$CLAUDE_PANE_ID" <<'PY'
import json, sys
pane_ids={p["pane_id"] for p in json.loads(sys.argv[1]).get("layout", {}).get("panes", [])}
assert set(sys.argv[2:]).issubset(pane_ids), (pane_ids, sys.argv[2:])
print("  pane.layout contains both exact bounded-held managed card panes")
PY

ok "fixture boundary: no provider was called; passing required live Herdr readiness, ordered identity/idle reports, and exact stdin delivery"
step "16-managed-p17: SYSTEM FILE + AGENT.PROMPT + HELD LAYOUT CONTRACTS PASSED"
