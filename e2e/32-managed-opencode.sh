#!/usr/bin/env bash
# 32-managed-opencode.sh — managed OpenCode TUI protocol-20/current launch
# contract, provider-free through e2e/fake-bin/opencode.
#
# Contract exercised end to end against a real Herdr:
#   1. Mint: `opencode [--agent herdr-board | -m M] [--auto]` — no session flag
#      (opencode TUI mints its own `ses_…` id), no prompt text in argv, no
#      system-prompt file; the daemon captures the integration-reported
#      session id from `agent.get.agent_session` ({agent: opencode, kind: id,
#      source: herdr:opencode, value}) and persists it on run+card; the card task
#      arrives through `agent.prompt` as ONE delimited system+task block and
#      matches the run snapshots. With a board effort the root/TUI has NO
#      `--variant` flag, so effort rides the process-local
#      `OPENCODE_CONFIG_CONTENT` env — a custom `herdr-board` agent carrying
#      model+variant — selected with `--agent herdr-board`; without an effort
#      the model stays `-m` and no config env exists;
#   2. Retry: `board retry` forks the recorded session (`-s <id> --fork`
#      argv tail), the fork mints a NEW `ses_…` id which replaces the source
#      id at promotion;
#   3. Same-conversation reuse: a non-fresh auto hop re-prompts the SAME pane
#      (no second `agent.start`) with the task alone and keeps the session id;
#   4. Rescue: a run whose pane is closed is reopened with `opencode -s <id>`
#      in a new pane WITHOUT re-sending the task; the runs row stays
#      byte-for-byte unchanged; a second focus reuses the rescued pane;
#   5. Fail-closed missing report: a card whose model is the documented
#      sentinel `no-session` (now riding the agent config, exactly like every
#      effort-bearing model) makes the fixture report only the idle lifecycle,
#      so the capture degrades, the mint still completes with session_id NULL,
#      rescue is refused with an actionable diagnostic, and a non-fresh hop
#      cannot reuse the conversation (it mints fresh instead);
#   6. Managed tabs stay anchorless: every launch converges the `card-<id>`
#      tab to exactly one OpenCode harness pane.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Fake-managed setup creates roots, so cleanup must already be armed.
trap e2e_cleanup EXIT
e2e_enable_fake_pi
[ -x "$E2E_FAKE_PI_BIN_DIR/opencode" ] || fail "fake opencode missing/not executable at $E2E_FAKE_PI_BIN_DIR/opencode"
# Same-conversation reuse needs the looping fixture, and a daemon split pane
# inherits the SERVER's env (never the workspace env), so the knob must be
# exported before e2e_boot boots the ephemeral server.
export FAKE_PI_LOOP=1
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

# The board protocol trailer (the column has no custom system_prompt, so this
# is the exact `system_prompt_snapshot` the daemon persisted).
OPENCODE_PROTOCOL="## herdr-board protocol
You are running a herdr-board card (\$BOARD_CARD_ID is preset). When this stage's goal is met you MUST finish with exactly two commands: first \`board comment \"<your results, files touched, findings>\"\`, then \`board done --outcome ok\`. If the stage goal was NOT met — something failed or you got lost — use \`board done --outcome fail --summary \"<why>\"\` instead. Always comment before done. Never use \`board move\`/\`cancel\`/\`retry\` on your own card. Finishing or going idle WITHOUT \`board done\` leaves the card in \`awaiting\` for human review — a run is never auto-completed."

opencode_failure_diag() {
  local phase="$1" card="$2" panes record
  printf '\n--- opencode %s failure diagnostics (disposable session only) ---\n' "$phase" >&2
  e2e_card_failure_diag "$card"
  printf 'fixture_records=%s\n' "$(find "$E2E_TMP" -maxdepth 1 -type f -name 'fake-opencode-run-*.json' -printf . 2>/dev/null | wc -c)" >&2
  for record in "$E2E_TMP"/fake-opencode-run-*.json; do
    [ -f "$record" ] || continue
    python3 - "$record" <<'PY' >&2 || true
import json, sys
try:
    x = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception as e:
    print(f"record_error={type(e).__name__}")
else:
    safe = {k: v for k, v in x.items() if k not in ("prompt", "expected_prompt")}
    print("record: %s" % safe)
PY
  done
  panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
  printf '%s\n' "pane.list: ${panes:-<unavailable>}" >&2
  printf '%s\n' 'daemon.log tail:' >&2
  tail -60 "$E2E_TMP/daemon.log" 2>/dev/null >&2 || true
  printf '%s\n' '--- end opencode diagnostics ---' >&2
}

