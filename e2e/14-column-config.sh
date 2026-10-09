#!/usr/bin/env bash
# 14-column-config.sh — the column harness_override (now a SELECT in the TUI)
# drives a run end-to-end through real Herdr, and `harness.list` advertises
# installed builtins plus config-defined harnesses. The TUI select's data
# source and the permission-hiding rule are unit/snapshot-tested in board-tui;
# this scenario exercises the dispatch path the select feeds: a column whose
# harness_override points at a config-defined harness, with effort/permission
# overrides that flow into the run's resolved argv.
#
# Evidence is two-sided, not stored-only: a logging shim (E2E_FAKE_AGENT
# override, real fake-agent underneath) records the argv the spawned process
# ACTUALLY received, and the scenario compares that exact argv against the
# run's stored argv_json tail. Placeholder resolution itself (`{model}`
# dropped when unset, `{effort}`/`{permission_mode}` substituted, nothing
# left unsubstituted) is pinned hermetically in
# `crates/board-daemon/src/dispatch/tests/launch_plan.rs`
# (`column_override_resolves_the_exact_configured_argv`).
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build

# Logging shim: append this process's argv (one per line) to ARGV_LOG, then
# run the real fake agent unchanged. The log is the actual-runner side of
# the argv evidence; the run row's argv_json is the stored side.
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

# A second config-defined harness that surfaces the resolved model/effort/
# permission through {…} argv placeholders. The prompt travels via BOARD_PROMPT
# (config-defined harnesses do not take a trailing prompt argv). Appended to the
# isolated config BEFORE the daemon starts (it reads config once at startup).
cat >> "$HERDR_BOARD_CONFIG" <<EOF
[harness.fake-ov]
efforts = ["low"]
permission_modes = ["auto"]
argv = ["env", "BOARD_BIN=$BOARD_BIN", "ARGV_LOG=$ARGV_LOG", "REAL_FAKE_AGENT=$E2E_LIB_DIR/fake-agent.sh", "bash", "$E2E_FAKE_AGENT", "{model}", "{effort}", "{permission_mode}"]
EOF

e2e_daemon_start

step "harness.list advertises installed built-ins + config-defined harnesses"
brpc harness.list '{}' | python3 -c '
import json, sys
hs = json.load(sys.stdin)["harnesses"]
# Harness discovery filters the builtins against the Herdr integration.list and
# the ephemeral CI session installs none of them, so the full five may be
# legitimately absent. Assert the documented shape instead of the exact list:
# an installed-builtin prefix in canonical order, then the config-defined
# harnesses (always appended) sorted.
builtins = ["pi", "claude", "codex", "opencode", "antigravity"]
split = next((i for i, h in enumerate(hs) if h not in builtins), len(hs))
installed, config = hs[:split], hs[split:]
assert installed == [h for h in builtins if h in installed], hs
assert config == ["fake", "fake-ov"], hs
print("  harnesses:", ", ".join(hs))
'
ok "harness.list returns installed builtins in canonical order, then config-defined (fake, fake-ov)"

step "HERDR MUTATION: create disposable workspace for the override column"
e2e_ws_create board-colcfg-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create an auto column whose overrides drive the run (harness fake-ov)"
COL_ID="$(col_create "$(python3 -c '
import json
print(json.dumps({
    "name": "Override Execute",
    "trigger": "auto",
    "system_prompt": "COLCFG E2E SYSTEM",
    "harness_override": "fake-ov",
    "effort_override": "low",
    "permission_override": "auto",
}))
')")"
[ -n "$COL_ID" ] || fail "could not create Override Execute column"
echo "  column: $COL_ID"

step "Create a default-harness card and dispatch into the override column"
card_json="$("$BOARD_BIN" card new --title "Override Column Card" \
  --description "run via the column harness_override" \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"

mut "board move $CARD_ID 'Override Execute' -> herdr agent.start (harness fake-ov)"
e2e_board_herdr_mutate -- move "$CARD_ID" "Override Execute" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 80)" || {

  e2e_card_failure_diag "$CARD_ID"
  fail "override-column run outcome '$outcome', expected ok"
}
[ "$outcome" = "ok" ] || fail "override-column run outcome '$outcome', expected ok"

show="$E2E_TMP/show.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$show"
python3 - "$show" "$ARGV_LOG" <<'PY'
import json, sys
x = json.load(open(sys.argv[1], encoding="utf-8"))
card, runs = x["card"], x["runs"]
assert len(runs) == 1
run = runs[0]
# The column harness_override drove the run; the card's own harness (pi) was
# overridden by the column setting.
assert run["harness"] == "fake-ov"
argv = json.loads(run["argv_json"])
assert argv[0] == "env"
# {model} was unset -> its element dropped; {effort}/{permission_mode} resolved.
assert "low" in argv
assert "auto" in argv
assert not any(a in ("{model}", "{effort}", "{permission_mode}") for a in argv)
print("  run harness:", run["harness"], "| stored argv:", argv)
# Actual-runner side: the spawned process logged exactly what it received,
# and it must equal the stored argv's tail (everything past the env/launcher
# prefix the runner never sees as positional args).
actual = open(sys.argv[2], encoding="utf-8").read().split()
assert actual == ["low", "auto"], actual
assert argv[-len(actual):] == actual, (argv, actual)
print("  actual runner argv:", actual, "| matches stored argv tail")
PY
ok "column harness_override=fake-ov drove the run; stored and actual argv agree (effort=low, permission=auto)"

step "14-column-config: ALL CHECKS PASSED"
