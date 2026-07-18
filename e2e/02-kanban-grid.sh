#!/usr/bin/env bash
# 02-kanban-grid.sh — dispatch several cards into one auto column and assert the
# daemon tiles their agent panes into a single `kanban` tab (mesh grid).
#
# Asserts:
#   - exactly ONE tab labeled `kanban` in the workspace,
#   - one agent pane per card, each named `card-<id>-execute`,
#   - the kanban tab's root shell was consumed (pane_count == number of cards),
#   - the grid actually split — pane rects span more than one distinct x OR y.
#
# The fake agents HOLD their panes open (FAKE_AGENT_HOLD) so the live layout is
# inspectable; cleanup closes the workspace, tearing the panes down.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

NCARDS=3

export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"   # keep panes alive for layout asserts

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create bgrid; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create an auto column 'Execute'"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"

step "Create $NCARDS cards and move each into 'Execute' (dispatches agent panes)"
CARD_IDS=()
for i in $(seq 1 "$NCARDS"); do
  card_json="$("$BOARD_BIN" card new --title "Grid Card $i" -d "grid card $i" \
    --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
  cid="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id ($i)"
  CARD_IDS+=("$cid")
  mut "board move $cid Execute -> agent.start in $WS_ID"
  "$BOARD_BIN" move "$cid" Execute --json >/dev/null
  echo "  card $i -> $cid dispatched"
done

step "Wait for every run to report outcome ok"
for cid in "${CARD_IDS[@]}"; do
  oc="$(wait_ok "$cid")" || {
    echo "--- daemon log:"; tail -40 "$E2E_TMP/daemon.log" || true
    fail "card $cid outcome '$oc' (expected ok)"
  }
  echo "  card $cid: $oc"
done
sleep 1   # let the daemon settle the final pane placement

step "Structural assertions on the kanban tab (hrpc tab.list / pane.list)"
tabs_json="$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")"
panes_json="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"

KANBAN_PANE="$(python3 - "$tabs_json" "$panes_json" "$NCARDS" "${CARD_IDS[@]}" <<'PY'
import json, re, sys
tabs = json.loads(sys.argv[1]).get("tabs", [])
panes = json.loads(sys.argv[2]).get("panes", [])
ncards = int(sys.argv[3])
card_ids = sys.argv[4:]

kanban = [t for t in tabs if t.get("label") == "kanban"]
if len(kanban) != 1:
    sys.exit(f"expected exactly one 'kanban' tab, got {len(kanban)}: "
             f"{[t.get('label') for t in tabs]}")
ktab = kanban[0]
ktab_id = ktab["tab_id"]

kpanes = [p for p in panes if p.get("tab_id") == ktab_id]
pane_count = ktab.get("pane_count")
if pane_count != ncards:
    sys.exit(f"kanban tab pane_count={pane_count}, expected {ncards} "
             f"(root shell should be consumed)")
if len(kpanes) != ncards:
    sys.exit(f"found {len(kpanes)} panes in kanban tab, expected {ncards}")

# The daemon names each agent pane via its herdr label (card-<id>-<column-slug>);
# an agent_name_taken collision retries with a -r<run> suffix (see AGENTS.md), so
# match each expected card id allowing that optional suffix.
labels = sorted(p.get("label") or "" for p in kpanes)
matched = set()
for lbl in labels:
    m = re.match(r"^card-(\d+)-execute(-r\d+)?$", lbl)
    if m:
        matched.add(m.group(1))
expected = set(card_ids)
if matched != expected:
    sys.exit(f"agent pane labels {labels} -> card ids {sorted(matched)} "
             f"!= expected {sorted(expected)}")

print(f"[ok] one kanban tab ({ktab_id}), pane_count={pane_count}, "
      f"labels={labels}", file=sys.stderr)
# emit a kanban pane id for the layout probe
print(kpanes[0]["pane_id"])
PY
)" || fail "kanban-tab assertions failed"
ok "one kanban tab, $NCARDS agent panes named card-<id>-execute, root shell consumed"

step "Assert the grid actually split (pane.layout rects)"
layout_json="$(hrpc pane.layout "{\"pane_id\":\"$KANBAN_PANE\"}")"
python3 - "$layout_json" <<'PY' || fail "layout did not split into a grid"
import json, sys
# hrpc returns the raw `result`: {"type":"pane_layout","layout":{...}}
layout = json.loads(sys.argv[1]).get("layout", {})
lp = layout.get("panes", [])
print("  layout rects:", file=sys.stderr)
xs, ys = set(), set()
for p in lp:
    r = p.get("rect", {})
    xs.add(r.get("x")); ys.add(r.get("y"))
    print(f"    {p['pane_id']}: x={r.get('x')} y={r.get('y')} "
          f"w={r.get('width')} h={r.get('height')}", file=sys.stderr)
if len(xs) > 1 or len(ys) > 1:
    print(f"  [ok] grid split: {len(xs)} distinct x, {len(ys)} distinct y",
          file=sys.stderr)
    sys.exit(0)
sys.exit(f"panes not tiled: distinct x={xs}, y={ys}")
PY
ok "agent panes are tiled into a mesh grid"

step "02-kanban-grid: ALL CHECKS PASSED"