# One pane's field from the disposable workspace (read-only probe).
pane_field() {
  hrpc pane.list "{\"workspace_id\":\"$1\"}" | python3 -c '
import json, sys
pane_id, field = sys.argv[1:3]
for pane in json.load(sys.stdin).get("panes", []):
    if pane.get("pane_id") == pane_id:
        print(pane.get(field) or "")
        break
' "$2" "$3"
}

# --- Phase 1: mint + session capture + exact delimited prompt + anchorless ---
step "Mint: fresh OpenCode launch captures its self-minted session id"
e2e_ws_create opencode-mint; WS_ID="$E2E_WS"
EXEC_ID="$(col_create '{"name":"OpenCode Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title 'OpenCode Mint' --description $'opencode mint task with spaces\nand a newline' \
  --harness opencode --model oc32/opencode-model --effort low --permission auto-approve \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse mint card id"
mut "board move $CARD_ID 'OpenCode Execute' -> managed agent.start kind=opencode (mint)"
e2e_board_herdr_mutate -- move "$CARD_ID" "$EXEC_ID" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || {
  opencode_failure_diag mint "$CARD_ID"
  fail "managed OpenCode mint outcome '$outcome' (readiness/capture/agent.prompt did not complete)"
}
[ "$outcome" = ok ] || fail "managed OpenCode mint did not complete ok (got '$outcome')"
MINT_RUN="$(card_field "$CARD_ID" runs[-1].id)"
MINT_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
MINT_SESSION="ses-$CARD_ID-$MINT_RUN"
[ -n "$MINT_RUN" ] && [ -n "$MINT_PANE" ] || fail "mint run did not record its identity"
[ "$(card_field "$CARD_ID" runs[-1].session_id)" = "$MINT_SESSION" ] \
  || fail "mint did not capture/persist the reported session id (expected $MINT_SESSION)"
MINT_RECORD="$E2E_TMP/fake-opencode-run-$MINT_RUN.json"
[ -f "$MINT_RECORD" ] || fail "fake OpenCode did not record run $MINT_RUN"
ok "run $MINT_RUN captured session $MINT_SESSION (pane $MINT_PANE)"

step "Assert the mint argv, the session capture, and the delimited prompt"
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/opencode-mint-show.json"
python3 - "$MINT_RECORD" "$E2E_TMP/opencode-mint-show.json" "$CARD_ID" "$MINT_RUN" "$MINT_SESSION" \
  "$BOARD_SOCKET" "$HERDR_SOCKET_PATH" "$OPENCODE_PROTOCOL" <<'PY' || fail "mint record assertions failed"
import json, sys
record, show_path, card, run, session, board, herdr, protocol = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
expected_prompt = show["runs"][-1]["prompt_snapshot"]
assert str(x["card_id"]) == card and x["run_id"] == int(run)
assert x["board_socket"] == board and x["herdr_socket"] == herdr
assert x["mode"] == "mint" and x["resume_id"] is None and x["fork_id"] is None
assert x["model"] == "oc32/opencode-model" and x["effort"] == "low" and x["permission_mode"] == "auto-approve"
assert x["system_prompt_file"] is None and x["startup_argv_has_no_prompt"] is True
# The pinned opencode TUI argv: `--agent herdr-board` (the config agent owns
# model+variant) + `--auto` permission; `-m` and `--variant` NEVER ride argv
# (the root/TUI rejects --variant); Mint carries NO session flag.
expected_argv = ["--agent", "herdr-board", "--auto"]
assert x["argv"] == expected_argv, f"mint argv {x['argv']} != {expected_argv}"
assert not any("opencode mint task" in arg for arg in x["argv"])
# The transport layer is the process-local config env: exact agent, model,
# and variant (board effort low -> opencode variant low), parsed safely.
assert x["agent"] == "herdr-board" and x["config_env_present"] is True
assert x["config_env_ok"] is True and x["config_env_error"] is None
assert x["config_env_agent"] == "herdr-board"
assert x["config_env_model"] == "oc32/opencode-model"
assert x["config_env_variant"] == "low"
# No prompt text leaks into startup argv or the process-local config evidence.
startup = json.dumps({
    "argv": x["argv"],
    "agent": x["config_env_agent"],
    "model": x["config_env_model"],
    "variant": x["config_env_variant"],
})
assert "opencode mint task" not in startup and "with a newline" not in startup
# The reported session id is exactly what the daemon captured and persisted.
assert x["session_id"] == session and x["agent_session_id"] == session
reports = x["reports"]
assert [r["phase"] for r in reports] == ["session_identity", "idle_lifecycle"]
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports)
identity, idle = (r["request"] for r in reports)
assert identity["method"] == "pane.report_agent_session"
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle"
assert identity["params"]["source"] == idle["params"]["source"] == "herdr:opencode"
assert identity["params"]["agent_session_id"] == session
assert identity["params"]["session_start_source"] == "startup"
assert x["readiness_report"] == "ok" and x["herdr_pane_id"]
# The Mint prompt is ONE delimited block: system instructions, then the task.
block = "## herdr-board system instructions\n" + protocol + "\n\n## herdr-board card task\n" + expected_prompt
assert x["stdin_isatty"] is True and x["prompt_received_via_stdin"] is True
assert x["prompt_matches_mint_block"] is True
assert x["mint_system_half"] == protocol, "the delivered system half is not the exact protocol trailer"
assert x["prompt"] == block, "the delivered mint prompt is not the exact delimited system+task block"
print("  Mint: no session flag/prompt in argv; session id captured; exact delimited block delivered", file=sys.stderr)
PY

