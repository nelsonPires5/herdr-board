#!/usr/bin/env bash
# 27-rescue-dead-pane.sh — `run.focus` reopens a run whose pane is gone by
# resuming its harness conversation in a NEW, ephemeral pane.
#
# Contract exercised end to end against a real Herdr, provider-free:
#   1. a managed Pi run completes and its pane is then closed;
#   2. `board card run focus CARD RUN` reports `action=rescued`, creates a new
#      pane in the card's `card-<id>` tab, and starts the harness in RESUME mode
#      with the run's recorded conversation id — without re-sending the task;
#   3. pressing focus again reports `action=focused_rescued_pane` and creates NO
#      second pane (idempotency by pane name, the only correlator available
#      because a rescue is forbidden from writing to the database);
#   4. the `runs` row is byte-for-byte unchanged — no new run, no updated
#      `herdr_pane_id`, no reopened `ended_at`/`outcome`;
#   5. a run that cannot be reopened is refused explicitly and non-destructively.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Fake-managed setup creates roots, so cleanup must already be armed.
trap e2e_cleanup EXIT
e2e_enable_fake_pi
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

# Read one pane's field from the disposable workspace (read-only probe).
pane_field() {
  hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json, sys
pane_id, field = sys.argv[1:3]
for pane in json.load(sys.stdin).get("panes", []):
    if pane.get("pane_id") == pane_id:
        print(pane.get(field) or "")
        break
' "$1" "$2"
}

pane_count() {
  hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json, sys
print(len(json.load(sys.stdin).get("panes", [])))
'
}

step "Dispatch a managed Pi run into a disposable workspace"
e2e_ws_create rescue-target; WS_ID="$E2E_WS"
EXEC_ID="$(col_create '{"name":"Rescue Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title 'rescue me' --description 'work to continue' \
  --harness pi --model rescue/pi-model --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)"
mut "board move $CARD_ID 'Rescue Execute' -> managed agent.start kind=pi"
e2e_board_herdr_mutate -- move "$CARD_ID" "$EXEC_ID" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || fail "managed Pi run did not finish (outcome '$outcome')"
[ "$outcome" = ok ] || fail "managed Pi outcome '$outcome' (expected ok)"

RUN_ID="$(card_field "$CARD_ID" 'runs[-1].id')"
DEAD_PANE="$(card_field "$CARD_ID" 'runs[-1].herdr_pane_id')"
CONV_ID="$(card_field "$CARD_ID" 'runs[-1].session_id')"
[ -n "$RUN_ID" ] && [ -n "$DEAD_PANE" ] || fail "run did not record its identity"
[ -n "$CONV_ID" ] || fail "managed Pi run did not record a harness conversation id"
TAB_ID="$(pane_field "$DEAD_PANE" tab_id)"
[ -n "$TAB_ID" ] || fail "run pane is not in a tab"
ok "run $RUN_ID recorded pane $DEAD_PANE (tab $TAB_ID) and conversation $CONV_ID"

# Freeze the authoritative run row so the no-DB-writes constraint is checkable.
RUNS_BEFORE="$E2E_TMP/runs-before.json"
"$BOARD_BIN" card show "$CARD_ID" --json | python3 -c '
import json, sys
print(json.dumps(json.load(sys.stdin)["runs"], sort_keys=True))
' >"$RUNS_BEFORE"

step "HERDR MUTATION: pane.close $DEAD_PANE (the run's terminal is closed)"
mut "pane.close $DEAD_PANE (disposable board-owned pane of a finished run)"
e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$DEAD_PANE\"}" >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
  hrpc pane.get "{\"pane_id\":\"$DEAD_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
! hrpc pane.get "{\"pane_id\":\"$DEAD_PANE\"}" >/dev/null 2>&1 \
  || fail "recorded pane $DEAD_PANE is still alive; the rescue case cannot be exercised"
PANES_AFTER_CLOSE="$(pane_count)"
ok "recorded pane $DEAD_PANE is gone ($PANES_AFTER_CLOSE panes left in $WS_ID)"

