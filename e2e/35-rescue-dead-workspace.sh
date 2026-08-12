#!/usr/bin/env bash
# 33-rescue-dead-workspace.sh — `run.focus` reopens a run whose pane AND its
# workspace were closed by creating a fresh workspace from the card's CURRENT
# space config (same resolution dispatch uses), in the run's own Herdr session,
# and resuming the harness conversation in an ephemeral pane there.
#
# Contract exercised end to end against a real Herdr, provider-free:
#   1. a managed Pi run completes in a daemon-created `new_workspace`; the pane
#      and the whole workspace are then closed;
#   2. `board card run focus CARD RUN` reports `action=rescued`: a NEW
#      workspace (label + cwd from the card's space config) is created in the
#      same Herdr session, its initial tab is adopted as the card tab, and the
#      harness starts in RESUME mode with the run's recorded conversation id —
#      without re-sending the task, without a new run row, and without the
#      BOARD_RUN_ID credential;
#   3. pressing focus again reports `action=focused_rescued_pane`: the same
#      workspace is reused (found by its label) and the same pane is focused —
#      no second workspace, no second pane;
#   4. the `runs` row is byte-for-byte unchanged;
#   5. a run that cannot be reopened is refused explicitly and
#      non-destructively: a run whose harness lacks resume support (configured
#      harness), and a run whose card config still references the closed
#      workspace (no replacement possible).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# Fake-managed setup creates roots, so cleanup must already be armed.
trap e2e_cleanup EXIT
e2e_enable_fake_pi
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

# Read one pane's field from a disposable workspace (read-only probe).
pane_field_ws() {
  hrpc pane.list "{\"workspace_id\":\"$1\"}" | python3 -c '
import json, sys
pane_id, field = sys.argv[1:3]
for pane in json.load(sys.stdin).get("panes", []):
    if pane.get("pane_id") == pane_id:
        print(pane.get(field) or "")
        break
' "$2" "$3"
}

# workspace_id of the single open workspace whose label matches, else empty.
workspace_by_label() {
  hrpc workspace.list '{}' | python3 -c '
import json, sys
label = sys.argv[1]
for ws in json.load(sys.stdin).get("workspaces", []):
    if ws.get("label") == label:
        print(ws.get("workspace_id") or "")
        break
' "$1"
}

open_workspace_count() {
  hrpc workspace.list '{}' | python3 -c '
import json, sys
print(len(json.load(sys.stdin).get("workspaces", [])))
'
}

WS_LABEL="hb-e2e-33-ws"
step "Dispatch a managed Pi run into a daemon-created new_workspace"
EXEC_ID="$(col_create '{"name":"Rescue Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title 'rescue across workspace' \
  --description 'work to continue after the workspace died' \
  --harness pi --model rescue/pi-model \
  --space-kind new-workspace --space-ref "$WS_LABEL" --space-cwd "$E2E_TMP" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)"
mut "board move $CARD_ID 'Rescue Execute' -> managed agent.start + daemon workspace.create(label=$WS_LABEL)"
e2e_board_herdr_mutate -- move "$CARD_ID" "$EXEC_ID" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || fail "managed Pi run did not finish (outcome '$outcome')"
[ "$outcome" = ok ] || fail "managed Pi outcome '$outcome' (expected ok)"

RUN_ID="$(card_field "$CARD_ID" 'runs[-1].id')"
DEAD_PANE="$(card_field "$CARD_ID" 'runs[-1].herdr_pane_id')"
CONV_ID="$(card_field "$CARD_ID" 'runs[-1].session_id')"
WS_ORIG="$(card_field "$CARD_ID" 'runs[-1].herdr_workspace_id')"
[ -n "$RUN_ID" ] && [ -n "$DEAD_PANE" ] && [ -n "$WS_ORIG" ] \
  || fail "run did not record its pane/workspace identity"
[ -n "$CONV_ID" ] || fail "managed Pi run did not record a harness conversation id"
[ "$WS_ORIG" = "$(workspace_by_label "$WS_LABEL")" ] \
  || fail "the daemon did not create workspace label '$WS_LABEL' ($WS_ORIG)"
e2e_ws_defer_close "$WS_ORIG"
ok "run $RUN_ID ran in daemon-created workspace $WS_ORIG (pane $DEAD_PANE, conversation $CONV_ID)"

# A second card whose space config REFERENCES the same workspace by id: once
# that workspace is closed, this card's config can never supply a replacement.
step "Dispatch two more runs into the same workspace for the refusal cases"
fail_json="$("$BOARD_BIN" card new --title 'dead space ref' --description 'cannot be replaced' \
  --harness pi --model rescue/pi-model \
  --space-kind workspace --space-ref "$WS_ORIG" --json)"
FAIL_ID="$(printf '%s' "$fail_json" | jget id)"
mut "board move $FAIL_ID 'Rescue Execute' -> managed agent.start in $WS_ORIG"
e2e_board_herdr_mutate -- move "$FAIL_ID" "$EXEC_ID" --json >/dev/null
outcome="$(wait_ok "$FAIL_ID" 100)" || fail "space-ref card did not finish (outcome '$outcome')"
FAIL_RUN="$(card_field "$FAIL_ID" 'runs[-1].id')"

