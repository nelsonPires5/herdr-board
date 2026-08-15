#!/usr/bin/env bash
# 36-managed-antigravity.sh — managed Antigravity CLI (agy) protocol-19/current
# launch contract, provider-free through e2e/fake-bin/agy.
#
# Contract exercised end to end against a real Herdr:
#   1. Mint: `agy [--model M] [--effort low|medium|high] [--sandbox |
#      --dangerously-skip-permissions]` — NO conversation flag (agy TUI mints
#      its own id), no prompt text in argv, no system-prompt file; the daemon
#      captures the integration-reported conversation id from
#      `agent.get.agent_session` ({agent: agy, kind: id, source:
#      herdr:antigravity_cli, value}) AFTER the prompt and persists it on
#      run+card; the card task arrives through `agent.prompt` as ONE delimited
#      system+task block and matches the run snapshots;
#   2. Retry: `board retry` re-attaches to the SAME conversation
#      (`--conversation <id>` argv tail — agy has no fork) in a FRESH pane and
#      re-sends the task alone;
#   3. Never-reuse: because resume and retry share byte-identical argv, the
#      daemon treats every `--conversation` hop as a fork and launches a fresh
#      pane even across a non-fresh auto hop (same conversation id, new pane);
#   4. Rescue: a run whose pane is closed is reopened with
#      `agy ... --conversation <id>` in a new pane WITHOUT re-sending the
#      task; the runs row stays byte-for-byte unchanged; a second focus reuses
#      the rescued pane;
#   5. Fallback: when the recorded conversation no longer exists, agy starts a
#      new one and the integration reports the NEW id; the daemon detects the
#      mismatch, persists the new id, and writes a visible `system` card
#      comment naming both the old and the new conversation;
#   6. Fail-closed missing report: a card whose model is the documented
#      sentinel `no-session` makes the fixture report only the idle lifecycle,
#      so the capture degrades, the mint still completes with session_id NULL,
#      a `system` warning explains the missing integration, rescue is refused
#      with an actionable diagnostic, and a non-fresh hop cannot reuse the
#      conversation (it mints fresh instead);
#   7. Permission modes are pinned: `current` carries no flag, `sandbox`
#      carries `--sandbox`, `always-proceed` carries
#      `--dangerously-skip-permissions`; a fixed-effort model
#      (claude-sonnet-4-6) never receives `--effort`;
#   8. Managed tabs stay anchorless: every launch converges the `card-<id>`
#      tab to exactly one agy harness pane.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Fake-managed setup creates roots, so cleanup must already be armed.
trap e2e_cleanup EXIT
e2e_enable_fake_pi
[ -x "$E2E_FAKE_PI_BIN_DIR/agy" ] || fail "fake agy missing/not executable at $E2E_FAKE_PI_BIN_DIR/agy"
# agy NEVER reuses a pane (every --conversation hop launches fresh by design),
# so no looping fixture and no server-side knob export is needed.
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

# The board protocol trailer (the column has no custom system_prompt, so this
# is the exact `system_prompt_snapshot` the daemon persisted).
AGY_PROTOCOL="## herdr-board protocol
You are running a herdr-board card (\$BOARD_CARD_ID is preset). When this stage's goal is met you MUST finish with exactly two commands: first \`board comment \"<your results, files touched, findings>\"\`, then \`board done --outcome ok\`. If the stage goal was NOT met — something failed or you got lost — use \`board done --outcome fail --summary \"<why>\"\` instead. Always comment before done. Never use \`board move\`/\`cancel\`/\`retry\` on your own card. Finishing or going idle WITHOUT \`board done\` leaves the card in \`awaiting\` for human review — a run is never auto-completed."