step "Assert the managed mint tab is anchorless with exactly one OpenCode pane"
python3 - "$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")" "$CARD_ID" "$MINT_PANE" \
  <<'PY' || fail "mint tab did not converge to one OpenCode pane"
import json, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
card, pane = sys.argv[3:5]
match = [t for t in tabs if t.get("label") == f"card-{card}"]
assert len(match) == 1, f"expected one card-{card} tab, got {len(match)}"
owned = [p for p in panes if p.get("tab_id") == match[0]["tab_id"]]
assert len(owned) == 1, f"expected exactly one pane in the mint tab, got {len(owned)}"
assert owned[0]["pane_id"] == pane and owned[0].get("agent") == "opencode"
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
PY
ok "mint tab holds exactly the one OpenCode pane, no anchor"

# --- Phase 2: retry forks the recorded session into a NEW conversation ------
step "Retry: with the mint pane closed, board retry forks into a NEW OpenCode session"
# A finished run's terminal is routinely closed by the user. Closing it also
# removes the reuse candidate, so the retry exercises the fork LAUNCH itself
# (`-s <id> --fork` in a fresh pane) rather than a same-pane re-prompt.
mut "pane.close $MINT_PANE (disposable board-owned pane of the finished mint run)"
e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$MINT_PANE\"}" >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
  hrpc pane.get "{\"pane_id\":\"$MINT_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
! hrpc pane.get "{\"pane_id\":\"$MINT_PANE\"}" >/dev/null 2>&1 \
  || fail "mint pane $MINT_PANE is still alive; the fork launch case cannot be exercised"
mut "board retry $CARD_ID -> opencode -s <recorded session> --fork"
e2e_board_herdr_mutate -- retry "$CARD_ID" >/dev/null
fork_outcome="$(wait_runs "$CARD_ID" 2 100)" || {
  opencode_failure_diag fork "$CARD_ID"
  fail "OpenCode fork run did not finish (outcome '$fork_outcome')"
}
[ "$fork_outcome" = ok ] || fail "OpenCode fork outcome '$fork_outcome' (expected ok)"
FORK_RUN="$(card_field "$CARD_ID" runs[-1].id)"
FORK_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
FORK_SESSION="ses-$CARD_ID-$FORK_RUN"
[ -n "$FORK_RUN" ] && [ -n "$FORK_PANE" ] || fail "fork run did not record its identity"
[ "$(card_field "$CARD_ID" runs[-1].session_id)" = "$FORK_SESSION" ] \
  || fail "fork did not capture/persist the NEW session id (expected $FORK_SESSION, the fork source is $MINT_SESSION)"
FORK_RECORD="$E2E_TMP/fake-opencode-run-$FORK_RUN.json"
[ -f "$FORK_RECORD" ] || fail "fake OpenCode did not record fork run $FORK_RUN"
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/opencode-fork-show.json"
python3 - "$FORK_RECORD" "$E2E_TMP/opencode-fork-show.json" "$CARD_ID" "$FORK_RUN" \
  "$MINT_SESSION" "$FORK_SESSION" <<'PY' || fail "fork record assertions failed"
