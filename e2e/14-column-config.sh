#!/usr/bin/env bash
# 14-column-config.sh — the column harness_override (now a SELECT in the TUI)
# drives a run end-to-end through real Herdr, and `harness.list` advertises
# config-defined harnesses. The TUI select's data source and the
# permission-hiding rule are unit/snapshot-tested in board-tui; this scenario
# exercises the dispatch path the select feeds: a column whose harness_override
# points at a config-defined harness, with effort/permission overrides that flow
# into the run's resolved argv.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate

# A second config-defined harness that surfaces the resolved model/effort/
# permission through {…} argv placeholders, so a column harness_override is
# observable in the run's stored argv_json. The prompt travels via BOARD_PROMPT
# (config-defined harnesses do not take a trailing prompt argv). Appended to the
# isolated config BEFORE the daemon starts (it reads config once at startup).
cat >> "$HERDR_BOARD_CONFIG" <<EOF
[harness.fake-ov]
argv = ["env", "BOARD_BIN=$BOARD_BIN", "bash", "$E2E_FAKE_AGENT", "{model}", "{effort}", "{permission_mode}"]
EOF

e2e_daemon_start

step "harness.list advertises built-ins + config-defined harnesses"
brpc harness.list '{}' | python3 -c '
import json, sys
hs = json.load(sys.stdin)["harnesses"]
# Built-ins first in default order (pi before claude), then config-defined sorted.
assert hs == ["pi", "claude", "fake", "fake-ov"], hs
print("  harnesses:", ", ".join(hs))
'
ok "harness.list returns built-ins (pi, claude) and config-defined (fake, fake-ov)"

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
python3 - "$show" <<'PY'
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
print("  run harness:", run["harness"], "| argv:", argv)
PY
ok "column harness_override=fake-ov drove the run; effort=low, permission=auto resolved in argv"

step "14-column-config: ALL CHECKS PASSED"