agy_failure_diag() {
  local phase="$1" card="$2" panes record
  printf '\n--- agy %s failure diagnostics (disposable session only) ---\n' "$phase" >&2
  e2e_card_failure_diag "$card"
  printf 'fixture_records=%s\n' "$(find "$E2E_TMP" -maxdepth 1 -type f -name 'fake-agy-run-*.json' -printf . 2>/dev/null | wc -c)" >&2
  for record in "$E2E_TMP"/fake-agy-run-*.json; do
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
  printf '%s\n' '--- end agy diagnostics ---' >&2
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
step "Mint: fresh agy launch captures its self-minted conversation id"
e2e_ws_create agy-mint; WS_ID="$E2E_WS"
EXEC_ID="$(col_create '{"name":"Agy Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title 'Antigravity Mint' --description $'agy mint task with spaces\nand a newline' \
  --harness antigravity --model gemini-3.7-flash --effort high --permission sandbox \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse mint card id"
mut "board move $CARD_ID 'Agy Execute' -> managed agent.start kind=agy (mint)"
e2e_board_herdr_mutate -- move "$CARD_ID" "$EXEC_ID" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || {
  agy_failure_diag mint "$CARD_ID"
  fail "managed agy mint outcome '$outcome' (readiness/capture/agent.prompt did not complete)"
}
[ "$outcome" = ok ] || fail "managed agy mint did not complete ok (got '$outcome')"
MINT_RUN="$(card_field "$CARD_ID" runs[-1].id)"
MINT_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
MINT_SESSION="conv-$CARD_ID-$MINT_RUN"
[ -n "$MINT_RUN" ] && [ -n "$MINT_PANE" ] || fail "mint run did not record its identity"
[ "$(card_field "$CARD_ID" runs[-1].session_id)" = "$MINT_SESSION" ] \
  || fail "mint did not capture/persist the reported conversation id (expected $MINT_SESSION)"
MINT_RECORD="$E2E_TMP/fake-agy-run-$MINT_RUN.json"
[ -f "$MINT_RECORD" ] || fail "fake agy did not record run $MINT_RUN"
ok "run $MINT_RUN captured conversation $MINT_SESSION (pane $MINT_PANE)"

step "Assert the mint argv, the session capture, and the delimited prompt"
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/agy-mint-show.json"
python3 - "$MINT_RECORD" "$E2E_TMP/agy-mint-show.json" "$CARD_ID" "$MINT_RUN" "$MINT_SESSION" \
  "$BOARD_SOCKET" "$HERDR_SOCKET_PATH" "$AGY_PROTOCOL" <<'PY' || fail "mint record assertions failed"
import json, sys
record, show_path, card, run, session, board, herdr, protocol = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
expected_prompt = show["runs"][-1]["prompt_snapshot"]
assert str(x["card_id"]) == card and x["run_id"] == int(run)
assert x["board_socket"] == board and x["herdr_socket"] == herdr
assert x["mode"] == "mint" and x["resume_id"] is None
assert x["model"] == "gemini-3.7-flash" and x["effort"] == "high" and x["permission_mode"] == "sandbox"
assert x["system_prompt_file"] is None and x["startup_argv_has_no_prompt"] is True
# The pinned agy TUI argv: model + effort + permission flag; Mint carries NO
# conversation flag (agy mints its own id, the board never invents one).
expected_argv = ["--model", "gemini-3.7-flash", "--effort", "high", "--sandbox"]
assert x["argv"] == expected_argv, f"mint argv {x['argv']} != {expected_argv}"
assert not any("agy mint task" in arg for arg in x["argv"])
assert x["new_conversation_fallback"] is False
# The reported conversation id is exactly what the daemon captured and
# persisted, reported through the pinned herdr:antigravity_cli integration.
assert x["session_id"] == session and x["agent_session_id"] == session
reports = x["reports"]
assert [r["phase"] for r in reports] == ["session_identity", "idle_lifecycle"]
assert all(r["ok"] and r["reply"]["result"]["type"] == "ok" for r in reports)
identity, idle = (r["request"] for r in reports)
assert identity["method"] == "pane.report_agent_session"
assert idle["method"] == "pane.report_agent" and idle["params"]["state"] == "idle"
assert identity["params"]["source"] == idle["params"]["source"] == "herdr:antigravity_cli"
assert identity["params"]["agent"] == idle["params"]["agent"] == "agy"
assert identity["params"]["agent_session_id"] == session
assert identity["params"]["session_start_source"] == "startup"
assert x["readiness_report"] == "ok" and x["herdr_pane_id"]
# The Mint prompt is ONE delimited block: system instructions, then the task.
block = "## herdr-board system instructions\n" + protocol + "\n\n## herdr-board card task\n" + expected_prompt
assert x["stdin_isatty"] is True and x["prompt_received_via_stdin"] is True
assert x["prompt_matches_mint_block"] is True
assert x["mint_system_half"] == protocol, "the delivered system half is not the exact protocol trailer"
assert x["prompt"] == block, "the delivered mint prompt is not the exact delimited system+task block"
print("  Mint: no conversation flag/prompt in argv; conversation id captured; exact delimited block delivered", file=sys.stderr)
PY

step "Assert the managed mint tab is anchorless with exactly one agy pane"
python3 - "$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")" "$CARD_ID" "$MINT_PANE" \
  <<'PY' || fail "mint tab did not converge to one agy pane"
import json, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
card, pane = sys.argv[3:5]
match = [t for t in tabs if t.get("label") == f"card-{card}"]
assert len(match) == 1, f"expected one card-{card} tab, got {len(match)}"
owned = [p for p in panes if p.get("tab_id") == match[0]["tab_id"]]
assert len(owned) == 1, f"expected exactly one pane in the mint tab, got {len(owned)}"
assert owned[0]["pane_id"] == pane and owned[0].get("agent") == "agy"
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
PY
ok "mint tab holds exactly the one agy pane, no anchor"

