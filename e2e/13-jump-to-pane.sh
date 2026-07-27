#!/usr/bin/env bash
# 13-jump-to-pane.sh — CLI focus and detail `o` reach the SELECTED same-session run pane.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"
e2e_init
e2e_build
e2e_isolate
e2e_daemon_start

step "Create a disposable target workspace and a held fake-agent pane"
e2e_ws_create jump-target; WS_ID="$E2E_WS"
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
card_json="$("$BOARD_BIN" card new --title jump-target --description 'focus this run' \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)"
e2e_board_herdr_mutate -- move "$CARD_ID" "$EXEC_ID" --json >/dev/null
outcome="$(wait_ok "$CARD_ID")" || fail "run did not finish (outcome '$outcome')"
[ "$outcome" = "ok" ] || fail "run outcome '$outcome' (expected ok)"

step "HERDR MUTATION: board retry $CARD_ID -> a SECOND run, so the card has a run history"
# Run selection in the TUI is only observable with more than one run: the
# scenario deliberately picks a NON-newest row below.
mut "board retry $CARD_ID"
e2e_board_herdr_mutate -- retry "$CARD_ID" --json >/dev/null || fail "board retry failed"
outcome2="$(wait_runs "$CARD_ID" 2)" || fail "retry did not spawn/finish a 2nd run"
[ "$outcome2" = "ok" ] || fail "retry run outcome '$outcome2' (expected ok)"

OLD_RUN="$(card_field "$CARD_ID" 'runs[-2].id')"
OLD_PANE="$(card_field "$CARD_ID" 'runs[-2].herdr_pane_id')"
TARGET_PANE="$(card_field "$CARD_ID" 'runs[-1].herdr_pane_id')"
# `run.focus` requires an explicit run id: read it from the same card detail.
TARGET_RUN="$(card_field "$CARD_ID" 'runs[-1].id')"
[ -n "$TARGET_PANE" ] || fail "run did not record a pane"
[ -n "$TARGET_RUN" ] || fail "card detail did not expose the run id"
[ -n "$OLD_RUN" ] && [ -n "$OLD_PANE" ] || fail "first run did not keep its identity"
[ "$OLD_RUN" != "$TARGET_RUN" ] || fail "retry did not create a distinct run"
[ "$OLD_PANE" != "$TARGET_PANE" ] || fail "retry reused the first run's pane"
hrpc pane.get "{\"pane_id\":\"$TARGET_PANE\"}" >/dev/null \
  || fail "held fake-agent pane is not accessible"
ok "target pane $TARGET_PANE remains alive after board done"

# The retry reclaims the first run's ended child pane (see
# spawner/placement.rs::reclaim_prior_children), so run $OLD_RUN keeps a
# recorded pane id that no longer exists. Make that premise deterministic
# instead of assuming the reclaim order: closing an already-closed disposable
# board-owned pane is a no-op.
if hrpc pane.get "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1; then
  mut "pane.close $OLD_PANE (disposable board-owned pane of the retried run)"
  e2e_hrpc_mutate -- pane.close "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1 || true
fi
for _ in $(seq 1 30); do
  hrpc pane.get "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1 || break
  sleep .1
done
! hrpc pane.get "{\"pane_id\":\"$OLD_PANE\"}" >/dev/null 2>&1 \
  || fail "older run's pane $OLD_PANE is still alive; the dead-pane case cannot be exercised"
ok "run $OLD_RUN still records the now-dead pane $OLD_PANE"

step "HERDR MUTATION: launch the real plugin overlay in the target workspace"
e2e_herdr_mutate -- --session "$E2E_SESSION" plugin link "$REPO_ROOT" >/dev/null
e2e_herdr_mutate -- workspace focus "$WS_ID" >/dev/null
e2e_hrpc_mutate -- pane.focus "{\"pane_id\":\"$TARGET_PANE\"}" >/dev/null
open_json="$(e2e_herdr_mutate -- plugin pane open --plugin herdr-board --entrypoint board \
  --placement overlay \
  --env "BOARD_SOCKET=$BOARD_SOCKET" --env "BOARD_DB=$BOARD_DB" \
  --env "HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG" \
  --env "BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH" --focus)"