refuse_json="$("$BOARD_BIN" card new --title 'no resume' --description 'cannot be reopened' \
  --harness fake --space-kind workspace --space-ref "$WS_ORIG" --json)"
REFUSE_ID="$(printf '%s' "$refuse_json" | jget id)"
mut "board move $REFUSE_ID 'Rescue Execute' -> configured harness run in $WS_ORIG"
e2e_board_herdr_mutate -- move "$REFUSE_ID" "$EXEC_ID" --json >/dev/null
refuse_outcome="$(wait_ok "$REFUSE_ID" 100)" \
  || fail "configured run did not finish (outcome '$refuse_outcome')"
REFUSE_RUN="$(card_field "$REFUSE_ID" 'runs[-1].id')"
# The minted-but-unused conversation id really is recorded, so the refusal can
# only come from the capability gate — see 27-rescue-dead-pane.sh.
[ -n "$(card_field "$REFUSE_ID" 'runs[-1].session_id')" ] \
  || fail "expected the configured run to carry a minted session id"
ok "space-ref card ran as run $FAIL_RUN; configured card ran as run $REFUSE_RUN"

# Freeze the authoritative run row so the no-DB-writes constraint is checkable.
RUNS_BEFORE="$E2E_TMP/runs-before.json"
"$BOARD_BIN" card show "$CARD_ID" --json | python3 -c '
import json, sys
print(json.dumps(json.load(sys.stdin)["runs"], sort_keys=True))
' >"$RUNS_BEFORE"

step "HERDR MUTATION: workspace.close $WS_ORIG (the run's workspace and panes are all gone)"
mut "workspace.close $WS_ORIG (disposable daemon-created workspace of finished runs)"
e2e_hrpc_mutate -- workspace.close "{\"workspace_id\":\"$WS_ORIG\"}" >/dev/null 2>&1 || true
for _ in $(seq 1 40); do
  hrpc workspace.list '{}' | grep -q "$WS_ORIG" || break
  sleep .1
done
hrpc workspace.list '{}' | grep -q "$WS_ORIG" \
  && fail "workspace $WS_ORIG is still open; the closed-workspace case cannot be exercised"
! hrpc pane.get "{\"pane_id\":\"$DEAD_PANE\"}" >/dev/null 2>&1 \
  || fail "recorded pane $DEAD_PANE is still alive after the workspace close"
[ "$(open_workspace_count)" = 0 ] || fail "unexpected workspaces remain open"
ok "workspace $WS_ORIG (and its panes) is gone; no workspace remains open"

step "Focus the run: the daemon must CREATE a replacement workspace and rescue into it"
rescue_json="$(e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$RUN_ID" --json)"
printf '  %s\n' "$rescue_json"
action="$(printf '%s' "$rescue_json" | jget action)"
[ "$action" = rescued ] || fail "focus reported action '$action' (expected 'rescued')"
RESCUED_PANE="$(printf '%s' "$rescue_json" | jget pane_id)"
recorded="$(printf '%s' "$rescue_json" | jget recorded_pane_id)"
[ "$recorded" = "$DEAD_PANE" ] \
  || fail "focus reported recorded pane '$recorded' (expected the dead '$DEAD_PANE')"
[ -n "$RESCUED_PANE" ] || fail "focus returned no rescued pane id"
WS_NEW="$(workspace_by_label "$WS_LABEL")"
[ -n "$WS_NEW" ] || fail "the rescue did not create a workspace labelled '$WS_LABEL'"
[ "$WS_NEW" != "$WS_ORIG" ] || fail "the rescue must not reuse the closed workspace id"
[ "$(open_workspace_count)" = 1 ] || fail "expected exactly one open workspace"
# Same-session guarantee: the new workspace appeared in the run's own Herdr
# session (the very socket this scenario talks to), never another one.
hrpc pane.get "{\"pane_id\":\"$RESCUED_PANE\"}" >/dev/null \
  || fail "rescued pane $RESCUED_PANE does not exist"
[ "$(pane_field_ws "$WS_NEW" "$RESCUED_PANE" workspace_id)" = "$WS_NEW" ] \
  || fail "rescued pane $RESCUED_PANE is not in the new workspace $WS_NEW"
# The pane was split with the card's configured space-cwd, exactly like a
# normal dispatch into a freshly created workspace.
[ "$(pane_field_ws "$WS_NEW" "$RESCUED_PANE" cwd)" = "$E2E_TMP" ] \
  || fail "rescued pane cwd is not the card's space-cwd ($E2E_TMP)"
e2e_ws_defer_close "$WS_NEW"
rescue_label="$(pane_field_ws "$WS_NEW" "$RESCUED_PANE" label)"
[ "$rescue_label" = "card-$CARD_ID-r$RUN_ID-rescue" ] \
  || fail "rescued pane label '$rescue_label' is not 'card-$CARD_ID-r$RUN_ID-rescue'"