# --- Phase 2: retry re-attaches to the SAME conversation in a FRESH pane ----
step "Retry: board retry resumes the SAME conversation in a NEW agy pane"
# A finished run's terminal is routinely closed by the user. Closing it also
# removes any reuse candidate, so the retry exercises the `--conversation`
# launch itself in a fresh pane rather than a same-pane re-prompt.
mut "pane.close $MINT_PANE (disposable board-owned pane of the finished mint run)"
e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$MINT_PANE\"}" >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
  hrpc pane.get "{\"pane_id\":\"$MINT_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
! hrpc pane.get "{\"pane_id\":\"$MINT_PANE\"}" >/dev/null 2>&1 \
  || fail "mint pane $MINT_PANE is still alive; the retry launch case cannot be exercised"
mut "board retry $CARD_ID -> agy --conversation <recorded id> in a fresh pane"
e2e_board_herdr_mutate -- retry "$CARD_ID" >/dev/null
retry_outcome="$(wait_runs "$CARD_ID" 2 100)" || {
  agy_failure_diag retry "$CARD_ID"
  fail "agy retry run did not finish (outcome '$retry_outcome')"
}
[ "$retry_outcome" = ok ] || fail "agy retry outcome '$retry_outcome' (expected ok)"
RETRY_RUN="$(card_field "$CARD_ID" runs[-1].id)"
RETRY_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
[ -n "$RETRY_RUN" ] && [ -n "$RETRY_PANE" ] || fail "retry run did not record its identity"
# agy has NO fork: the retry re-attaches to the recorded conversation and the
# captured id equals the mint's id — the run keeps the SAME conversation.
[ "$(card_field "$CARD_ID" runs[-1].session_id)" = "$MINT_SESSION" ] \
  || fail "retry changed the conversation id (expected it to keep $MINT_SESSION)"
# ...but because resume/retry argv are byte-identical, every --conversation hop
# launches a FRESH pane by design (agy panes are never reused).
[ "$RETRY_PANE" != "$MINT_PANE" ] \
  || fail "retry reused the mint pane $MINT_PANE (agy must always launch a fresh pane)"
RETRY_RECORD="$E2E_TMP/fake-agy-run-$RETRY_RUN.json"
[ -f "$RETRY_RECORD" ] || fail "fake agy did not record retry run $RETRY_RUN"
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/agy-retry-show.json"
python3 - "$RETRY_RECORD" "$E2E_TMP/agy-retry-show.json" "$CARD_ID" "$RETRY_RUN" \
  "$MINT_SESSION" <<'PY' || fail "retry record assertions failed"
import json, sys
record, show_path, card, run, session = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
assert str(x["card_id"]) == card and x["run_id"] == int(run)
assert x["mode"] == "resume" and x["resume_id"] == session
# The retry argv closes with `--conversation <id>` after the same settings.
assert x["argv"] == ["--model", "gemini-3.7-flash", "--effort", "high",
                     "--sandbox", "--conversation", session], \
    f"retry argv {x['argv']} != pinned --conversation argv"
assert x["session_id"] == session and x["agent_session_id"] == session
assert x["new_conversation_fallback"] is False
assert x["system_prompt_file"] is None and x["startup_argv_has_no_prompt"] is True
# Resume/retry receive the task ALONE — never the system block.
assert x["prompt_matches_run_snapshot"] is True
assert x["prompt"] == show["runs"][-1]["prompt_snapshot"]
assert "## herdr-board system instructions" not in x["prompt"]
assert "## herdr-board card task" not in x["prompt"]
print("  Retry: '--conversation <id>' closes the argv; same conversation, fresh pane, task-only prompt", file=sys.stderr)
PY
ok "retry run $RETRY_RUN kept conversation $MINT_SESSION in a fresh pane ($RETRY_PANE)"

step "Assert the retry recreated the card tab and converged to one agy pane"
python3 - "$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")" "$CARD_ID" "$RETRY_PANE" \
  <<'PY' || fail "retry tab did not converge to one agy pane"
import json, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
card, pane = sys.argv[3:5]
match = [t for t in tabs if t.get("label") == f"card-{card}"]
assert len(match) == 1
owned = [p for p in panes if p.get("tab_id") == match[0]["tab_id"]]
assert len(owned) == 1, f"expected exactly one pane in the retry tab, got {len(owned)}"
assert owned[0]["pane_id"] == pane and owned[0].get("agent") == "agy"
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
PY
ok "the retry's fresh launch recreated the card tab with exactly one agy pane"