BOARD_PANE="$(printf '%s' "$open_json" | jget pane_id)"

step "Wait for the overlay and select the run's column"
ready=0
for _ in $(seq 1 50); do
  screen="$("$HERDR_BIN" pane read "$BOARD_PANE" --source recent-unwrapped --lines 100 2>/dev/null || true)"
  # Match the column NAME only, not the desktop header's decoration: below 60
  # columns the board renders LayoutMode::Compact, whose header reads
  # `‹  [ ⇄ Todo  1/6 ]  ›` instead of `┌ Todo · 1 · manual ─┐`. Both contain the
  # column name, and this loop only needs to know the TUI painted its first
  # column — the visibility assertion below is unchanged.
  if printf '%s\n' "$screen" | grep -q 'Todo'; then
    ready=1
    break
  fi
  sleep .1
done
[ "$ready" = 1 ] || fail "board TUI did not render its initial column"
# The overlay is intentionally narrow in this workspace, so only the selected
# column is rendered. The run is in the second, auto-created Execute column;
# navigate before asserting its card is visible.
e2e_herdr_mutate -- pane send-keys "$BOARD_PANE" right
for _ in $(seq 1 50); do
  screen="$("$HERDR_BIN" pane read "$BOARD_PANE" --source recent-unwrapped --lines 100 2>/dev/null || true)"
  printf '%s\n' "$screen" | grep -q 'jump-target' && break
  sleep .1
done
if ! printf '%s\n' "$screen" | grep -q 'jump-target'; then
  # Keep startup failures actionable without turning the visibility assertion
  # into a weaker readiness check. These are bounded, read-only probes of the
  # exact overlay, its workspace, and the isolated board daemon.
  printf '  overlay diagnostics (pane=%s workspace=%s):\n' "$BOARD_PANE" "$WS_ID" >&2
  "$HERDR_BIN" pane get "$BOARD_PANE" >&2 || true
  printf '%s\n' '--- overlay recent-unwrapped ---' >&2
  "$HERDR_BIN" pane read "$BOARD_PANE" --source recent-unwrapped --lines 100 >&2 || true
  printf '%s\n' '--- target workspace panes ---' >&2
  hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" >&2 || true
  printf '%s\n' '--- board daemon log (tail 80) ---' >&2
  tail -n 80 "$E2E_TMP/daemon.log" >&2 || true
  fail "card not visible in board TUI"
fi

step "Focus the same run through the canonical CLI"
cli_focus_json="$(e2e_board_herdr_mutate -- card run focus "$CARD_ID" "$TARGET_RUN" --json)"
cli_focus_pane="$(printf '%s' "$cli_focus_json" | jget pane_id)"
[ "$cli_focus_pane" = "$TARGET_PANE" ] \
  || fail "CLI focus returned pane '$cli_focus_pane' (expected owned pane '$TARGET_PANE')"
cli_focus_run="$(printf '%s' "$cli_focus_json" | jget run_id)"
[ "$cli_focus_run" = "$TARGET_RUN" ] \
  || fail "CLI focus returned run '$cli_focus_run' (expected requested run '$TARGET_RUN')"
cli_focused=""
for _ in $(seq 1 60); do
  panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
  cli_focused="$(printf '%s' "$panes" | python3 -c '
import json,sys
try: ps=json.load(sys.stdin).get("panes",[])
except Exception: sys.exit(0)
for p in ps:
    if p.get("focused"):
        print(p.get("pane_id", "")); break
' 2>/dev/null || true)"
  [ "$cli_focused" = "$TARGET_PANE" ] && break
  sleep .1
done
[ "$cli_focused" = "$TARGET_PANE" ] \
  || fail "CLI focus left pane '$cli_focused' focused (expected '$TARGET_PANE')"
ok "board card run focus reached run $TARGET_RUN on owned pane $TARGET_PANE"

# The CLI focus intentionally leaves the plugin overlay open. Restore its focus
# so the existing detail `o` flow below remains an independent TUI assertion.
e2e_hrpc_mutate -- pane.focus "{\"pane_id\":\"$BOARD_PANE\"}" >/dev/null