ok "replacement workspace $WS_NEW (label '$WS_LABEL') created in the same session; rescue pane $RESCUED_PANE placed in it"

step "Assert the harness started in RESUME mode and did NOT re-run the task"
RESCUE_RECORD="$E2E_TMP/fake-pi-run-$RUN_ID-rescue.json"
for _ in $(seq 1 100); do
  [ -f "$RESCUE_RECORD" ] && break
  sleep .1
done
[ -f "$RESCUE_RECORD" ] || fail "the rescued pane's fake Pi never recorded its startup"
python3 - "$RESCUE_RECORD" "$CONV_ID" "$RUN_ID" "$CARD_ID" "$E2E_TMP" <<'PY'
import json, sys
path, conv_id, run_id, card_id, expected_cwd = sys.argv[1:6]
record = json.load(open(path, encoding="utf-8"))
# Resume: Pi re-attaches by handing back a session id it already knows.
assert record["session_id"] == conv_id
assert record["fork_id"] is None
assert record["run_id"] == int(run_id)
assert record["card_id"] == int(card_id)
# The persisted execution environment is preserved, not rebuilt from config.
assert record["model"] == "rescue/pi-model"
assert record["cwd"] == expected_cwd
# The card task must NOT appear in startup argv, and no agent.prompt is sent at
# all for a rescue, so the shim's prompt evidence must be absent.
assert not any("work to continue" in arg for arg in record["argv"])
assert "prompt" not in record
print("  rescue startup:", json.dumps({k: record[k] for k in
      ("session_id", "fork_id", "model", "cwd")}, ensure_ascii=False))
PY
# The original run's record is untouched: the rescue wrote its own file.
[ -f "$E2E_TMP/fake-pi-run-$RUN_ID.json" ] \
  || fail "the original run's fixture record disappeared"
# The env contract is enforced by the fixture itself, which exits 2 otherwise:
# BOARD_RESCUE=1, BOARD_RESUME_SESSION_ID == resume id, BOARD_RESCUED_RUN_ID
# present, and BOARD_RUN_ID *absent* — a rescued pane must never hold the actor
# credential that would let it write to the immutable historical run row.
ok "harness resumed conversation $CONV_ID in the new workspace without re-sending the task or holding the run credential"

step "Focus again: must reuse the workspace and the pane, create neither"
again_json="$(e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$RUN_ID" --json)"
printf '  %s\n' "$again_json"
again_action="$(printf '%s' "$again_json" | jget action)"
[ "$again_action" = focused_rescued_pane ] \
  || fail "second focus reported '$again_action' (expected 'focused_rescued_pane')"
[ "$(printf '%s' "$again_json" | jget pane_id)" = "$RESCUED_PANE" ] \
  || fail "second focus did not reuse rescued pane $RESCUED_PANE"
[ "$(workspace_by_label "$WS_LABEL")" = "$WS_NEW" ] \
  || fail "second focus did not reuse workspace $WS_NEW"
[ "$(open_workspace_count)" = 1 ] || fail "second focus created another workspace"
ok "second focus reused $RESCUED_PANE in $WS_NEW and created no workspace or pane"

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

step "A run whose card config cannot replace the workspace is refused explicitly"
# The card still references the CLOSED workspace id: the recorded workspace is
# gone and the current space config (a `workspace`-kind ref to the same id)
# cannot resolve a replacement. The refusal names both and creates nothing.
set +e
fail_out="$(e2e_board_herdr_mutate -- card run focus "$FAIL_ID" "$FAIL_RUN" --json 2>&1)"
fail_code=$?
set -e
[ "$fail_code" -ne 0 ] || fail "focus on an irreplaceable run unexpectedly succeeded"
printf '%s\n' "$fail_out" | grep -q "$WS_ORIG" \
  || fail "refusal does not name the dead workspace: $fail_out"
printf '%s\n' "$fail_out" | grep -qi 'replacement' \
  || fail "refusal does not explain the config dead end: $fail_out"
[ "$(open_workspace_count)" = 1 ] || fail "a refused rescue created a workspace"
ok "an irreplaceable run is refused, naming the dead workspace, without creating anything"

step "A run whose harness cannot resume is refused by name, non-destructively"
set +e
refuse_out="$(e2e_board_herdr_mutate -- card run focus "$REFUSE_ID" "$REFUSE_RUN" --json 2>&1)"
refuse_code=$?
set -e
[ "$refuse_code" -ne 0 ] || fail "focus on an unreopenable run unexpectedly succeeded"
printf '%s\n' "$refuse_out" | grep -q "fake" \
  || fail "refusal does not name the harness that cannot resume: $refuse_out"
printf '%s\n' "$refuse_out" | grep -qi 'retry the card' \
  || fail "refusal is not actionable: $refuse_out"
[ "$(open_workspace_count)" = 1 ] || fail "a refused rescue created a workspace"
ok "an unreopenable run is refused by harness name without creating anything"

step "33-rescue-dead-workspace: ALL CHECKS PASSED"