# --- Phase 3: never-reuse across a non-fresh auto hop ------------------------
step "Never-reuse: a non-fresh auto hop ALSO launches a fresh agy pane"
# opencode reuses the same pane on a non-fresh hop; agy cannot: resume and
# retry carry byte-identical `--conversation` argv, so the daemon's fork
# detector treats every conversation hop as a fresh launch. The conversation
# id is still preserved across the hop.
e2e_ws_create agy-chain; CHAIN_WS="$E2E_WS"
MANUAL_ID="$(col_create '{"name":"Agy Manual","trigger":"manual"}')"
IMPL_ID="$(col_create "{\"name\":\"Agy Impl\",\"trigger\":\"auto\",\"on_success_column_id\":$MANUAL_ID,\"fresh_session\":false}")"
SETUP_ID="$(col_create "{\"name\":\"Agy Setup\",\"trigger\":\"auto\",\"on_success_column_id\":$IMPL_ID,\"fresh_session\":true}")"
[ -n "$SETUP_ID" ] && [ -n "$IMPL_ID" ] && [ -n "$MANUAL_ID" ] \
  || fail "could not create the chain columns"
chain_json="$("$BOARD_BIN" card new --title 'Antigravity Chain' -d 'traverse the agy chain' \
  --harness antigravity --model gemini-3.7-flash --effort medium \
  --permission current \
  --space-kind workspace --space-ref "$CHAIN_WS" --json)"
CHAIN_ID="$(printf '%s' "$chain_json" | jget id)" || fail "could not parse chain card id"
mut "board move $CHAIN_ID 'Agy Setup' -> agy mint; non-fresh hop must still launch a fresh pane"
e2e_board_herdr_mutate -- move "$CHAIN_ID" "$SETUP_ID" --json >/dev/null
chain_outcome="$(wait_runs "$CHAIN_ID" 2 120)" || {
  agy_failure_diag chain "$CHAIN_ID"
  fail "agy chain did not produce 2 runs (last='$chain_outcome')"
}
[ "$chain_outcome" = ok ] || fail "agy chain last outcome '$chain_outcome' (expected ok)"
"$BOARD_BIN" card show "$CHAIN_ID" --json >"$E2E_TMP/agy-chain-show.json"
CHAIN_RUN1="$(card_field "$CHAIN_ID" runs[0].id)"
CHAIN_RUN2="$(card_field "$CHAIN_ID" runs[-1].id)"
CHAIN_SESSION="conv-$CHAIN_ID-$CHAIN_RUN1"
python3 - "$E2E_TMP/agy-chain-show.json" "$(hrpc tab.list "{\"workspace_id\":\"$CHAIN_WS\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$CHAIN_WS\"}")" "$CHAIN_ID" "$CHAIN_SESSION" \
  <<'PY' || fail "chain assertions failed"
import json, sys
show_path, tabs_json, panes_json, card, session = sys.argv[1:]
show = json.load(open(show_path, encoding="utf-8"))
runs = show["runs"]
assert len(runs) == 2, f"expected 2 runs, got {len(runs)}"
assert all(r.get("outcome") == "ok" for r in runs)
# ONE conversation survives the hop — but the panes MUST differ: agy never
# reuses a pane (resume and retry argv are byte-identical, so the fork
# detector always fires and every hop is a fresh launch).
assert runs[0]["session_id"] == session, f"run 1 session {runs[0].get('session_id')} != {session}"
assert runs[1]["session_id"] == session, f"run 2 session {runs[1].get('session_id')} != {session}"
assert runs[0]["herdr_pane_id"] != runs[1]["herdr_pane_id"], \
    "the non-fresh hop must NOT reuse the mint pane (agy never reuses)"
# The hop argv closes with `--conversation <id>` exactly like a retry.
argv0 = json.loads(runs[0]["argv_json"])
argv1 = json.loads(runs[1]["argv_json"])
assert argv0 == ["agy", "--model", "gemini-3.7-flash", "--effort", "medium"], \
    f"mint argv {argv0} != pinned medium/current argv (current must carry no flag)"
assert argv1 == ["agy", "--model", "gemini-3.7-flash", "--effort", "medium",
                 "--conversation", session], f"hop argv {argv1} != --conversation argv"
panes = json.loads(panes_json).get("panes", [])
tabs = json.loads(tabs_json).get("tabs", [])
match = [t for t in tabs if t.get("label") == f"card-{card}"]
assert len(match) == 1
owned = [p for p in panes if p.get("tab_id") == match[0]["tab_id"]]
assert len(owned) == 1 and owned[0].get("agent") == "agy", f"tab holds {len(owned)} panes"
assert owned[0]["pane_id"] == runs[1]["herdr_pane_id"]
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
print("  Chain: 2 runs share conversation %s but use DIFFERENT panes (%s -> %s)" %
      (session, runs[0]["herdr_pane_id"], runs[1]["herdr_pane_id"]), file=sys.stderr)
