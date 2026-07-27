#!/usr/bin/env bash
# 18-nullable-clear.sh — omitted/null/value semantics and merged validation.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# This scenario uses only the configured fake harness. It never logs prompt or
# system-prompt bodies; assertions inspect durable settings and run metadata.
e2e_init
e2e_build
e2e_isolate

# Add capabilities to the already-created fake harness without introducing a
# second TOML table. The configured runner remains provider-free.
python3 - "$HERDR_BOARD_CONFIG" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
s = s.replace('argv = ["env",', 'models = ["fake-model"]\nefforts = ["low"]\npermission_modes = ["auto"]\nargv = ["env",', 1)
p.write_text(s, encoding="utf-8")
PY

e2e_daemon_start
e2e_ws_create nullable-clear
WS_ID="$E2E_WS"

step "Create a fully configured column and prove omitted fields preserve values"
COLUMN_PARAMS="$(python3 - <<'PY'
import json
print(json.dumps({
    "name": "Nullable Target",
    "trigger": "auto",
    "system_prompt": "column-test-system",
    "on_success_column_id": None,
    "on_fail_column_id": None,
    "harness_override": "fake",
    "model_override": "fake-model",
    "effort_override": "low",
    "permission_override": "auto",
    "timeout_minutes": 2,
}))
PY
)"
TARGET_ID="$(col_create "$COLUMN_PARAMS")"
# The first no-op update omits every nullable member; only the name changes.
OMITTED_PARAMS="$(python3 - "$TARGET_ID" <<'PY'
import json, sys
print(json.dumps({"id": int(sys.argv[1]), "name": "Nullable Target Renamed"}))
PY
)"
BRPC_TARGET="$(brpc column.update "$OMITTED_PARAMS")"
python3 - "$BRPC_TARGET" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
assert v["system_prompt"] == "column-test-system"
assert v["harness_override"] == "fake"
assert v["model_override"] == "fake-model"
assert v["effort_override"] == "low"
assert v["permission_override"] == "auto"
assert v["timeout_minutes"] == 2
print("  omitted column fields preserved")
PY

step "Clear every nullable column override atomically"
CLEAR_COLUMN_PARAMS="$(python3 - "$TARGET_ID" <<'PY'
import json, sys
print(json.dumps({
    "id": int(sys.argv[1]),
    "system_prompt": None,
    "on_success_column_id": None,
    "on_fail_column_id": None,
    "harness_override": None,
    "model_override": None,
    "effort_override": None,
    "permission_override": None,
    "timeout_minutes": None,
}))
PY
)"
CLEARED="$(brpc column.update "$CLEAR_COLUMN_PARAMS")"
python3 - "$CLEARED" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
for key in ("system_prompt", "on_success_column_id", "on_fail_column_id",
            "harness_override", "model_override", "effort_override",
            "permission_override", "timeout_minutes"):
    assert v[key] is None
print("  column nulls persisted as clears")
PY

step "Exercise card set, omitted preservation, and explicit null clears"
CARD_JSON="$($BOARD_BIN card new --title 'Nullable card' --description 'provider-free nullable scenario' \
  --harness fake --model fake-model --effort low --permission auto \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jget id)"
# Set then clear the card model; omission of other fields must preserve them.
SET_CARD_PARAMS="$(python3 - "$CARD_ID" <<'PY'
import json, sys
print(json.dumps({"id": int(sys.argv[1]), "model": "set-then-clear"}))
PY
)"
SET_CARD="$(brpc card.update "$SET_CARD_PARAMS")"
[ "$(printf '%s' "$SET_CARD" | jget model)" = set-then-clear ] || fail "card set did not persist"
CLEAR_MODEL_PARAMS="$(python3 - "$CARD_ID" <<'PY'
import json, sys
print(json.dumps({"id": int(sys.argv[1]), "model": None}))
PY
)"
PRESERVED="$(brpc card.update "$CLEAR_MODEL_PARAMS")"
python3 - "$PRESERVED" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
assert v["model"] is None
assert v["effort"] == "low"
assert v["permission_mode"] == "auto"
assert v["space_ref"] is not None
print("  card set/null worked and omitted fields were preserved")
PY
# Clear the remaining compatible nullable card settings. Keep space_ref so the
# workspace remains dispatchable; space_cwd is already NULL for this kind.
CLEAR_CARD_PARAMS="$(python3 - "$CARD_ID" <<'PY'
import json, sys
print(json.dumps({
    "id": int(sys.argv[1]),
    "effort": None,
    "permission_mode": None,
    "session": None,
    "space_cwd": None,
}))
PY
)"
CLEARED_CARD="$(brpc card.update "$CLEAR_CARD_PARAMS")"
python3 - "$CLEARED_CARD" <<'PY'
import json, sys
v=json.loads(sys.argv[1])
assert all(v[k] is None for k in ("model", "effort", "permission_mode", "session", "space_cwd"))
print("  compatible card nulls persisted")
PY

step "Reject an invalid merged new_workspace clear without mutation"
INVALID_JSON="$($BOARD_BIN card new --title 'Invalid merged state' --harness fake \
  --space-kind new_workspace --space-ref nullable-feature --space-cwd "$E2E_TMP" --json)"
INVALID_ID="$(printf '%s' "$INVALID_JSON" | jget id)"
BEFORE="$($BOARD_BIN card show "$INVALID_ID" --json)"
set +e
brpc card.update "$(python3 - "$INVALID_ID" <<'PY'
import json, sys
print(json.dumps({"id": int(sys.argv[1]), "space_cwd": None}))
PY
)" >"$E2E_TMP/invalid-update.out" 2>"$E2E_TMP/invalid-update.err"
status=$?
set -e
[ "$status" -ne 0 ] || fail "invalid merged new_workspace clear was accepted"
AFTER="$($BOARD_BIN card show "$INVALID_ID" --json)"
python3 - "$BEFORE" "$AFTER" <<'PY'
import json, sys
before=json.loads(sys.argv[1]); after=json.loads(sys.argv[2])
assert before["card"] == after["card"]
assert before["runs"] == after["runs"]
assert before["comments"] == after["comments"]
print("  invalid merged update rejected atomically")
PY

step "Dispatch after clears uses the card harness, not stale column overrides"
# The target column is now override-free and the card has only the configured
# fake harness left. This mutation is identity-gated by the shared wrapper.
e2e_board_herdr_mutate -- move "$CARD_ID" "Nullable Target Renamed" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || {
  e2e_card_failure_diag "$CARD_ID"
  fail "cleared card did not complete with configured fake harness"
}
[ "$outcome" = ok ] || fail "cleared card outcome was '$outcome'"
run_harness="$(card_field "$CARD_ID" 'runs[-1].harness')"
[ "$run_harness" = fake ] || fail "dispatch used stale harness '$run_harness'"

step "18-nullable-clear: ALL CHECKS PASSED"
