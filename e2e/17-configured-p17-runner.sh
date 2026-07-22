#!/usr/bin/env bash
# 17-configured-p17-runner.sh — unmanaged protocol-17 harness contract.
# The disposable runner records startup/env evidence and never invokes a provider.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate
RUNNER="$E2E_TMP/p17-runner.sh"
cat >"$RUNNER" <<'RUNNER'
#!/usr/bin/env bash
set -euo pipefail
: "${BOARD_BIN:?BOARD_BIN required}"
: "${BOARD_CARD_ID:?BOARD_CARD_ID required}"
: "${BOARD_RUN_ID:?BOARD_RUN_ID required}"
: "${BOARD_PROMPT:?BOARD_PROMPT required}"
: "${BOARD_SYSTEM_PROMPT:?BOARD_SYSTEM_PROMPT required}"
: "${BOARD_SOCKET:?BOARD_SOCKET required}"
: "${HERDR_SOCKET_PATH:?HERDR_SOCKET_PATH required}"
out="$(dirname "$BOARD_SOCKET")/p17-runner-$BOARD_RUN_ID.json"
python3 - "$out" "$PWD" "$BOARD_CARD_ID" "$BOARD_RUN_ID" "$BOARD_PROMPT" "$BOARD_SYSTEM_PROMPT" "$BOARD_SOCKET" "$HERDR_SOCKET_PATH" "$@" <<'PY'
import json, os, sys
path, cwd, card_id, run_id, prompt, system, board_socket, herdr_socket, *argv = sys.argv[1:]
with open(path, "w", encoding="utf-8") as f:
    json.dump({"cwd": cwd, "card_id": int(card_id), "run_id": int(run_id),
               "prompt": prompt, "system_prompt": system,
               "herdr_socket": herdr_socket, "board_socket": board_socket,
               "argv": argv}, f, ensure_ascii=False, indent=2)
PY
"$BOARD_BIN" comment "p17 runner: exact startup/env evidence recorded"
# Agent-originated protocol callback, not a scenario-side infrastructure command.
"$BOARD_BIN" done --outcome ok
# Keep the pane alive for the structural checks below. This is deliberately
# bounded; owned-workspace cleanup normally ends it before the timeout.
sleep "${P17_RUNNER_HOLD:-60}"
RUNNER
chmod 700 "$RUNNER"
e2e_script_resource_register configured-runner p17-configured-runner "$RUNNER" \
  || fail "cannot record configured runner digest"
e2e_defer "e2e_script_remove_owned p17-configured-runner '$RUNNER'"
# JSON quoting preserves each configured argv element, including the newline.
python3 - "$HERDR_BOARD_CONFIG" "$RUNNER" "$BOARD_BIN" <<'PY'
import json, sys
p, runner, board = sys.argv[1:]
with open(p, "a", encoding="utf-8") as f:
    f.write("\n[harness.p17-runner]\n")
    f.write("argv = " + json.dumps(["env", "BOARD_BIN=" + board, runner,
        "literal argument with spaces", "line one\nline two", "{effort}"], ensure_ascii=False) + "\n")
PY
e2e_daemon_start
e2e_ws_create p17-runner; WS_ID="$E2E_WS"
workspace_panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
EXPECTED_PANE_CWD="$(printf '%s' "$workspace_panes" | python3 -c '
import json, sys
panes=json.load(sys.stdin).get("panes", [])
cwds=[p.get("cwd") for p in panes if p.get("cwd")]
assert cwds, panes
print(cwds[0])
')"
printf '  disposable workspace pane cwd: %s\n' "$EXPECTED_PANE_CWD"
EXEC_ID="$(col_create '{"name":"P17 Runner","trigger":"auto"}')"

step "Dispatch configured harness through the protocol-17 runner bridge"
card_json="$($BOARD_BIN card new --title 'P17 configured' \
  --description $'configured prompt with spaces\nand a newline' --harness p17-runner --effort low \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)"
