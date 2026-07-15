#!/usr/bin/env bash
# 07-cancel.sh — `board cancel` kills a LIVE run's herdr pane and fails the card.
#
# The fake agent holds (FAKE_AGENT_SLEEP=30) so its pane is still alive when we
# cancel. Asserts:
#   - before cancel: the agent pane (card-<id>-execute) is present in the
#     workspace (hrpc pane.list);
#   - `board cancel <card>` kills that pane (gone from pane.list);
#   - the run's outcome is `cancelled` and the card status is `failed`
#     (cancel never transitions the card).
#
# Grounds: ops.rs::run_cancel -> finalize_run(Cancelled, kill=true,
# transition=false) -> spawner.kill(pane). Mirrors the crate test
# `cancel_running_card`, plus a live herdr pane-death assertion.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_SLEEP=30"   # keep the run live (not yet `done`)

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create an auto column 'Execute'"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
[ -n "$EXEC_ID" ] || fail "could not create Execute column"

step "Create a card and move it into 'Execute' (agent will hold the pane open)"
card_json="$("$BOARD_BIN" card new --title "Cancel Card" -d "cancel me" \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Execute -> agent.start in $WS_ID"
"$BOARD_BIN" move "$CARD_ID" Execute --json >/dev/null

# pane_present <card_id> — echo the matching agent pane label (card-<id>-execute[-r<n>])
# in WS_ID, or empty. Uses the raw herdr socket (labels live on panes, not the CLI).
pane_present() {
  hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" | python3 -c '
import json, re, sys
panes = json.load(sys.stdin).get("panes", [])
card = sys.argv[1]
want = re.compile(rf"^card-{re.escape(card)}-execute(-r\d+)?$")
print(next((p.get("label") for p in panes if p.get("label") and want.match(p["label"])), ""))
' "$1"
}

# Gate on the run's DB state, not just the pane: agent.start makes the pane visible
# a beat BEFORE the daemon commits start_run (started_at/herdr_pane_id). Cancelling
# in that window hits the QUEUED branch (finish, no kill) instead of killing the
# live pane. Also note: a bash pane reports agent_status "unknown", so the card
# never flips to `running` — `started_at` is the reliable "run is live" signal.
step "Wait until the run is actually STARTED (runs[-1].started_at set) and its pane is up"
STARTED=""
for _ in $(seq 1 60); do
  sa="$(card_field "$CARD_ID" runs[-1].started_at 2>/dev/null || true)"
  if [ -n "$sa" ] && [ "$sa" != "None" ] && [ "$sa" != "null" ] && [ -n "$(pane_present "$CARD_ID")" ]; then
    STARTED="$sa"; break
  fi
  sleep 0.5
done
[ -n "$STARTED" ] || { tail -40 "$E2E_TMP/daemon.log"; fail "run for card $CARD_ID never started (started_at unset)"; }
PANE_LABEL="$(pane_present "$CARD_ID")"
ok "run started (started_at=$STARTED); live agent pane present: $PANE_LABEL"

step "HERDR MUTATION: board cancel $CARD_ID -> kill the pane, fail the run"
mut "board cancel $CARD_ID"
"$BOARD_BIN" cancel "$CARD_ID" >/dev/null || fail "board cancel failed"

step "Assert the agent pane is gone (killed) from the workspace"
GONE=0
for _ in $(seq 1 40); do
  if [ -z "$(pane_present "$CARD_ID")" ]; then GONE=1; break; fi
  sleep 0.5
done
[ "$GONE" = "1" ] || { hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}"; fail "agent pane still present after cancel"; }
ok "agent pane killed on cancel"

step "Assert run outcome=cancelled and card status=failed (no transition)"
oc="$(wait_runs "$CARD_ID" 1)" || { tail -40 "$E2E_TMP/daemon.log"; fail "run never finalized"; }
[ "$oc" = "cancelled" ] || { "$BOARD_BIN" card show "$CARD_ID" --json; fail "run outcome '$oc', expected 'cancelled'"; }
st="$(card_field "$CARD_ID" card.status || true)"
col="$(card_field "$CARD_ID" card.column_id || true)"
[ "$st" = "failed" ] || fail "card status '$st', expected 'failed'"
[ "$col" = "$EXEC_ID" ] || fail "card moved to $col; cancel must not transition (expected $EXEC_ID)"
ok "run cancelled, card failed in place"

step "07-cancel: ALL CHECKS PASSED"