import json, sys
record, show_path, card, run, source, session = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
assert str(x["card_id"]) == card and x["run_id"] == int(run)
assert x["mode"] == "fork" and x["fork_id"] == source and x["resume_id"] is None
# The fork is a fresh launch whose argv closes with `-s <source> --fork`.
assert x["argv"][-3:] == ["-s", source, "--fork"], f"fork argv tail {x['argv'][-3:]} != ['-s', {source}, '--fork']"
# The fork minted a NEW session, which the capture persisted over the source id.
assert x["session_id"] == session and x["agent_session_id"] == session
assert x["model"] == "oc32/opencode-model" and x["effort"] == "low"
# The fork launch re-carries the same config transport: --agent herdr-board
# plus the exact model/variant config env, no --variant anywhere.
assert x["agent"] == "herdr-board" and x["config_env_ok"] is True
assert x["config_env_model"] == "oc32/opencode-model" and x["config_env_variant"] == "low"
assert not any(a == "--variant" or a == "-m" for a in x["argv"])
assert x["system_prompt_file"] is None and x["startup_argv_has_no_prompt"] is True
# Resume/fork receive the task ALONE — never the system block.
assert x["prompt_matches_run_snapshot"] is True
assert x["prompt"] == show["runs"][-1]["prompt_snapshot"]
assert "## herdr-board system instructions" not in x["prompt"]
assert "## herdr-board card task" not in x["prompt"]
print("  Fork: '-s <source> --fork' closes the argv; new session captured; task-only prompt", file=sys.stderr)
PY
ok "fork run $FORK_RUN replaced session $MINT_SESSION with $FORK_SESSION (pane $FORK_PANE)"

step "Assert the fork recreated the card tab and converged to one OpenCode pane"
python3 - "$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")" "$CARD_ID" "$FORK_PANE" \
  <<'PY' || fail "fork tab did not converge to one OpenCode pane"
import json, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
card, pane = sys.argv[3:5]
match = [t for t in tabs if t.get("label") == f"card-{card}"]
assert len(match) == 1
owned = [p for p in panes if p.get("tab_id") == match[0]["tab_id"]]
assert len(owned) == 1, f"expected exactly one pane in the fork tab, got {len(owned)}"
assert owned[0]["pane_id"] == pane and owned[0].get("agent") == "opencode"
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
PY
ok "the fork's fresh launch recreated the card tab with exactly one OpenCode pane"

# --- Phase 3: same-conversation reuse across a non-fresh auto hop -----------
step "Same-conversation reuse: a non-fresh auto hop re-prompts the SAME pane"
# FAKE_PI_LOOP must reach the managed pane: a daemon split pane inherits the
# SERVER's env (not the workspace env), so this is exported before e2e_boot
# and stays on for the whole scenario (other cards' finished panes merely
# linger until their next shim's prompt timeout — harmless for assertions).
e2e_ws_create opencode-reuse; REUSE_WS="$E2E_WS"
MANUAL_ID="$(col_create '{"name":"OpenCode Manual","trigger":"manual"}')"
IMPL_ID="$(col_create "{\"name\":\"OpenCode Implement\",\"trigger\":\"auto\",\"on_success_column_id\":$MANUAL_ID,\"fresh_session\":false}")"
SETUP_ID="$(col_create "{\"name\":\"OpenCode Setup\",\"trigger\":\"auto\",\"on_success_column_id\":$IMPL_ID,\"fresh_session\":true}")"
[ -n "$SETUP_ID" ] && [ -n "$IMPL_ID" ] && [ -n "$MANUAL_ID" ] \
  || fail "could not create the reuse chain columns"
reuse_json="$("$BOARD_BIN" card new --title 'OpenCode Reuse' -d 'traverse the opencode chain' \
  --harness opencode --model oc32/reuse-model --effort off \
  --space-kind workspace --space-ref "$REUSE_WS" --json)"
REUSE_ID="$(printf '%s' "$reuse_json" | jget id)" || fail "could not parse reuse card id"
mut "board move $REUSE_ID 'OpenCode Setup' -> opencode mint; non-fresh hop reuses pane"
e2e_board_herdr_mutate -- move "$REUSE_ID" "$SETUP_ID" --json >/dev/null
chain_outcome="$(wait_runs "$REUSE_ID" 2 120)" || {
  opencode_failure_diag reuse "$REUSE_ID"
  fail "opencode chain did not produce 2 runs (last='$chain_outcome')"
}
[ "$chain_outcome" = ok ] || fail "opencode chain last outcome '$chain_outcome' (expected ok)"
"$BOARD_BIN" card show "$REUSE_ID" --json >"$E2E_TMP/opencode-reuse-show.json"
REUSE_RUN1="$(card_field "$REUSE_ID" runs[0].id)"
REUSE_RUN2="$(card_field "$REUSE_ID" runs[-1].id)"
REUSE_SESSION="ses-$REUSE_ID-$REUSE_RUN1"
python3 - "$E2E_TMP/opencode-reuse-show.json" "$(hrpc tab.list "{\"workspace_id\":\"$REUSE_WS\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$REUSE_WS\"}")" "$REUSE_ID" "$REUSE_SESSION" \
  <<'PY' || fail "reuse chain assertions failed"
