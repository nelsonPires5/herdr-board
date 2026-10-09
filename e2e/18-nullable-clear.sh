#!/usr/bin/env bash
# 18-nullable-clear.sh — clearing nullable column/card overrides, then one
# dispatch proving the cleared state drives the run.
#
# Live scope is ONE dispatch with distinguishable before/after values: the
# column carries `fake-col` overrides (model `col-model-live`, effort `high`)
# while the card carries `fake` values (model `card-model-live`, effort
# `low`). After clearing both, the single dispatch must resolve to the card's
# `fake` harness — a stale column override would surface as `fake-col` — and
# a logging shim records the argv the spawned process ACTUALLY received for
# comparison against the stored argv_json tail.
#
# The omitted/null/value semantics per field, the set-then-clear and
# omitted-preservation checks, and the invalid-merged-update atomic rejection
# (runs/comments untouched) all live hermetically now:
# `crates/board-core/tests/protocol.rs`
# (`nullable_update_patches_distinguish_omitted_null_and_value`),
# `crates/board-core/tests/db/crud.rs`
# (`nullable_updates_set_then_clear_and_survive_reopen`),
# `crates/board-core/tests/engine.rs` (merged validation),
# `crates/board-daemon/src/ops/tests/validation.rs`
# (`merged_invalid_updates_are_atomic_and_emit_no_event`).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# This scenario uses the configured fake harness plus a second,
# column-only harness. It never logs prompt or system-prompt bodies;
# assertions inspect durable settings, run metadata, and the runner's own
# argv log.
e2e_init
e2e_build

# Logging shim: append this process's argv (one per line) to ARGV_LOG, then
# run the real fake agent unchanged.
cat >"$E2E_SCENARIO_ROOT/log-agent.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: "${REAL_FAKE_AGENT:?REAL_FAKE_AGENT required}"
if [ -n "${ARGV_LOG:-}" ]; then printf '%s\n' "$@" >>"$ARGV_LOG"; fi
exec bash "$REAL_FAKE_AGENT" "$@"
EOF
chmod +x "$E2E_SCENARIO_ROOT/log-agent.sh"
export E2E_FAKE_AGENT="$E2E_SCENARIO_ROOT/log-agent.sh"
export ARGV_LOG="$E2E_SCENARIO_ROOT/actual-argv.log"
: >"$ARGV_LOG"
export E2E_FAKE_ENV="ARGV_LOG=$ARGV_LOG REAL_FAKE_AGENT=$E2E_LIB_DIR/fake-agent.sh"

e2e_isolate

# Add capabilities to the already-created fake harness without introducing a
# second TOML table, and give the card harness a literal marker arg so the
# actual-runner argv log is distinguishable. The configured runner remains
# provider-free.
python3 - "$HERDR_BOARD_CONFIG" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
s = s.replace('argv = ["env",', 'models = ["fake-model"]\nefforts = ["low"]\npermission_modes = ["auto"]\nargv = ["env",', 1)
p.write_text(s, encoding="utf-8")
PY

# A column-only harness with a DIFFERENT name from the card's `fake`, so a
# stale column override leaking into dispatch is visible in run.harness.
# Appended BEFORE the daemon starts (it reads config once at startup).
cat >> "$HERDR_BOARD_CONFIG" <<EOF
[harness.fake-col]
efforts = ["low", "high"]
permission_modes = ["auto"]
argv = ["env", "BOARD_BIN=$BOARD_BIN", "bash", "$E2E_LIB_DIR/fake-agent.sh", "{model}", "{effort}", "{permission_mode}"]
EOF

# Route the card harness through the logging shim with a literal marker: the
# spawned process must log exactly ["card-marker"], proving the card's
# harness (not the column's) executed.
python3 - "$HERDR_BOARD_CONFIG" "$E2E_FAKE_AGENT" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1])
agent = sys.argv[2]
s = p.read_text(encoding="utf-8")
old = '"bash", "%s"]' % agent
assert old in s, "logging shim argv missing from config"
s = s.replace(old, '"bash", "%s", "card-marker"]' % agent, 1)
p.write_text(s, encoding="utf-8")
PY

e2e_daemon_start
e2e_ws_create nullable-clear
WS_ID="$E2E_WS"

step "Create a column with distinctive overrides (fake-col, not the card's fake)"
COLUMN_PARAMS="$(python3 - <<'PY'
import json
print(json.dumps({
    "name": "Nullable Target",
    "trigger": "auto",
    "system_prompt": "col-system-live",
    "harness_override": "fake-col",
    "model_override": "col-model-live",
    "effort_override": "high",
    "permission_override": "auto",
    "timeout_minutes": 2,
}))
PY
)"
TARGET_ID="$(col_create "$COLUMN_PARAMS")"
[ -n "$TARGET_ID" ] || fail "could not create Nullable Target column"

step "Create a card with distinctive values on the fake harness"
CARD_JSON="$($BOARD_BIN card new --title 'Nullable card' --description 'provider-free nullable scenario' \
  --harness fake --model card-model-live --effort low --permission auto \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jget id)"
[ -n "$CARD_ID" ] || fail "could not create Nullable card"

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

step "Clear the card's nullable settings (keep space_ref so it stays dispatchable)"
CLEAR_CARD_PARAMS="$(python3 - "$CARD_ID" <<'PY'
import json, sys
print(json.dumps({
    "id": int(sys.argv[1]),
    "model": None,
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
assert v["space_ref"] is not None
print("  card nulls persisted as clears")
PY

step "Dispatch after clears uses the card harness, not stale column overrides"
e2e_board_herdr_mutate -- move "$CARD_ID" "Nullable Target" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || {
  e2e_card_failure_diag "$CARD_ID"
  fail "cleared card did not complete with configured fake harness"
}
[ "$outcome" = ok ] || fail "cleared card outcome was '$outcome'"
show="$E2E_TMP/show.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$show"
python3 - "$show" "$ARGV_LOG" <<'PY'
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
runs = x["runs"]
assert len(runs) == 1
run = runs[-1]
# Distinguishable harnesses: the cleared column's fake-col must not leak;
# the card's fake drove the run.
assert run["harness"] == "fake", run["harness"]
argv = json.loads(run["argv_json"])
assert argv[-1] == "card-marker", argv
print("  run harness:", run["harness"], "| stored argv tail:", argv[-1])
# Actual-runner side: the spawned process logged exactly what it received.
actual = open(sys.argv[2], encoding="utf-8").read().split()
assert actual == ["card-marker"], actual
assert argv[-len(actual):] == actual, (argv, actual)
print("  actual runner argv:", actual, "| matches stored argv tail")
PY

step "18-nullable-clear: ALL CHECKS PASSED"
