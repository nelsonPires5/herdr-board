#!/usr/bin/env bash
# 25-card-tabs.sh — per-card tab ownership, restart reconstruction, and safe
# closed-tab recreation using only disposable provider-free Herdr resources.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

e2e_ws_create card-tabs; WS_ID="$E2E_WS"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"

card_json="$($BOARD_BIN card new --title 'Per-card tabs' --description 'tab ownership' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)"
CARD_TAB_LABEL="card-$CARD_ID"
# A duplicate label is a user tab, not an ownership claim.
USER_TAB="$(e2e_herdr_mutate -- tab create --workspace "$WS_ID" --label "$CARD_TAB_LABEL" --no-focus)"
USER_TAB_ID="$(printf '%s' "$USER_TAB" | jget tab_id)"
[ "$USER_TAB_ID" != "" ] || fail "user duplicate tab was not created"

step "First run creates a board-owned card tab, ignoring duplicate label"
e2e_board_herdr_mutate -- move "$CARD_ID" Execute --json >/dev/null
wait_ok "$CARD_ID" >/dev/null || fail "first card run did not finish"
RUN1_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
ANCHOR_PANE="$(python3 - "$BOARD_DB" "$CARD_ID" <<'PY'
import sqlite3, sys
conn=sqlite3.connect(sys.argv[1])
row=conn.execute(
    "SELECT herdr_anchor_pane_id FROM runs WHERE card_id=? ORDER BY id DESC LIMIT 1",
    (int(sys.argv[2]),),
).fetchone()
assert row and row[0]
print(row[0])
PY
)" || fail "first run did not persist its exact anchor pane"
TABS="$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")"
PANES="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
BOARD_TAB_ID="$(python3 - "$TABS" "$PANES" "$RUN1_PANE" "$ANCHOR_PANE" "$USER_TAB_ID" "$CARD_TAB_LABEL" <<'PY'
import json, sys
tabs=json.loads(sys.argv[1]).get('tabs',[]); panes=json.loads(sys.argv[2]).get('panes',[])
pane, anchor, user, label=sys.argv[3:]
owned=next(p for p in panes if p.get('pane_id')==pane)
tab=owned.get('tab_id')
assert tab != user
match=[t for t in tabs if t.get('tab_id')==tab]
assert len(match)==1 and match[0].get('label')==label
owned_panes=[p for p in panes if p.get('tab_id')==tab]
anchors=[p for p in owned_panes if p.get('pane_id') == anchor and not p.get('agent')]
assert len(anchors)==1
assert pane != anchor
assert sum(t.get('label')==label for t in tabs)==2
print(tab)
PY
)" || fail "first run adopted a duplicate-label user tab"
ok "board-owned tab $BOARD_TAB_ID is distinct from duplicate user tab $USER_TAB_ID"

step "Remove the only run child, retaining the durable exact anchor"
e2e_herdr_mutate -- pane close "$RUN1_PANE" >/dev/null
PANES="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$PANES" "$ANCHOR_PANE" <<'PY'
import json, sys
panes=json.loads(sys.argv[1]).get('panes',[])
assert any(p.get('pane_id') == sys.argv[2] and not p.get('agent') for p in panes)
PY
ok "exact durable anchor survives after its run child disappears"

step "Restart boardd and retry: reconstruct exact tab ownership from durable anchor"
e2e_daemon_stop
e2e_daemon_start
e2e_board_herdr_mutate -- retry "$CARD_ID" >/dev/null
wait_runs "$CARD_ID" 2 >/dev/null || fail "retry after daemon restart did not finish"
RUN2_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
PANES="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$PANES" "$RUN2_PANE" "$BOARD_TAB_ID" "$ANCHOR_PANE" <<'PY'
import json, sys
panes=json.loads(sys.argv[1]).get('panes',[])
pane, tab, anchor=sys.argv[2:]
assert next(p for p in panes if p.get('pane_id')==pane).get('tab_id') == tab
assert any(p.get('pane_id') == anchor and not p.get('agent') for p in panes if p.get('tab_id') == tab)
PY
ok "daemon restart reused the exact board-owned card tab"

step "Close only the exact anchor, then retry: recover it from the durable child"
e2e_herdr_mutate -- pane close "$ANCHOR_PANE" >/dev/null
e2e_board_herdr_mutate -- retry "$CARD_ID" >/dev/null
wait_runs "$CARD_ID" 3 >/dev/null || fail "retry after anchor close did not finish"
RUN3_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
PANES="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$PANES" "$RUN3_PANE" "$BOARD_TAB_ID" "$CARD_TAB_LABEL" "$ANCHOR_PANE" <<'PY'
import json, sys
panes=json.loads(sys.argv[1]).get('panes',[])
pane, tab, label, old_anchor=sys.argv[2:]
assert next(p for p in panes if p.get('pane_id')==pane).get('tab_id') == tab
owned=[p for p in panes if p.get('tab_id')==tab]
anchors=[p for p in owned if p.get('label')==f"{label}-anchor" and not p.get('agent')]
assert len(anchors)==1 and anchors[0].get('pane_id') != old_anchor
assert pane != anchors[0].get('pane_id')
PY
ok "missing anchor was recreated from the exact durable run child"

step "Close owned tab, then retry: recreate without adopting duplicate label"
e2e_herdr_mutate -- tab close "$BOARD_TAB_ID" >/dev/null
e2e_board_herdr_mutate -- retry "$CARD_ID" >/dev/null
wait_runs "$CARD_ID" 4 >/dev/null || fail "retry after tab close did not finish"
RUN4_PANE="$(card_field "$CARD_ID" runs[-1].herdr_pane_id)"
PANES="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$PANES" "$RUN4_PANE" "$USER_TAB_ID" "$BOARD_TAB_ID" "$CARD_TAB_LABEL" <<'PY'
import json, sys
panes=json.loads(sys.argv[1]).get('panes',[])
pane, user, old, label=sys.argv[2:]
new=next(p for p in panes if p.get('pane_id')==pane).get('tab_id')
assert new not in {user, old}
owned=[p for p in panes if p.get('tab_id')==new]
anchors=[p for p in owned if p.get('label')==f"{label}-anchor" and not p.get('agent')]
assert len(anchors)==1 and pane != anchors[0].get('pane_id')
PY
ok "closed owned tab was recreated without selecting a duplicate-label user tab"

step "25-card-tabs: exact ownership, restart reuse, and safe recreation PASSED"