import json, sys
show_path, tabs_json, panes_json, card, session = sys.argv[1:]
show = json.load(open(show_path, encoding="utf-8"))
runs = show["runs"]
assert len(runs) == 2, f"expected 2 runs, got {len(runs)}"
assert all(r.get("outcome") == "ok" for r in runs)
# ONE conversation: the minted session survives the resume hop, and ONE pane
# serves both runs (no second agent.start).
assert runs[0]["session_id"] == session, f"run 1 session {runs[0].get('session_id')} != {session}"
assert runs[1]["session_id"] == session, f"run 2 session {runs[1].get('session_id')} != {session}"
assert runs[0]["herdr_pane_id"] == runs[1]["herdr_pane_id"], "the resume hop must reuse the mint pane"
# The retry-style fork contract stays on the enqueue argv, while the reuse hop
# keeps the resume spelling (its enqueue-time argv is persisted on the run).
argv0 = json.loads(runs[0]["argv_json"])
argv1 = json.loads(runs[1]["argv_json"])
assert argv0[0] == "opencode" and "--agent" in argv0 \
    and argv0[argv0.index("--agent") + 1] == "herdr-board", \
    "board effort must select the herdr-board agent (--variant never rides argv)"
assert "--variant" not in argv0 and "-m" not in argv0
assert argv1[-2:] == ["-s", session], f"reuse hop argv tail {argv1[-2:]} != ['-s', {session}]"
panes = json.loads(panes_json).get("panes", [])
tabs = json.loads(tabs_json).get("tabs", [])
match = [t for t in tabs if t.get("label") == f"card-{card}"]
assert len(match) == 1
owned = [p for p in panes if p.get("tab_id") == match[0]["tab_id"]]
assert len(owned) == 1 and owned[0].get("agent") == "opencode", f"tab holds {len(owned)} panes"
assert owned[0]["pane_id"] == runs[1]["herdr_pane_id"]
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
print("  Reuse: 2 runs share pane %s and session %s (off -> config variant none)" %
      (runs[1]["herdr_pane_id"], session), file=sys.stderr)
PY
REUSE_RECORD="$E2E_TMP/fake-opencode-run-$REUSE_RUN1.json"
python3 - "$REUSE_RECORD" "$E2E_TMP/opencode-reuse-show.json" "$REUSE_RUN1" <<'PY' \
  || fail "reuse fixture record assertions failed"
import json, sys
record, show_path, run1 = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
# The SAME process served both stages: minted at run 1, re-prompted for run 2.
assert x["mode"] == "mint" and x["effort"] == "none"
# Board effort off -> config variant none, carried through the agent config.
assert x["agent"] == "herdr-board" and x["config_env_ok"] is True
assert x["config_env_model"] == "oc32/reuse-model" and x["config_env_variant"] == "none"
assert x["prompt_matches_mint_block"] is True, "stage 1 must have delivered the delimited mint block"
assert x["prompt_matches_run_snapshot"] is True, "stage 2 must have delivered the task alone"
assert x["prompt"] == show["runs"][-1]["prompt_snapshot"], "the last delivered prompt is stage 2's task"
print("  Reuse fixture: one process, mint block then task-only re-prompt, both verified", file=sys.stderr)
PY
ok "non-fresh hop reused one pane/conversation; the chain landed on the manual column"

# --- Phase 4: rescue reopens a dead pane with `-s <id>`, no re-prompt --------
step "Rescue: a dead OpenCode pane is reopened with opencode -s <session>"
rescue_json="$("$BOARD_BIN" card new --title 'OpenCode Rescue' --description 'work to continue' \
  --harness opencode --model oc32/rescue-model --effort medium \
  --space-kind workspace --space-ref "$WS_ID" --json)"
RCUE_ID="$(printf '%s' "$rescue_json" | jget id)" || fail "could not parse rescue card id"
mut "board move $RCUE_ID 'OpenCode Execute' -> managed opencode mint (rescue target)"
e2e_board_herdr_mutate -- move "$RCUE_ID" "$EXEC_ID" --json >/dev/null
rcue_outcome="$(wait_ok "$RCUE_ID" 100)" || {
  opencode_failure_diag rescue "$RCUE_ID"
  fail "opencode rescue-target run did not finish (outcome '$rcue_outcome')"
}
[ "$rcue_outcome" = ok ] || fail "opencode rescue-target outcome '$rcue_outcome' (expected ok)"
RCUE_RUN="$(card_field "$RCUE_ID" runs[-1].id)"
RCUE_PANE="$(card_field "$RCUE_ID" runs[-1].herdr_pane_id)"
RCUE_SESSION="$(card_field "$RCUE_ID" runs[-1].session_id)"
[ -n "$RCUE_RUN" ] && [ -n "$RCUE_PANE" ] || fail "rescue-target run did not record its identity"
[ "$RCUE_SESSION" = "ses-$RCUE_ID-$RCUE_RUN" ] || fail "rescue-target session mismatch"
RUNS_BEFORE="$E2E_TMP/opencode-runs-before.json"
"$BOARD_BIN" card show "$RCUE_ID" --json | python3 -c '
import json, sys
print(json.dumps(json.load(sys.stdin)["runs"], sort_keys=True))
' >"$RUNS_BEFORE"
ok "rescue-target run $RCUE_RUN recorded session $RCUE_SESSION (pane $RCUE_PANE)"

