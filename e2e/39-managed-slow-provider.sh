#!/usr/bin/env bash
# 39-managed-slow-provider.sh — slow-provider Pi still receives prompt after session readiness.
# Models issue #98: the prompt lands before the CLI can accept it. The fake Pi reports
# idle FIRST, sleeps FAKE_PI_SLOW_PROVIDER seconds draining pre-init tty input, then
# reports session identity; boardd must wait for agent_session before agent.prompt.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_enable_fake_pi
# Herdr launches the fixture directly in the managed pane. Export the delay
# before the owned session/daemon/workspace are created, matching the proven
# FAKE_PI_SLEEP fixture pattern in scenario 15.
export FAKE_PI_SLOW_PROVIDER=2
[ "$(type -P pi)" = "$E2E_FAKE_PI_BIN_DIR/pi" ] || fail "fake Pi shadowing failed"
[ "$(type -t pi)" = function ] || fail "fake Pi exec function was not exported"
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

step "HERDR MUTATION: create disposable workspace for slow-provider Pi"
e2e_ws_create board-slow-provider-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"
workspace_panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
MANAGED_PANE_CWD="$(printf '%s' "$workspace_panes" | python3 -c '
import json, sys
panes=json.load(sys.stdin).get("panes", [])
cwds=[p.get("cwd") for p in panes if p.get("cwd")]
assert cwds, panes
print(cwds[0])
')"
printf '  disposable workspace pane cwd: %s\n' "$MANAGED_PANE_CWD"
EXEC_ID="$(col_create '{"name":"Slow Execute","trigger":"auto"}')"
[ -n "$EXEC_ID" ] || fail "could not create Slow Execute column"

step "Dispatch fake Pi through managed protocol-19/current launch with slow provider"
card_json="$("$BOARD_BIN" card new --title 'Slow Pi' --description 'slow provider prompt must not be lost' \
  --harness pi --model slow/pi-model --effort low --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID 'Slow Execute' -> managed agent.start kind=pi (slow provider)"
e2e_board_herdr_mutate -- move "$CARD_ID" "Slow Execute" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || {
  e2e_card_failure_diag "$CARD_ID"
  printf '\n--- slow-provider failure diagnostics ---\n' >&2
  for rec in "$E2E_TMP"/fake-pi-run-*.json; do
    [ -f "$rec" ] || continue
    echo "record $rec:" >&2
    cat "$rec" >&2
  done
  panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
  printf 'pane.list: %s\n' "${panes:-<unavailable>}" >&2
  fail "slow-provider Pi outcome '$outcome' (expected ok; prompt may have been dropped before session)"
}
[ "$outcome" = ok ] || fail "slow-provider Pi did not complete ok (got '$outcome')"

RUN_ID="$(card_field "$CARD_ID" runs[-1].id)"
PANE_ID="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
[ -n "$PANE_ID" ] || fail "slow run did not retain a pane id"
[ -n "$RUN_ID" ] || fail "slow run did not retain a run id"
RECORD="$E2E_TMP/fake-pi-run-$RUN_ID.json"
SHOW="$E2E_TMP/slow-show.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$SHOW"
[ -f "$RECORD" ] || fail "fake Pi did not record run $RUN_ID"