PY
CHAIN_RECORD1="$E2E_TMP/fake-agy-run-$CHAIN_RUN1.json"
CHAIN_RECORD2="$E2E_TMP/fake-agy-run-$CHAIN_RUN2.json"
[ -f "$CHAIN_RECORD1" ] && [ -f "$CHAIN_RECORD2" ] \
  || fail "fake agy did not record both chain runs"
python3 - "$CHAIN_RECORD1" "$CHAIN_RECORD2" "$E2E_TMP/agy-chain-show.json" <<'PY' \
  || fail "chain fixture record assertions failed"
import json, sys
record1, record2, show_path = sys.argv[1:]
r1 = json.load(open(record1, encoding="utf-8"))
r2 = json.load(open(record2, encoding="utf-8"))
show = json.load(open(show_path, encoding="utf-8"))
# Stage 1 minted and delivered the delimited block; stage 2 resumed the same
# conversation and delivered the task alone — two SEPARATE processes.
assert r1["mode"] == "mint" and r1["permission_mode"] == "current"
assert r1["prompt_matches_mint_block"] is True, "stage 1 must have delivered the delimited mint block"
assert r2["mode"] == "resume" and r2["resume_id"] == r1["session_id"]
assert r2["prompt_matches_run_snapshot"] is True, "stage 2 must have delivered the task alone"
assert r2["prompt"] == show["runs"][-1]["prompt_snapshot"], "the last delivered prompt is stage 2's task"
PY
ok "non-fresh hop kept the conversation but launched a fresh pane (never-reuse)"

# --- Phase 4: rescue reopens a dead pane with `--conversation`, no re-prompt --
step "Rescue: a dead agy pane is reopened with agy --conversation <id>"
# Fixed-effort model (no `--effort` ever) + the third permission mode
# (always-proceed -> --dangerously-skip-permissions), pinned on one card.
rescue_json="$("$BOARD_BIN" card new --title 'Antigravity Rescue' --description 'work to continue' \
  --harness antigravity --model claude-sonnet-4-6 \
  --permission always-proceed \
  --space-kind workspace --space-ref "$WS_ID" --json)"
RCUE_ID="$(printf '%s' "$rescue_json" | jget id)" || fail "could not parse rescue card id"
mut "board move $RCUE_ID 'Agy Execute' -> managed agy mint (rescue target)"
e2e_board_herdr_mutate -- move "$RCUE_ID" "$EXEC_ID" --json >/dev/null
rcue_outcome="$(wait_ok "$RCUE_ID" 100)" || {
  agy_failure_diag rescue "$RCUE_ID"
  fail "agy rescue-target run did not finish (outcome '$rcue_outcome')"
}
[ "$rcue_outcome" = ok ] || fail "agy rescue-target outcome '$rcue_outcome' (expected ok)"
RCUE_RUN="$(card_field "$RCUE_ID" runs[-1].id)"
RCUE_PANE="$(card_field "$RCUE_ID" runs[-1].herdr_pane_id)"
RCUE_SESSION="$(card_field "$RCUE_ID" runs[-1].session_id)"
[ -n "$RCUE_RUN" ] && [ -n "$RCUE_PANE" ] || fail "rescue-target run did not record its identity"
[ "$RCUE_SESSION" = "conv-$RCUE_ID-$RCUE_RUN" ] || fail "rescue-target conversation mismatch"
RCUE_RECORD="$E2E_TMP/fake-agy-run-$RCUE_RUN.json"
[ -f "$RCUE_RECORD" ] || fail "fake agy did not record the rescue-target run"
python3 - "$RCUE_RECORD" <<'PY' || fail "fixed-effort/permission record assertions failed"
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
# Fixed-effort model: `--effort` must NEVER ride the argv (the fixture would
# reject it; this assert pins the daemon side). always-proceed -> the pinned
# --dangerously-skip-permissions flag.
assert x["argv"] == ["--model", "claude-sonnet-4-6",
                     "--dangerously-skip-permissions"], f"argv {x['argv']} != pinned"
assert x["effort"] is None and x["permission_mode"] == "always-proceed"
PY
RUNS_BEFORE="$E2E_TMP/agy-runs-before.json"
"$BOARD_BIN" card show "$RCUE_ID" --json | python3 -c '
import json, sys
print(json.dumps(json.load(sys.stdin)["runs"], sort_keys=True))
' >"$RUNS_BEFORE"
ok "rescue-target run $RCUE_RUN recorded conversation $RCUE_SESSION (pane $RCUE_PANE)"

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
RCUE_RESUME_RECORD="$E2E_TMP/fake-agy-run-$RCUE_RUN-rescue.json"
for _ in $(seq 1 100); do
  [ -f "$RCUE_RESUME_RECORD" ] && break
  sleep .1
done
[ -f "$RCUE_RESUME_RECORD" ] || fail "the rescued pane's fake agy never recorded its startup"
python3 - "$RCUE_RESUME_RECORD" "$RCUE_SESSION" "$RCUE_RUN" "$RCUE_ID" \
  <<'PY' || fail "rescue record assertions failed"