step "Focus the run: the daemon must RESCUE it into a new pane"
rescue_json="$(e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$RUN_ID" --json)"
printf '  %s\n' "$rescue_json"
action="$(printf '%s' "$rescue_json" | jget action)"
[ "$action" = rescued ] || fail "focus reported action '$action' (expected 'rescued')"
RESCUED_PANE="$(printf '%s' "$rescue_json" | jget pane_id)"
recorded="$(printf '%s' "$rescue_json" | jget recorded_pane_id)"
[ "$recorded" = "$DEAD_PANE" ] \
  || fail "focus reported recorded pane '$recorded' (expected the dead '$DEAD_PANE')"
[ -n "$RESCUED_PANE" ] || fail "focus returned no rescued pane id"
[ "$RESCUED_PANE" != "$DEAD_PANE" ] || fail "the dead pane id must never be revived"
hrpc pane.get "{\"pane_id\":\"$RESCUED_PANE\"}" >/dev/null \
  || fail "rescued pane $RESCUED_PANE does not exist"
# Managed tabs are anchorless: the daemon closed the anchor after the launch,
# so closing the run's sole pane removed the whole tab (verified live: Herdr
# removes a tab when its last pane closes). The rescue therefore recreates the
# card tab under the same `card-<id>` label instead of the exact old tab id —
# and because this is a MANAGED rescue, it then closes that tab's anchor too,
# so the recreated tab converges to exactly the one rescued harness pane.
RESCUED_TAB="$(pane_field "$RESCUED_PANE" tab_id)"
[ -n "$RESCUED_TAB" ] || fail "rescued pane is not in any tab"
python3 - "$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")" \
  "$RESCUED_TAB" "$TAB_ID" "$CARD_ID" "$RESCUED_PANE" <<'PY' \
  || fail "rescued pane landed outside a converged card tab (old tab $TAB_ID)"
import json, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
rescued_tab, old_tab, card, rescued_pane = sys.argv[3:7]
match = next((t for t in tabs if t.get("tab_id") == rescued_tab), None)
assert match is not None
assert match.get("label") == f"card-{card}"
owned = [p for p in panes if p.get("tab_id") == rescued_tab]
assert owned == [p for p in owned if p.get("pane_id") == rescued_pane]
assert not any(p.get("label") == f"card-{card}-anchor" for p in owned)
print(f"  [ok] rescued pane landed in card tab {rescued_tab} (old tab {old_tab} was removed with its sole pane); the managed rescue closed the new tab's anchor, leaving exactly one harness pane", file=sys.stderr)
PY
rescue_label="$(pane_field "$RESCUED_PANE" label)"
# The dedup correlator must depend only on STABLE identity (card id + run id).
# With no database row permitted, a marker derived from the column's current name
# would stop matching the moment someone renamed the column, and `o` would resume
# the same conversation a second time.
[ "$rescue_label" = "card-$CARD_ID-r$RUN_ID-rescue" ] \
  || fail "rescued pane label '$rescue_label' is not 'card-$CARD_ID-r$RUN_ID-rescue'"
ok "rescued pane $RESCUED_PANE ('$rescue_label') created in tab $TAB_ID"

step "Assert the harness started in RESUME mode and did NOT re-run the task"
RESCUE_RECORD="$E2E_TMP/fake-pi-run-$RUN_ID-rescue.json"
for _ in $(seq 1 100); do
  [ -f "$RESCUE_RECORD" ] && break
  sleep .1
done
[ -f "$RESCUE_RECORD" ] || fail "the rescued pane's fake Pi never recorded its startup"
python3 - "$RESCUE_RECORD" "$CONV_ID" "$RUN_ID" "$CARD_ID" <<'PY'
import json, sys
path, conv_id, run_id, card_id = sys.argv[1:5]
record = json.load(open(path, encoding="utf-8"))
# Resume: Pi re-attaches by handing back a session id it already knows.
assert record["session_id"] == conv_id
assert record["fork_id"] is None
assert record["run_id"] == int(run_id)
assert record["card_id"] == int(card_id)
# The persisted execution environment is preserved, not rebuilt from config.
assert record["model"] == "rescue/pi-model"
# The card task must NOT appear in startup argv, and no agent.prompt is sent at
# all for a rescue, so the shim's prompt evidence must be absent.
assert not any("work to continue" in arg for arg in record["argv"])
# A rescue must not receive the card task again: the shim never runs, so it
# never records prompt evidence.
assert "prompt" not in record
print("  rescue startup:", json.dumps({k: record[k] for k in
      ("session_id", "fork_id", "model", "argv")}, ensure_ascii=False))