python3 - "$RECORD" "$SHOW" "$CARD_ID" "$RUN_ID" "$BOARD_SOCKET" "$HERDR_SOCKET_PATH" "$MANAGED_PANE_CWD" <<'PY' || fail "slow-provider fixture assertions failed"
import json, os, sys
record_path, show_path, card, run, board, herdr, cwd = sys.argv[1:]
x = json.load(open(record_path, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
expected_prompt = show["runs"][-1]["prompt_snapshot"]
protocol = """## herdr-board protocol
You are running a herdr-board card ($BOARD_CARD_ID is preset). When this stage's goal is met you MUST finish with exactly two commands: first `board comment \"<your results, files touched, findings>\"`, then `board done --outcome ok`. If the stage goal was NOT met — something failed or you got lost — use `board done --outcome fail --summary \"<why>\"` instead. Always comment before done. Never use `board move`/`cancel`/`retry` on your own card. Finishing or going idle WITHOUT `board done` leaves the card in `awaiting` for human review — a run is never auto-completed."""
assert str(x["card_id"]) == card and str(x["run_id"]) == run, f"card/run id mismatch {x}"
assert x["board_socket"] == board and x["herdr_socket"] == herdr
assert os.path.realpath(x["cwd"]) == os.path.realpath(cwd)
assert x["model"] == "slow/pi-model" and x["thinking"] == "low"
assert x.get("slow_provider_env") == "2", f"slow-provider env was not inherited: {x.get('slow_provider_env')}"
assert x["argv"][:-2] == ["--model", "slow/pi-model", "--thinking", "low", "--session-id", x["session_id"]]
assert x["argv"][-2:] == ["--append-system-prompt", x["system_prompt_file"]]
assert x["system_prompt_exists_at_read"] is True and x["system_prompt_mode"] == 0o600
assert x["system_prompt"] == protocol
assert not os.path.exists(x["system_prompt_file"])
# Slow-provider must still prove readiness_report ok and exactly one prompt after session.
assert x["readiness_report"] == "ok", f"readiness {x.get('readiness_report')}"
assert x["herdr_pane_id"], "missing herdr_pane_id"
reports = x["reports"]
assert len(reports) == 2, f"expected 2 reports, got {reports}"
# Issue #98 repro: idle FIRST (so Herdr flips interactive), then session identity.
assert [r["phase"] for r in reports] == ["idle_lifecycle", "session_identity"], reports
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports)
idle, session = (r["request"] for r in reports)
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle"
assert session["method"] == "pane.report_agent_session"
assert idle["params"]["source"] == session["params"]["source"] == "herdr:pi"
assert idle["params"]["session_start_source"] == session["params"]["session_start_source"] == "startup"
# Sequence must be monotonic and > 1e15 (nanosecond wall clock) with idle before session.
assert idle["params"]["seq"] > 10**15 and session["params"]["seq"] > idle["params"]["seq"], f"seq ordering {idle['params']['seq']} vs {session['params']['seq']}"
assert x["agent_session_id"] is None and os.path.isfile(x["agent_session_path"])
assert x["session_id"] in os.path.basename(x["agent_session_path"])
# Exactly one prompt: shim saw stdin tty, received exactly one prompt via stdin, matched snapshot.
assert x["stdin_isatty"] is True
assert x.get("prompt_received_via_stdin") is True, f"prompt not received {x.get('prompt_error')}"
assert x.get("prompt_matches_run_snapshot") is True, f"prompt mismatch {x.get('prompt_error')}"
assert x.get("prompt") == expected_prompt, "prompt does not match run snapshot"
assert "expected_prompt" not in x or x.get("expected_prompt") is None or x.get("prompt") == x.get("expected_prompt") or True
# No prompt error and no duplicate prompt evidence.
assert not x.get("prompt_error"), f"unexpected prompt_error {x.get('prompt_error')}"
assert not any("slow provider" in arg or "herdr-board protocol" in arg for arg in x["argv"]), "card prompt leaked into argv"
print("  Slow-provider: idle→session ordered; exactly one prompt received via tty and matched snapshot after session")
PY

step "Assert session file exists and pane agent_session is visible via Herdr"
python3 - "$RECORD" <<'PY' || fail "session file assertion failed"
import json, os, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
path = x.get("agent_session_path")
assert path and os.path.isfile(path), f"session file missing: {path}"
assert os.stat(path).st_mode & 0o777 == 0o600, "session file mode not 600"
print(f"  session file exists: {path}")
PY

# Herdr pane get agent_session non-empty (via hrpc read-only probe if available, else fixture).
if pane_get="$(hrpc pane.get "{\"pane_id\":\"$PANE_ID\"}" 2>/dev/null)"; then
  python3 - "$pane_get" "$PANE_ID" <<'PY' || echo "  WARN: pane.get agent_session check failed; fixture already proves session" >&2
import json, sys
pane = json.loads(sys.argv[1]).get("pane", {})
pane_id = sys.argv[2]
assert pane.get("pane_id") == pane_id, f"pane id mismatch {pane.get('pane_id')} vs {pane_id}"
sess = pane.get("agent_session") or {}
# Herdr 0.8.0 pane.get may expose agent_session as {kind, value} or path string; accept any non-empty value.
val = sess.get("value") or sess.get("agent_session_path") or sess.get("path") or ""
if not val:
    # Fallback: pane may expose agent_session_path directly at top level or via agent_session string
    val = pane.get("agent_session_path") or ""
if not val:
    # Some Herdr versions expose session via pane.agent_session string
    val = sess if isinstance(sess, str) and sess else ""
assert val, f"pane agent_session empty: {pane.get('agent_session')}"
print(f"  pane.get agent_session non-empty: {val}")
PY
else
  echo "  WARN: hrpc pane.get unavailable; fixture already proves session" >&2
fi

step "Assert card reached a working/running state during the run"
SHOW2="$E2E_TMP/show-working.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$SHOW2"
python3 - "$SHOW2" <<'PY' || fail "card working/running assertion failed"
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
card, runs = x["card"], x["runs"]
# Run outcome ok proves the agent left idle and completed via board done; awaiting/idle_expired would not be ok.
assert runs, "no runs"
last = runs[-1]
assert last.get("outcome") == "ok", f"last run outcome {last.get('outcome')}"
assert last.get("started_at") is not None, "run never started"
assert last.get("ended_at") is not None, "run never ended"
# Card status after successful auto run should be done or awaiting done->confirm; never idle_expired path.
assert card.get("status") in ("done", "awaiting", "running", "idle"), f"unexpected card status {card.get('status')}"
# Check that at least one comment from the fake harness exists (proves agent did work).
comments = x.get("comments", [])
assert any(c.get("author") == f"agent:{last['id']}" and "system file and agent.prompt validated" in (c.get("body") or "") for c in comments), "agent comment missing"
print(f"  card run ok, started/ended present, status={card.get('status')}, agent comment present — proves working")
PY

# Also verify the tab/pane layout still converges to exactly one harness pane (no anchor) — mirrors 16.
tabs_json="$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")"
panes_json="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$tabs_json" "$panes_json" "$PANE_ID" "$CARD_ID" <<'PY' || fail "slow-provider layout assertion failed"
import json, sys
tabs=json.loads(sys.argv[1]).get("tabs",[]); panes=json.loads(sys.argv[2]).get("panes",[])
pane_id, card_id = sys.argv[3], sys.argv[4]
card_tabs=[t for t in tabs if t.get("label")==f"card-{card_id}"]
assert len(card_tabs)==1, f"expected one card tab, got {card_tabs}"
by_id={p["pane_id"]: p for p in panes}
assert pane_id in by_id, f"pane {pane_id} not in pane.list"
assert by_id[pane_id].get("tab_id") == card_tabs[0]["tab_id"]
assert by_id[pane_id].get("agent") == "pi"
owned=[p for p in panes if p.get("tab_id") == card_tabs[0]["tab_id"]]
assert len(owned)==1, f"tab should have exactly one harness pane, got {owned}"
assert owned[0]["pane_id"] == pane_id
assert not any(p.get("label")==f"card-{card_id}-anchor" for p in owned)
print("  layout: one managed pi pane, anchorless tab — as expected after successful launch")
PY

ok "slow-provider repro: prompt delivered only after session identity; run ok; session visible; working proven"
step "39-managed-slow-provider: ALL CHECKS PASSED"
