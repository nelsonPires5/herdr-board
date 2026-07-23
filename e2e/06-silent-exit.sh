#!/usr/bin/env bash
# 06-silent-exit.sh — an agent pane that exits WITHOUT `board done` fails the run,
# and (per the watchers design) does NOT auto-transition the card.
#
# The fake agent runs with FAKE_AGENT_SILENT=1: it sleeps, then `exit 0` before
# ever commenting or calling `board done` (simulates a crashed agent). herdr emits
# a pane-exited event; the daemon finalizes the open run as failed WITHOUT
# applying any on_success/on_fail transition. Asserts:
#   - the run's outcome is `fail`;
#   - the card status is `failed`;
#   - the card did NOT move (still in the auto column it was dispatched into),
#     even though that column HAS an on_fail target — pane-exit is not a
#     `board done --outcome fail`, so the transition rules are deliberately skipped;
#   - a `system` comment "pane exited without board done" is recorded.
#
# Grounds: watchers.rs PaneExited / local_liveness_poller -> finalize_run(Fail,
# transition=false). Mirrors the crate test `process_exit_without_done`.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_SILENT=1"   # exit without ever calling board done

e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "HERDR MUTATION: create disposable workspace"
e2e_ws_create board-e2e; WS_ID="$E2E_WS"
echo "  workspace: $WS_ID"

step "Create a manual 'Backlog' and an auto 'Execute' WITH on_fail->Backlog"
# Give Execute an on_fail target on purpose: a silent exit must NOT follow it.
BACKLOG_ID="$(col_create '{"name":"Backlog","trigger":"manual"}')"
EXEC_ID="$(col_create "{\"name\":\"Execute\",\"trigger\":\"auto\",\"on_fail_column_id\":$BACKLOG_ID}")"
[ -n "$EXEC_ID" ] || fail "could not create Execute column"
echo "  Execute id: $EXEC_ID (on_fail -> Backlog $BACKLOG_ID)"

step "Create a card and move it into 'Execute' (agent will exit silently)"
card_json="$("$BOARD_BIN" card new --title "Silent Card" -d "crashes" \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Execute -> agent.start in $WS_ID"
e2e_board_herdr_mutate -- move "$CARD_ID" Execute --json >/dev/null

step "Wait for the daemon to detect the pane exit and fail the run"
oc="$(wait_runs "$CARD_ID" 1)" || { fail "run never finalized after silent exit"; }
[ "$oc" = "fail" ] || { e2e_card_failure_diag "$CARD_ID"; fail "run outcome '$oc', expected 'fail'"; }
ok "run finalized as fail after the pane exited"

step "Assert NO auto-transition: card stayed in 'Execute' and is 'failed'"
col_now="$(card_field "$CARD_ID" card.column_id || true)"
st_now="$(card_field "$CARD_ID" card.status || true)"
echo "  card column_id=$col_now status=$st_now (want column=$EXEC_ID status=failed)"
[ "$col_now" = "$EXEC_ID" ] || { e2e_card_failure_diag "$CARD_ID"; fail "card moved to $col_now; pane-exit must NOT apply on_fail (expected still $EXEC_ID)"; }
[ "$st_now" = "failed" ] || fail "card status '$st_now', expected 'failed'"
ok "card did not transition (still in Execute), status failed"

step "Assert the 'pane exited without board done' system comment"
"$BOARD_BIN" card show "$CARD_ID" --json | grep -q "pane exited without board done" \
  || { e2e_card_failure_diag "$CARD_ID"; fail "missing 'pane exited without board done' comment"; }
ok "system comment records the silent exit"

step "06-silent-exit: ALL CHECKS PASSED"