PY
# The original run's record is untouched: the rescue wrote its own file.
[ -f "$E2E_TMP/fake-pi-run-$RUN_ID.json" ] \
  || fail "the original run's fixture record disappeared"
ok "harness resumed conversation $CONV_ID without re-sending the card task"

# The env contract is enforced by the fixture itself, which exits 2 otherwise
# (see `e2e/fake-bin/pi`): BOARD_CARD_ID/BOARD_SOCKET present, BOARD_RESCUE=1,
# BOARD_RESUME_SESSION_ID equal to the resume id, BOARD_RESCUED_RUN_ID present,
# and BOARD_RUN_ID *absent* — a rescued pane must never hold the actor credential
# that would let it write to the immutable historical run row. A written record
# therefore proves the whole contract; `run_id`/`card_id` above could only have
# come from that env.
ok "rescued pane received the board env WITHOUT the run credential"

step "Focus again: must reuse the rescued pane, never create a second one"
PANES_AFTER_RESCUE="$(pane_count)"
again_json="$(e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$RUN_ID" --json)"
printf '  %s\n' "$again_json"
again_action="$(printf '%s' "$again_json" | jget action)"
[ "$again_action" = focused_rescued_pane ] \
  || fail "second focus reported '$again_action' (expected 'focused_rescued_pane')"
[ "$(printf '%s' "$again_json" | jget pane_id)" = "$RESCUED_PANE" ] \
  || fail "second focus did not reuse rescued pane $RESCUED_PANE"
[ "$(pane_count)" = "$PANES_AFTER_RESCUE" ] \
  || fail "second focus created another pane (was $PANES_AFTER_RESCUE, now $(pane_count))"
ok "second focus reused $RESCUED_PANE and created no extra pane"

step "A rescued pane that outlives its harness must not make o a permanent no-op"
# A Herdr pane label outlives the process that ran in it, so treating a label
# match alone as "already rescued" would leave the user staring at an idle shell
# forever. Kill the resumed harness and require `o` to reopen the run again.
# This is also the live check of what `PaneInfo.agent` does once a managed
# process is gone — the signal the idempotency rule depends on.
mut "pane send-keys $RESCUED_PANE C-c (terminate the resumed fake harness)"
e2e_herdr_mutate -- pane send-keys "$RESCUED_PANE" C-c >/dev/null
agent_after_exit="(pane closed)"
for _ in $(seq 1 60); do
  if hrpc pane.get "{\"pane_id\":\"$RESCUED_PANE\"}" >/dev/null 2>&1; then
    agent_after_exit="$(pane_field "$RESCUED_PANE" agent)"
    [ -z "$agent_after_exit" ] && { agent_after_exit="(absent)"; break; }
  else
    break
  fi
  sleep .1
done
printf '  observed: after the harness exited, pane.agent = %s\n' "$agent_after_exit"

third_json="$(e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$RUN_ID" --json)"
printf '  %s\n' "$third_json"
third_action="$(printf '%s' "$third_json" | jget action)"
THIRD_PANE="$(printf '%s' "$third_json" | jget pane_id)"
# Whether Herdr keeps the pane as a bare shell or removes it entirely, the
# contract is the same: the run gets a *working* pane again, never a dead one.
[ "$third_action" = rescued ] \
  || fail "focus after the harness exited reported '$third_action' (expected 'rescued')"
[ "$THIRD_PANE" != "$RESCUED_PANE" ] \
  || fail "focus after the harness exited reused the dead pane $RESCUED_PANE"
# The dead shell is reclaimed, not accumulated: no run row could ever collect it.
! hrpc pane.get "{\"pane_id\":\"$RESCUED_PANE\"}" >/dev/null 2>&1 \
  || fail "the dead rescue pane $RESCUED_PANE was left behind"
[ -f "$E2E_TMP/fake-pi-run-$RUN_ID-rescue.json" ] \
  || fail "the re-rescue did not record a startup"
ok "a rescue whose harness exited is reopened again into $THIRD_PANE"

