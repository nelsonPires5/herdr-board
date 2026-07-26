#!/usr/bin/env bash
# 02-kanban-grid.sh — dispatch several cards into one auto column and assert
# that each card gets its own stable `card-<id>` tab.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

NCARDS=3
export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"
e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

e2e_ws_create bgrid; WS_ID="$E2E_WS"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
CARD_IDS=()
for i in $(seq 1 "$NCARDS"); do
  card_json="$($BOARD_BIN card new --title "Grid Card $i" -d "grid card $i" \
    --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
  cid="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id ($i)"
  CARD_IDS+=("$cid")
  e2e_board_herdr_mutate -- move "$cid" Execute --json >/dev/null
done

for cid in "${CARD_IDS[@]}"; do
  oc="$(wait_ok "$cid")" || fail "card $cid outcome '$oc' (expected ok)"
  [ "$oc" = ok ] || fail "card $cid failed"
done
sleep 1

tabs_json="$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")"
panes_json="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$tabs_json" "$panes_json" "${CARD_IDS[@]}" <<'PY' || fail "per-card tab assertions failed"
import json, re, sys
tabs=json.loads(sys.argv[1]).get("tabs",[])
panes=json.loads(sys.argv[2]).get("panes",[])
ids=sys.argv[3:]
assert not any(t.get("label")=="kanban" for t in tabs)
for card in ids:
    label=f"card-{card}"
    matches=[t for t in tabs if t.get("label")==label]
    assert len(matches)==1
    tab=matches[0]
    owned=[p for p in panes if p.get("tab_id")==tab["tab_id"]]
    assert len(owned)>=2
    anchors=[p for p in owned if p.get("label")==f"card-{card}-anchor" and not p.get("agent")]
    assert len(anchors)==1
    assert any(re.match(rf"^card-{re.escape(card)}-execute(-r\d+)?$", p.get("label") or "") for p in owned)
print(f"[ok] {len(ids)} cards use separate card-<id> tabs", file=sys.stderr)
PY
ok "each card has a separate stable tab and pane"
step "02-kanban-grid: PER-CARD TABS PASSED"