import json, sys
path, session, run_id, card_id = sys.argv[1:]
record = json.load(open(path, encoding="utf-8"))
# Resume: `--conversation <id>` closes the argv with the recorded conversation.
assert record["mode"] == "resume" and record["resume_id"] == session
assert record["argv"] == ["--model", "claude-sonnet-4-6",
                          "--dangerously-skip-permissions", "--conversation", session]
assert record["session_id"] == session and record["agent_session_id"] == session
assert record["run_id"] == int(run_id) and record["card_id"] == int(card_id)
assert record["rescue"] is True
assert record["new_conversation_fallback"] is False
# The persisted execution environment is preserved, not rebuilt from config.
assert record["model"] == "claude-sonnet-4-6" and record["permission_mode"] == "always-proceed"
assert record["effort"] is None, "a fixed-effort model must never receive --effort on rescue"
# The card task must NOT appear in startup argv, and a rescue sends NO
# agent.prompt at all, so the shim's prompt evidence must be absent.
assert record["startup_argv_has_no_prompt"] is True
assert not any("work to continue" in arg for arg in record["argv"])
assert "prompt" not in record, "a rescue must never re-send the card task"
print("  Rescue startup:", json.dumps({k: record[k] for k in
    ("mode", "resume_id", "session_id", "model", "argv")}, ensure_ascii=False))
PY
ok "harness resumed conversation $RCUE_SESSION without re-sending the card task"

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
' >"$E2E_TMP/agy-runs-after.json"
diff -u "$RUNS_BEFORE" "$E2E_TMP/agy-runs-after.json" \
  || fail "a rescue mutated the runs table (it must be byte-for-byte immutable)"
run_rows="$(python3 -c '
import json, sys
print(len(json.load(open(sys.argv[1], encoding="utf-8"))))
' "$E2E_TMP/agy-runs-after.json")"
[ "$run_rows" = 1 ] || fail "expected exactly 1 run row, found $run_rows"
ok "the historical run row is unchanged and no run row was added"

step "Assert the managed rescue converged the recreated tab to one agy pane"
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
assert len(owned) == 1 and owned[0]["pane_id"] == pane and owned[0].get("agent") == "agy"
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
PY
ok "the managed rescue closed its anchor; the recreated tab holds exactly one agy pane"

# --- Phase 5: fallback when the recorded conversation no longer exists -------
step "Fallback: agy starts a NEW conversation; the daemon persists it + warns"
# The per-run sentinel model `new-conversation` (documented in
# e2e/fake-bin/agy) switches the fixture into fallback mode for this card: on
# a `--conversation <old>` launch it reports a brand-new id, exactly like the
# real agy "conversation not found" fallback. The daemon compares the captured
# id against the requested one, persists the new id, and writes a system
# warning naming both.
e2e_ws_create agy-fallback; FB_WS="$E2E_WS"
FB_EXEC="$(col_create '{"name":"Fallback Execute","trigger":"auto"}')"
fb_json="$("$BOARD_BIN" card new --title 'Antigravity Fallback' --description 'recorded conversation disappears' \
  --harness antigravity --model new-conversation --permission current \
  --space-kind workspace --space-ref "$FB_WS" --json)"
FB_ID="$(printf '%s' "$fb_json" | jget id)" || fail "could not parse fallback card id"
mut "board move $FB_ID 'Fallback Execute' -> managed agy mint (fallback target)"
e2e_board_herdr_mutate -- move "$FB_ID" "$FB_EXEC" --json >/dev/null
fb_outcome="$(wait_ok "$FB_ID" 100)" || {
  agy_failure_diag fallback "$FB_ID"
  fail "agy fallback mint did not complete (outcome '$fb_outcome')"
}
[ "$fb_outcome" = ok ] || fail "agy fallback mint outcome '$fb_outcome' (expected ok)"
FB_RUN1="$(card_field "$FB_ID" runs[-1].id)"
FB_SESSION1="conv-$FB_ID-$FB_RUN1"
[ "$(card_field "$FB_ID" runs[-1].session_id)" = "$FB_SESSION1" ] \
  || fail "fallback mint did not record its conversation id"
ok "fallback card minted conversation $FB_SESSION1 (run $FB_RUN1)"

mut "board retry $FB_ID -> agy resumes conv-$FB_ID-$FB_RUN1; agy starts a NEW conversation instead"
e2e_board_herdr_mutate -- retry "$FB_ID" >/dev/null
fb2_outcome="$(wait_runs "$FB_ID" 2 100)" || {
  agy_failure_diag fallback-retry "$FB_ID"
  fail "agy fallback retry did not finish (outcome '$fb2_outcome')"
}
[ "$fb2_outcome" = ok ] || fail "agy fallback retry outcome '$fb2_outcome' (expected ok: the new conversation runs)"
FB_RUN2="$(card_field "$FB_ID" runs[-1].id)"
FB_SESSION2="conv-$FB_ID-$FB_RUN2"
[ -n "$FB_RUN2" ] || fail "fallback retry did not record its run"
[ "$FB_SESSION2" != "$FB_SESSION1" ] \
  || fail "fallback retry did not mint a new conversation id"