step "Assert the rescued pane cannot finalize the historical run"
# A rescued pane holds no BOARD_RUN_ID, so `board done` from inside it has no run
# to act as — exactly the situation reproduced here, since this shell has none
# either. It must be refused: the historical row is immutable and a rescue must
# never rewrite an outcome. The runs diff in the next step is the authoritative
# guard if it ever were not refused.
set +e
done_out="$("$BOARD_BIN" card run done "$CARD_ID" --outcome fail --summary 'must not apply' 2>&1)"
done_code=$?
set -e
[ "$done_code" -ne 0 ] || fail "a closed run could be finalized again: $done_out"
ok "the closed run cannot be re-finalized without an open run"

step "Assert the rescue wrote NOTHING to the database"
"$BOARD_BIN" card show "$CARD_ID" --json | python3 -c '
import json, sys
print(json.dumps(json.load(sys.stdin)["runs"], sort_keys=True))
' >"$E2E_TMP/runs-after.json"
diff -u "$RUNS_BEFORE" "$E2E_TMP/runs-after.json" \
  || fail "a rescue mutated the runs table (it must be byte-for-byte immutable)"
run_rows="$(python3 -c '
import json, sys
print(len(json.load(open(sys.argv[1], encoding="utf-8"))))
' "$E2E_TMP/runs-after.json")"
[ "$run_rows" = 1 ] || fail "expected exactly 1 run row, found $run_rows"
ok "the historical run row is unchanged and no run row was added"

step "A run that cannot be reopened is refused explicitly and non-destructively"
# A configured (unmanaged) harness declares no resume support, so its run must be
# refused by name instead of being relaunched as a fresh conversation.
#
# Note what this proves about the capability gate: a configured run DOES carry a
# `session_id` — `dispatch::enqueue` mints a uuid and persists it even when the
# harness never received it (`build_invocation` returns no session id for a
# configured harness, and the fallback keeps the minted one). So "a conversation
# id is recorded" is NOT evidence that the harness can resume it. Only the
# explicit per-harness capability is, which is exactly why the daemon asks
# `[harness.NAME] resume` rather than inferring from the presence of an id.
refuse_json="$("$BOARD_BIN" card new --title 'no resume' --description 'cannot be reopened' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
REFUSE_ID="$(printf '%s' "$refuse_json" | jget id)"
mut "board move $REFUSE_ID 'Rescue Execute' -> configured harness run"
e2e_board_herdr_mutate -- move "$REFUSE_ID" "$EXEC_ID" --json >/dev/null
refuse_outcome="$(wait_ok "$REFUSE_ID" 100)" \
  || fail "configured run did not finish (outcome '$refuse_outcome')"
REFUSE_RUN="$(card_field "$REFUSE_ID" 'runs[-1].id')"
REFUSE_PANE="$(card_field "$REFUSE_ID" 'runs[-1].herdr_pane_id')"
# The minted-but-unused id really is there, so the refusal below can only come
# from the capability gate — not from a missing conversation id.
[ -n "$(card_field "$REFUSE_ID" 'runs[-1].session_id')" ] \
  || fail "expected the configured run to carry a minted session id"
if hrpc pane.get "{\"pane_id\":\"$REFUSE_PANE\"}" >/dev/null 2>&1; then
  mut "pane.close $REFUSE_PANE (disposable board-owned pane of a finished run)"
  e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$REFUSE_PANE\"}" >/dev/null 2>&1 || true
fi
for _ in $(seq 1 40); do
  hrpc pane.get "{\"pane_id\":\"$REFUSE_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
before_refuse="$(pane_count)"
set +e
refuse_out="$(e2e_board_herdr_mutate -- card run focus "$REFUSE_ID" "$REFUSE_RUN" --json 2>&1)"
refuse_code=$?
set -e
[ "$refuse_code" -ne 0 ] || fail "focus on an unreopenable run unexpectedly succeeded"
printf '%s\n' "$refuse_out" | grep -q "fake" \
  || fail "refusal does not name the harness that cannot resume: $refuse_out"
printf '%s\n' "$refuse_out" | grep -qi 'retry the card' \
  || fail "refusal is not actionable: $refuse_out"
[ "$(pane_count)" = "$before_refuse" ] \
  || fail "a refused rescue created a pane (it must be non-destructive)"
ok "an unreopenable run is refused by harness name without creating anything"

step "27-rescue-dead-pane: ALL CHECKS PASSED"