step "HERDR MUTATION: pane.close $RCUE_PANE (the run's terminal is closed)"
mut "pane.close $RCUE_PANE (disposable board-owned pane of a finished run)"
e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$RCUE_PANE\"}" >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
  hrpc pane.get "{\"pane_id\":\"$RCUE_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
! hrpc pane.get "{\"pane_id\":\"$RCUE_PANE\"}" >/dev/null 2>&1 \
  || fail "recorded pane $RCUE_PANE is still alive; the rescue case cannot be exercised"

focus_json="$(e2e_board_herdr_mutate -- card run focus "$RCUE_ID" "$RCUE_RUN" --json)"
printf '  %s\n' "$focus_json"
focus_action="$(printf '%s' "$focus_json" | jget action)"
[ "$focus_action" = rescued ] || fail "focus reported action '$focus_action' (expected 'rescued')"
RESCUED_PANE="$(printf '%s' "$focus_json" | jget pane_id)"
[ -n "$RESCUED_PANE" ] && [ "$RESCUED_PANE" != "$RCUE_PANE" ] \
  || fail "focus returned no NEW rescued pane id"
hrpc pane.get "{\"pane_id\":\"$RESCUED_PANE\"}" >/dev/null \
  || fail "rescued pane $RESCUED_PANE does not exist"
rescue_label="$(pane_field "$WS_ID" "$RESCUED_PANE" label)"
[ "$rescue_label" = "card-$RCUE_ID-r$RCUE_RUN-rescue" ] \
  || fail "rescued pane label '$rescue_label' is not 'card-$RCUE_ID-r$RCUE_RUN-rescue'"
RCUE_RESUME_RECORD="$E2E_TMP/fake-opencode-run-$RCUE_RUN-rescue.json"
for _ in $(seq 1 100); do
  [ -f "$RCUE_RESUME_RECORD" ] && break
  sleep .1
done
[ -f "$RCUE_RESUME_RECORD" ] || fail "the rescued pane's fake OpenCode never recorded its startup"
python3 - "$RCUE_RESUME_RECORD" "$RCUE_SESSION" "$RCUE_RUN" "$RCUE_ID" "$OPENCODE_PROTOCOL" \
  <<'PY' || fail "rescue record assertions failed"
import json, sys
path, session, run_id, card_id, protocol = sys.argv[1:]
record = json.load(open(path, encoding="utf-8"))
# Resume: `-s <id>` closes the argv with the recorded session.
assert record["mode"] == "resume" and record["resume_id"] == session
assert record["argv"][-2:] == ["-s", session]
assert record["session_id"] == session and record["fork_id"] is None
assert record["run_id"] == int(run_id) and record["card_id"] == int(card_id)
assert record["rescue"] is True
# The persisted execution environment is preserved, not rebuilt from config:
# the agent config env (model + medium) survives onto the rescued pane.
assert record["model"] == "oc32/rescue-model" and record["effort"] == "medium"
assert record["agent"] == "herdr-board" and record["config_env_ok"] is True
assert record["config_env_model"] == "oc32/rescue-model"
assert record["config_env_variant"] == "medium"
assert record["argv"][-2:] == ["-s", session]
# The card task must NOT appear in startup argv, and a rescue sends NO
# agent.prompt at all, so the shim's prompt evidence must be absent.
assert record["startup_argv_has_no_prompt"] is True
assert not any("work to continue" in arg for arg in record["argv"])
assert "prompt" not in record, "a rescue must never re-send the card task"
print("  Rescue startup:", json.dumps({k: record[k] for k in
    ("mode", "resume_id", "session_id", "model", "effort", "argv")}, ensure_ascii=False))
PY
ok "harness resumed session $RCUE_SESSION without re-sending the card task"

step "Focus again: must reuse the rescued pane, never create a second one"
PANES_AFTER_RESCUE="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("panes",[])))')"
again_json="$(e2e_board_herdr_mutate -- card run focus "$RCUE_ID" "$RCUE_RUN" --json)"
again_action="$(printf '%s' "$again_json" | jget action)"
[ "$again_action" = focused_rescued_pane ] \
  || fail "second focus reported '$again_action' (expected 'focused_rescued_pane')"