step "Open card detail (the runs section now lists two runs)"
e2e_herdr_mutate -- pane send-keys "$BOARD_PANE" enter
detail_ready=0
for _ in $(seq 1 50); do
  detail_screen="$("$HERDR_BIN" pane read "$BOARD_PANE" --source recent-unwrapped --lines 100 2>/dev/null || true)"
  if printf '%s\n' "$detail_screen" | grep -q 'focus this run' \
    && printf '%s\n' "$detail_screen" | grep -q 'runs'; then
    detail_ready=1
    break
  fi
  sleep .1
done
[ "$detail_ready" = 1 ] || {
  printf '%s\n' '--- detail startup screen ---' >&2
  printf '%s\n' "$detail_screen" >&2
  fail "card detail did not open for jump-target"
}
# `o` asks boardd to focus/close Herdr panes, so gate daemon and session too.
e2e_process_identity_verify "$E2E_DAEMON_PID" "$E2E_DAEMON_IDENTITY" \
  || fail "refusing jump action: daemon identity does not match"

step "Select the OLDER run in the Runs section and press o (dead pane, must not jump)"
# Tab focuses the runs section; the cursor starts on the newest run, so one `up`
# lands on run $OLD_RUN — whose recorded pane was reclaimed. `o` must therefore
# refuse (proving it used the SELECTED row, not the newest one, which would have
# succeeded and closed this pane).
e2e_herdr_mutate -- pane send-keys "$BOARD_PANE" tab
e2e_herdr_mutate -- pane send-keys "$BOARD_PANE" up
e2e_herdr_mutate -- pane send-keys "$BOARD_PANE" o
sleep 1
panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
still_focused="$(printf '%s' "$panes" | python3 -c '
import json,sys
try: ps=json.load(sys.stdin).get("panes",[])
except Exception: sys.exit(0)
for p in ps:
    if p.get("focused"):
        print(p.get("pane_id", "")); break
' 2>/dev/null || true)"
[ "$still_focused" = "$BOARD_PANE" ] \
  || fail "selecting the dead-pane run left pane '$still_focused' focused (expected the board overlay $BOARD_PANE)"
printf '%s' "$panes" | python3 -c '
import json,sys
pid=sys.argv[1]
try: ps=json.load(sys.stdin).get("panes",[])
except Exception: sys.exit(1)
sys.exit(0 if any(p.get("pane_id")==pid for p in ps) else 1)
' "$BOARD_PANE" || fail "board overlay exited on a refused jump (it must stay usable)"
ok "o on the selected run $OLD_RUN refused the dead pane $OLD_PANE without quitting"

step "Move the cursor back to the newest run and press o"
e2e_herdr_mutate -- pane send-keys "$BOARD_PANE" down
e2e_herdr_mutate -- pane send-keys "$BOARD_PANE" o

step "Assert target pane focused and board pane exited"
focused=""
board_present=1
for _ in $(seq 1 60); do
  panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
  focused="$(printf '%s' "$panes" | python3 -c '
import json,sys
try: ps=json.load(sys.stdin).get("panes",[])
except Exception: sys.exit(0)
for p in ps:
    if p.get("focused"):
        print(p.get("pane_id", "")); break
' 2>/dev/null || true)"
  if printf '%s' "$panes" | python3 -c '
import json,sys
pid=sys.argv[1]
try: ps=json.load(sys.stdin).get("panes",[])
except Exception: sys.exit(1)
sys.exit(0 if any(p.get("pane_id")==pid for p in ps) else 1)
' "$BOARD_PANE"; then
    board_present=1
  else
    board_present=0
  fi
  [ "$focused" = "$TARGET_PANE" ] && [ "$board_present" = 0 ] && break
  sleep .1
done
[ "$focused" = "$TARGET_PANE" ] || fail "focused pane '$focused' (expected '$TARGET_PANE')"
[ "$board_present" = 0 ] || fail "board pane $BOARD_PANE remained after successful jump"
ok "o focused $TARGET_PANE and closed board pane $BOARD_PANE"

step "13-jump-to-pane: ALL CHECKS PASSED"
