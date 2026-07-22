#!/usr/bin/env bash
# 09-comment-context.sh — a comment from one auto stage flows into the next
# stage's prompt.
#
# Two chained auto columns: Stage1 (on_success -> Stage2) and Stage2. The fake
# agent posts a distinctive marker comment (FAKE_AGENT_COMMENT) and reports ok in
# EACH stage. When the card auto-advances from Stage1 to Stage2, the daemon
# rebuilds the prompt from the card's comments, so Stage2's run captures Stage1's
# marker. Asserts:
#   - two run rows exist (one per stage), both finished ok;
#   - the Stage2 run's `prompt_snapshot` contains a `## Card comments` section AND
#     the Stage1 marker text.
#
# Grounds: prompt.rs::assemble_prompt (adds "## Card comments" + recent comments),
# dispatch.rs::enqueue_run re-reads db.list_comments before each run;
# runs.prompt_snapshot is exposed via `board card show --json`.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

MARKER="E2E-CTX-MARKER-$$"
export E2E_FAKE_ENV="FAKE_AGENT_COMMENT=${MARKER}"   # each stage comments the marker

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create two chained auto columns: Stage1 (on_success -> Stage2) and Stage2"
STAGE2_ID="$(col_create '{"name":"Stage2","trigger":"auto"}')"          # create target first
STAGE1_ID="$(col_create "{\"name\":\"Stage1\",\"trigger\":\"auto\",\"on_success_column_id\":$STAGE2_ID}")"
[ -n "$STAGE1_ID" ] && [ -n "$STAGE2_ID" ] || fail "could not create the two stage columns"
echo "  Stage1 id=$STAGE1_ID (on_success -> Stage2 $STAGE2_ID)"

step "Create a card and move it into 'Stage1' (marker='$MARKER')"
card_json="$("$BOARD_BIN" card new --title "Ctx Card" -d "context flow" \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Stage1 -> agent.start; on ok auto-advances to Stage2"
e2e_board_herdr_mutate -- move "$CARD_ID" Stage1 --json >/dev/null

step "Wait for BOTH stage runs to finish (chained auto advance)"
oc="$(wait_runs "$CARD_ID" 2)" || { e2e_card_failure_diag "$CARD_ID"; fail "second (Stage2) run never finished"; }
echo "  last (Stage2) run outcome: $oc"
[ "$oc" = "ok" ] || fail "Stage2 run outcome '$oc', expected ok"

step "Assert the Stage2 run's prompt_snapshot carries the Stage1 comment context"
"$BOARD_BIN" card show "$CARD_ID" --json | python3 -c '
import json, sys
marker = sys.argv[1]
d = json.load(sys.stdin)
runs = d.get("runs", [])
if len(runs) < 2:
    sys.exit(f"expected >=2 runs, got {len(runs)}")
last = runs[-1]
snap = last.get("prompt_snapshot") or ""
if "## Card comments" not in snap:
    sys.exit(f"Stage2 prompt_snapshot missing comments section (length={len(snap)})")
if marker not in snap:
    sys.exit(f"Stage2 prompt_snapshot missing fixed marker (length={len(snap)})")
print(f"  [ok] Stage2 prompt_snapshot contains \"## Card comments\" and the marker {marker}", file=sys.stderr)
' "$MARKER" || { e2e_card_failure_diag "$CARD_ID"; fail "comment context did not flow into Stage2 prompt"; }
ok "Stage1 comment flowed into Stage2's prompt via the '## Card comments' section"

step "09-comment-context: ALL CHECKS PASSED"