# The promotion already persisted the NEW id on the run (and with it the card).
[ "$(card_field "$FB_ID" runs[-1].session_id)" = "$FB_SESSION2" ] \
  || fail "the new conversation id $FB_SESSION2 was not persisted on the run"
FB_RECORD2="$E2E_TMP/fake-agy-run-$FB_RUN2.json"
[ -f "$FB_RECORD2" ] || fail "fake agy did not record the fallback run $FB_RUN2"
python3 - "$FB_RECORD2" "$FB_SESSION1" "$FB_SESSION2" <<'PY' || fail "fallback record assertions failed"
import json, sys
record, requested, reported = sys.argv[1:]
x = json.load(open(record, encoding="utf-8"))
# The fallback launch argv still ASKED for the recorded conversation...
assert x["mode"] == "resume" and x["resume_id"] == requested
assert x["argv"] == ["--model", "new-conversation", "--conversation", requested]
# ...but the fixture reported a NEW id, which is what the daemon captured.
assert x["new_conversation_fallback"] is True
assert x["session_id"] == reported and x["agent_session_id"] == reported
assert x["prompt_matches_run_snapshot"] is True
print("  Fallback: argv asked for %s; agy reported %s" % (requested, reported), file=sys.stderr)
PY
ok "fallback persisted new conversation $FB_SESSION2 on run $FB_RUN2"

step "Assert the fallback warning is visible in the card history"
"$BOARD_BIN" card show "$FB_ID" --json >"$E2E_TMP/agy-fallback-show.json"
python3 - "$E2E_TMP/agy-fallback-show.json" "$FB_SESSION1" "$FB_SESSION2" \
  <<'PY' || fail "fallback system warning was not recorded on the card"
import json, sys
show, old, new = sys.argv[1:]
card = json.load(open(show, encoding="utf-8"))
comments = card.get("comments", [])
system = [c for c in comments if c.get("author") == "system"]
assert system, "no system comments on the fallback card"
warnings = [c["body"] for c in system if "no longer exists" in c["body"]]
assert warnings, f"no fallback warning among system comments: {[c['body'] for c in system]}"
assert any(old in w and new in w for w in warnings), \
    f"the warning must name BOTH conversations: {warnings}"
print("  Warning:", warnings[0], file=sys.stderr)
PY
ok "card history names both the lost and the new conversation"

# --- Phase 6: fail-closed missing session report -----------------------------
step "Fail-closed: no session report -> mint completes NULL, rescue is refused"
# The daemon's split-pane launch env cannot carry scenario exports, so the
# per-run sentinel model `no-session` (documented in e2e/fake-bin/agy)
# switches the fixture into idle-report-only mode for this card.
e2e_ws_create agy-nosession; NOSESSION_WS="$E2E_WS"
NOSESSION_EXEC="$(col_create '{"name":"NoSession Execute","trigger":"auto"}')"
NOSESSION_IMPL="$(col_create "{\"name\":\"NoSession Impl\",\"trigger\":\"auto\",\"fresh_session\":false}")"
nosession_json="$("$BOARD_BIN" card new --title 'Antigravity No Session' --description 'degraded integration' \
  --harness antigravity --model no-session --permission sandbox \
  --space-kind workspace --space-ref "$NOSESSION_WS" --json)"
NS_ID="$(printf '%s' "$nosession_json" | jget id)" || fail "could not parse no-session card id"
mut "board move $NS_ID 'NoSession Execute' -> managed agy mint WITHOUT session report"
e2e_board_herdr_mutate -- move "$NS_ID" "$NOSESSION_EXEC" --json >/dev/null
ns_outcome="$(wait_ok "$NS_ID" 100)" || {
  agy_failure_diag nosession "$NS_ID"
  fail "no-session agy mint did not complete (outcome '$ns_outcome')"
}
[ "$ns_outcome" = ok ] || fail "no-session agy outcome '$ns_outcome' (expected ok: basic execution continues)"
NS_RUN="$(card_field "$NS_ID" runs[-1].id)"
NS_PANE="$(card_field "$NS_ID" runs[-1].herdr_pane_id)"
[ -n "$NS_RUN" ] && [ -n "$NS_PANE" ] || fail "no-session run did not record its identity"
if card_field "$NS_ID" runs[-1].session_id >/dev/null 2>&1; then
  fail "no-session mint must persist NULL session_id, got '$(card_field "$NS_ID" runs[-1].session_id)'"