[ "$(printf '%s' "$again_json" | jget pane_id)" = "$RESCUED_PANE" ] \
  || fail "second focus did not reuse rescued pane $RESCUED_PANE"
[ "$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("panes",[])))')" = "$PANES_AFTER_RESCUE" ] \
  || fail "second focus created another pane"
ok "second focus reused $RESCUED_PANE and created no extra pane"

step "Assert the rescue wrote NOTHING to the database"
"$BOARD_BIN" card show "$RCUE_ID" --json | python3 -c '
import json, sys
print(json.dumps(json.load(sys.stdin)["runs"], sort_keys=True))
' >"$E2E_TMP/opencode-runs-after.json"
diff -u "$RUNS_BEFORE" "$E2E_TMP/opencode-runs-after.json" \
  || fail "a rescue mutated the runs table (it must be byte-for-byte immutable)"
run_rows="$(python3 -c '
import json, sys
print(len(json.load(open(sys.argv[1], encoding="utf-8"))))
' "$E2E_TMP/opencode-runs-after.json")"
[ "$run_rows" = 1 ] || fail "expected exactly 1 run row, found $run_rows"
ok "the historical run row is unchanged and no run row was added"

step "Assert the managed rescue converged the recreated tab to one OpenCode pane"
RESCUED_TAB="$(pane_field "$WS_ID" "$RESCUED_PANE" tab_id)"
[ -n "$RESCUED_TAB" ] || fail "rescued pane is not in any tab"
python3 - "$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$RESCUED_TAB" "$RCUE_ID" "$RESCUED_PANE" <<'PY' || fail "rescued tab did not converge"
import json, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
rescued_tab, card, pane = sys.argv[3:6]
match = [t for t in tabs if t.get("tab_id") == rescued_tab]
assert len(match) == 1 and match[0].get("label") == f"card-{card}"
owned = [p for p in panes if p.get("tab_id") == rescued_tab]
assert len(owned) == 1 and owned[0]["pane_id"] == pane and owned[0].get("agent") == "opencode"
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
PY
ok "the managed rescue closed its anchor; the recreated tab holds exactly one OpenCode pane"

# --- Phase 5: fail-closed missing session report -----------------------------
step "Fail-closed: no session report -> mint completes NULL, rescue is refused"
# The daemon's split-pane launch env cannot carry scenario exports, so the
# per-run sentinel model `no-session` (documented in e2e/fake-bin/opencode;
# it now rides the agent config model like every effort-bearing model)
# switches the fixture into idle-report-only mode for this card.
e2e_ws_create opencode-nosession; NOSESSION_WS="$E2E_WS"
NOSESSION_EXEC="$(col_create '{"name":"NoSession Execute","trigger":"auto"}')"
NOSESSION_IMPL="$(col_create "{\"name\":\"NoSession Impl\",\"trigger\":\"auto\",\"fresh_session\":false}")"
nosession_json="$("$BOARD_BIN" card new --title 'OpenCode No Session' --description 'degraded integration' \
  --harness opencode --model no-session --effort off --permission auto-approve \
  --space-kind workspace --space-ref "$NOSESSION_WS" --json)"
NS_ID="$(printf '%s' "$nosession_json" | jget id)" || fail "could not parse no-session card id"
mut "board move $NS_ID 'NoSession Execute' -> managed opencode mint WITHOUT session report"
e2e_board_herdr_mutate -- move "$NS_ID" "$NOSESSION_EXEC" --json >/dev/null
ns_outcome="$(wait_ok "$NS_ID" 100)" || {
  opencode_failure_diag nosession "$NS_ID"
  fail "no-session opencode mint did not complete (outcome '$ns_outcome')"
}
[ "$ns_outcome" = ok ] || fail "no-session opencode outcome '$ns_outcome' (expected ok: basic execution continues)"
NS_RUN="$(card_field "$NS_ID" runs[-1].id)"
NS_PANE="$(card_field "$NS_ID" runs[-1].herdr_pane_id)"
NS_SESSION="ses-$NS_ID-$NS_RUN"
[ -n "$NS_RUN" ] && [ -n "$NS_PANE" ] || fail "no-session run did not record its identity"
if card_field "$NS_ID" runs[-1].session_id >/dev/null 2>&1; then
  fail "no-session mint must persist NULL session_id, got '$(card_field "$NS_ID" runs[-1].session_id)'"