mut "board move $CARD_ID 'P17 Runner' -> unmanaged pane run bridge"
e2e_board_herdr_mutate -- move "$CARD_ID" "P17 Runner" --json >/dev/null
outcome="$(wait_ok "$CARD_ID" 100)" || {
  printf '%s\n' '--- configured runner failure diagnostics (disposable session only) ---' >&2
  e2e_card_failure_diag "$CARD_ID"
  printf 'runner_records=%s\n' "$(find "$E2E_TMP" -maxdepth 1 -type f -name 'p17-runner-*.json' -printf . 2>/dev/null | wc -c)" >&2
  hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" >&2 2>/dev/null || true

  fail "configured runner outcome '$outcome'"
}
[ "$outcome" = ok ] || fail "configured runner did not complete ok (got '$outcome')"
RUN_ID="$(card_field "$CARD_ID" runs[-1].id)"
RECORD="$E2E_TMP/p17-runner-$RUN_ID.json"
SHOW="$E2E_TMP/p17-runner-show.json"
"$BOARD_BIN" card show "$CARD_ID" --json >"$SHOW"
[ -f "$RECORD" ] || fail "temporary runner did not execute (missing $RECORD)"
python3 - "$RECORD" "$SHOW" "$CARD_ID" "$RUN_ID" "$BOARD_SOCKET" "$HERDR_SOCKET_PATH" "$EXPECTED_PANE_CWD" <<'PY'
import json, sys
x=json.load(open(sys.argv[1])); show=json.load(open(sys.argv[2]))
card, run, board, herdr, cwd = sys.argv[3:]
assert x["card_id"] == int(card) and x["run_id"] == int(run)
assert x["argv"] == ["literal argument with spaces", "line one\nline two", "low"]
assert x["prompt"] == show["runs"][-1]["prompt_snapshot"]
assert x["prompt"].startswith("configured prompt with spaces\nand a newline\n\n")
assert x["system_prompt"].startswith("## herdr-board protocol\n")
assert "$BOARD_CARD_ID" in x["system_prompt"]
assert x["board_socket"] == board and x["herdr_socket"] == herdr
assert x["cwd"] == cwd
print("  configured argv preserved; card/run, BOARD_SYSTEM_PROMPT, sockets, cwd exact")
PY

step "Assert configured pane structure and explicit completion behavior"
tabs_json="$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")"
panes_json="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$tabs_json" "$panes_json" "$CARD_ID" <<'PY'
import json, re, sys
tabs=json.loads(sys.argv[1]).get("tabs",[]); panes=json.loads(sys.argv[2]).get("panes",[])
card=sys.argv[3]; kanban=[t for t in tabs if t.get("label")=="kanban"]
assert len(kanban)==1
kp=[p for p in panes if p.get("tab_id")==kanban[0]["tab_id"]]
assert any(re.search(rf"card-{card}-p17-runner(?:-r\d+)?$", p.get("label") or "") for p in kp)
print(f"  kanban tab {kanban[0]['tab_id']} has the configured runner pane")
PY
LAYOUT_PANE="$(printf '%s' "$panes_json" | python3 -c '
import json, re, sys
panes=json.load(sys.stdin).get("panes", [])
card=sys.argv[1]
matched=[p for p in panes if re.search(rf"card-{re.escape(card)}-p17-runner(?:-r\d+)?$", p.get("label") or "")]
assert len(matched) == 1, panes
print(matched[0]["pane_id"])
' "$CARD_ID")"
layout_json="$(hrpc pane.layout "{\"pane_id\":\"$LAYOUT_PANE\"}")"
python3 - "$layout_json" "$LAYOUT_PANE" <<'PY'
import json, sys
pane_ids={p["pane_id"] for p in json.loads(sys.argv[1]).get("layout", {}).get("panes", [])}
assert sys.argv[2] in pane_ids
print("  pane.layout contains the exact configured runner pane")
PY

echo "17-configured-p17-runner: argv/env bridge, bounded-held layout, and explicit done contract passed"