fi
NS_RECORD="$E2E_TMP/fake-agy-run-$NS_RUN.json"
[ -f "$NS_RECORD" ] || fail "fake agy did not record the no-session run $NS_RUN"
python3 - "$NS_RECORD" <<'PY' || fail "no-session record assertions failed"
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
assert x["no_session_report"] is True and x["agent_session_id"] is None
assert x["agent_session_path"] is None and x["session_start_source"] is None
reports = x["reports"]
assert [r["phase"] for r in reports] == ["idle_lifecycle"], "only the idle lifecycle may be reported"
assert all(r["ok"] for r in reports)
assert x["readiness_report"] == "ok"
# The fixture still mints an internal session id, it just never reports it —
# so the board cannot know it.
assert x["mode"] == "mint"
assert x["permission_mode"] == "sandbox"
assert x["argv"] == ["--model", "no-session", "--sandbox"]
assert x["prompt_matches_mint_block"] is True and x["prompt_received_via_stdin"] is True
print("  No-session mint: idle-only report, NULL persisted, prompt still delivered", file=sys.stderr)
PY
ok "no-session mint completed ok with session_id NULL (basic execution continues)"

step "Assert the missing-integration warning is visible in the card history"
"$BOARD_BIN" card show "$NS_ID" --json >"$E2E_TMP/agy-nosession-show.json"
python3 - "$E2E_TMP/agy-nosession-show.json" <<'PY' || fail "missing-integration warning was not recorded"
import json, sys
card = json.load(open(sys.argv[1], encoding="utf-8"))
comments = card.get("comments", [])
system = [c for c in comments if c.get("author") == "system"]
warnings = [c["body"] for c in system if "No Antigravity conversation id was captured" in c["body"]]
assert warnings, f"no missing-integration warning among system comments: {[c['body'] for c in system]}"
assert any("antigravity_cli integration" in w for w in warnings), warnings
print("  Warning:", warnings[0], file=sys.stderr)
PY
ok "card history warns that the herdr antigravity_cli integration reported nothing"

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
[ "$refuse_code" -ne 0 ] || fail "focus on a no-session agy run unexpectedly succeeded"
printf '%s\n' "$refuse_out" | grep -q "no harness conversation id" \
  || fail "refusal does not name the missing conversation id: $refuse_out"
printf '%s\n' "$refuse_out" | grep -qi "retry the card" \
  || fail "refusal is not actionable: $refuse_out"
[ "$(hrpc pane.list "{\"workspace_id\":\"$NOSESSION_WS\"}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("panes",[])))')" = "$before_refuse" ] \
  || fail "a refused rescue created a pane (it must be non-destructive)"
ok "the no-session run's rescue is refused by name with an actionable diagnostic"

step "A non-fresh hop on a no-session card cannot reuse: it mints fresh instead"
mut "board move $NS_ID 'NoSession Impl' -> cannot reuse without a conversation id"
e2e_board_herdr_mutate -- move "$NS_ID" "$NOSESSION_IMPL" --json >/dev/null
ns2_outcome="$(wait_runs "$NS_ID" 2 100)" || {
  agy_failure_diag nosession-reuse "$NS_ID"
  fail "no-session second run did not finish (outcome '$ns2_outcome')"
}
[ "$ns2_outcome" = ok ] || fail "no-session second run outcome '$ns2_outcome' (expected ok)"
NS_RUN2="$(card_field "$NS_ID" runs[-1].id)"
NS_PANE2="$(card_field "$NS_ID" runs[-1].herdr_pane_id)"
[ "$NS_RUN2" != "$NS_RUN" ] || fail "the no-session hop did not create a new run"
[ "$NS_PANE2" != "$NS_PANE" ] || fail "the no-session hop must mint a fresh pane, not reuse"
NS_RECORD2="$E2E_TMP/fake-agy-run-$NS_RUN2.json"
[ -f "$NS_RECORD2" ] || fail "fake agy did not record the no-session hop run $NS_RUN2"
python3 - "$NS_RECORD2" <<'PY' || fail "no-session hop record assertions failed"
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
assert x["no_session_report"] is True and x["mode"] == "mint"
assert x["argv"] == ["--model", "no-session", "--sandbox"], \
    "without a recorded id the hop cannot reuse; it must mint fresh"
assert x["prompt_matches_mint_block"] is True
PY
ok "the no-session hop minted a fresh conversation (no reuse possible without an id)"

ok "fixture boundary: no provider was called; live Herdr readiness, capture, prompts, retry, never-reuse, rescue, fallback, and fail-closed paths proven"
step "36-managed-antigravity: MINT CAPTURE + SAME-CONVERSATION RETRY + NEVER-REUSE + RESCUE + FALLBACK + FAIL-CLOSED CONTRACTS PASSED"