fi
NS_RECORD="$E2E_TMP/fake-opencode-run-$NS_RUN.json"
[ -f "$NS_RECORD" ] || fail "fake OpenCode did not record the no-session run $NS_RUN"
python3 - "$NS_RECORD" "$NS_SESSION" <<'PY' || fail "no-session record assertions failed"
import json, sys
record, session = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
assert x["no_session_report"] is True and x["agent_session_id"] is None
assert x["agent_session_path"] is None and x["session_start_source"] is None
reports = x["reports"]
assert [r["phase"] for r in reports] == ["idle_lifecycle"], "only the idle lifecycle may be reported"
assert all(r["ok"] for r in reports)
assert x["readiness_report"] == "ok"
# The fixture still mints an internal session (opencode mints its own), it just
# never reports it — so the board cannot know it.
assert x["mode"] == "mint" and x["session_id"] == session
# off -> config variant none, delivered through the herdr-board agent config;
# the no-session sentinel rides the config model exactly like every effort
# model, and the prompt still arrives (as the delimited mint block) even
# without a session report.
assert x["effort"] == "none" and x["permission_mode"] == "auto-approve"
assert x["agent"] == "herdr-board" and x["config_env_ok"] is True
assert x["config_env_model"] == "no-session" and x["config_env_variant"] == "none"
assert x["argv"] == ["--agent", "herdr-board", "--auto"]
assert x["prompt_matches_mint_block"] is True and x["prompt_received_via_stdin"] is True
print("  No-session mint: idle-only report, NULL persisted, prompt still delivered", file=sys.stderr)
PY
ok "no-session mint completed ok with session_id NULL (basic execution continues)"

step "Rescue of the no-session run must fail closed, non-destructively"
mut "pane.close $NS_PANE (disposable board-owned pane of the no-session run)"
e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$NS_PANE\"}" >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
  hrpc pane.get "{\"pane_id\":\"$NS_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
before_refuse="$(hrpc pane.list "{\"workspace_id\":\"$NOSESSION_WS\"}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("panes",[])))')"
set +e
refuse_out="$(e2e_board_herdr_mutate -- card run focus "$NS_ID" "$NS_RUN" --json 2>&1)"
refuse_code=$?
set -e
[ "$refuse_code" -ne 0 ] || fail "focus on a no-session opencode run unexpectedly succeeded"
printf '%s\n' "$refuse_out" | grep -q "no harness conversation id" \
  || fail "refusal does not name the missing conversation id: $refuse_out"
printf '%s\n' "$refuse_out" | grep -qi "retry the card" \
  || fail "refusal is not actionable: $refuse_out"
[ "$(hrpc pane.list "{\"workspace_id\":\"$NOSESSION_WS\"}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("panes",[])))')" = "$before_refuse" ] \
  || fail "a refused rescue created a pane (it must be non-destructive)"
ok "the no-session run's rescue is refused by name with an actionable diagnostic"

step "A non-fresh hop on a no-session card cannot reuse: it mints fresh instead"
mut "board move $NS_ID 'NoSession Impl' -> cannot reuse without a session id"
e2e_board_herdr_mutate -- move "$NS_ID" "$NOSESSION_IMPL" --json >/dev/null
ns2_outcome="$(wait_runs "$NS_ID" 2 100)" || {
  opencode_failure_diag nosession-reuse "$NS_ID"
  fail "no-session second run did not finish (outcome '$ns2_outcome')"
}
[ "$ns2_outcome" = ok ] || fail "no-session second run outcome '$ns2_outcome' (expected ok)"
NS_RUN2="$(card_field "$NS_ID" runs[-1].id)"
NS_PANE2="$(card_field "$NS_ID" runs[-1].herdr_pane_id)"
[ "$NS_RUN2" != "$NS_RUN" ] || fail "the no-session hop did not create a new run"
[ "$NS_PANE2" != "$NS_PANE" ] || fail "the no-session hop must mint a fresh pane, not reuse"
NS_RECORD2="$E2E_TMP/fake-opencode-run-$NS_RUN2.json"
[ -f "$NS_RECORD2" ] || fail "fake OpenCode did not record the no-session hop run $NS_RUN2"
python3 - "$NS_RECORD2" "$NS_ID" "$NS_RUN2" "$NS_SESSION" <<'PY' || fail "no-session hop record assertions failed"
import json, sys
record, card, run, prior_session = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
assert x["no_session_report"] is True and x["mode"] == "mint"
assert x["session_id"] != prior_session, "without a recorded id the hop cannot reuse; it must mint a NEW session"
assert x["run_id"] == int(run) and str(x["card_id"]) == card
PY
ok "the no-session hop minted a fresh conversation (no reuse possible without an id)"

ok "fixture boundary: no provider was called; live Herdr readiness, capture, prompts, fork, reuse, rescue, and fail-closed paths proven"
step "32-managed-opencode: MINT CAPTURE + FORK + REUSE + RESCUE + FAIL-CLOSED CONTRACTS PASSED"
