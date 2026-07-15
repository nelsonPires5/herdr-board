#!/usr/bin/env bash
# run-all.sh — boot ONE ephemeral herdr session, build once, run every live e2e
# scenario in order against it, print a PASS/FAIL/SKIP summary, tear the session
# down, and exit non-zero if ANY scenario FAILED.
#
# Ephemeral session: the suite never touches your real sessions. This boots a
# throwaway `hb-e2e-<pid>` session (via `herdr --session <name> server &`), and
# every scenario's isolated boardd binds to it (see lib.sh's session model), so
# no second running session is needed anymore. Cost: ~2s to boot the session.
#
# Usage:
#   e2e/run-all.sh                 run every scenario
#   e2e/run-all.sh --keep          keep sessions + each scenario's workspace for
#                                  review (also: E2E_KEEP=1). Prints a review block.
#   e2e/run-all.sh 04 07           only scenarios whose filename matches a filter
#   e2e/run-all.sh --keep 04       combine: keep mode + a single scenario
#
# Scenario exit convention: 0 = PASS, 3 = SKIP (missing precondition), anything
# else = FAIL. This is the stable entrypoint (scripts/e2e.sh forwards here).
# Live: needs a running herdr. NOT part of CI — see docs/testing.md.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
. "$DIR/lib.sh"

# --- args: --keep and optional scenario filters -----------------------------
KEEP=0
FILTERS=()
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    -h|--help) sed -n '2,20p' "$DIR/run-all.sh"; exit 0 ;;
    -*) echo "run-all.sh: unknown flag: $arg" >&2; exit 2 ;;
    *) FILTERS+=("$arg") ;;
  esac
done
if [ -n "${E2E_KEEP:-}" ] && [ "$E2E_KEEP" = "1" ]; then KEEP=1; fi

e2e_require
e2e_build   # build once; scenarios' own e2e_build is then a no-op

SCENARIOS=(
  01-core.sh
  02-kanban-grid.sh
  03-sessions.sh
  04-fail-on-fail.sh
  05-retry.sh
  06-silent-exit.sh
  07-cancel.sh
  08-column-timeout.sh
  09-comment-context.sh
)

# apply filters (substring match on the filename); empty = run all
run_this() {
  [ "${#FILTERS[@]}" -eq 0 ] && return 0
  local f
  for f in "${FILTERS[@]}"; do [[ "$1" == *"$f"* ]] && return 0; done
  return 1
}

# --- boot ONE ephemeral session for the whole run ---------------------------
RUN_SESSION="hb-e2e-$$"
[ "$KEEP" = 1 ] && export E2E_KEEP=1
step "Booting ephemeral herdr session '$RUN_SESSION' for the run (~2s)"
e2e_session_boot "$RUN_SESSION" E2E_SESSION_SOCKET RUN_SESSION_PID
export E2E_SESSION="$RUN_SESSION"
export E2E_SESSION_SOCKET
echo "  session socket: $E2E_SESSION_SOCKET (server pid $RUN_SESSION_PID)"

# Trap tears the session down on ANY exit (safety net for an aborted run). The
# normal path also calls it explicitly BEFORE the no-sessions-remain check, then
# clears the trap. e2e_session_teardown is a no-op under keep mode.
_run_torn=0
run_teardown() { [ "$_run_torn" = 1 ] && return 0; _run_torn=1; e2e_session_teardown "$RUN_SESSION" "$RUN_SESSION_PID"; }
trap run_teardown EXIT

declare -a NAMES RESULTS
rc_any_fail=0

for s in "${SCENARIOS[@]}"; do
  run_this "$s" || continue
  printf '\n############################################################\n'
  printf '# SCENARIO: %s\n' "$s"
  printf '############################################################\n'
  set +e
  bash "$DIR/$s"
  code=$?
  set -e 2>/dev/null || true
  case "$code" in
    0) verdict="PASS" ;;
    3) verdict="SKIP" ;;
    *) verdict="FAIL"; rc_any_fail=1 ;;
  esac
  NAMES+=("$s")
  RESULTS+=("$verdict")
  printf '\n>>> %s: %s (exit %d)\n' "$s" "$verdict" "$code"
done

printf '\n============================================================\n'
printf '# E2E SUMMARY\n'
printf '============================================================\n'
for i in "${!NAMES[@]}"; do
  printf '  %-6s %s\n' "${RESULTS[$i]}" "${NAMES[$i]}"
done

# --- session teardown + review ----------------------------------------------
if [ "$KEEP" = 1 ]; then
  printf '\n--- KEEP MODE: sessions + workspaces left for review -------\n'
  "$HERDR_BIN" session list --json 2>/dev/null | python3 -c '
import json, sys
kept = [s for s in json.load(sys.stdin).get("sessions", []) if s.get("name","").startswith("hb-e2e-")]
if not kept:
    print("  (no hb-e2e-* sessions found)"); sys.exit(0)
for s in kept:
    n = s["name"]
    running = s.get("running")
    print("  session {}  (running={})".format(n, running))
    print("    attach : herdr session attach {}".format(n))
    print("    cleanup: herdr session stop {} && herdr session delete {}".format(n, n))
'
  echo "  (each kept session holds this run's disposable workspace(s) — review, then run the cleanup line)"
else
  run_teardown
  trap - EXIT
  step "Verify no hb-e2e-* sessions remain"
  remaining="$("$HERDR_BIN" session list --json 2>/dev/null | python3 -c '
import json, sys
print(" ".join(s.get("name","") for s in json.load(sys.stdin).get("sessions", []) if s.get("name","").startswith("hb-e2e-")))')"
  if [ -n "$remaining" ]; then
    printf 'E2E FAIL: leftover ephemeral sessions: %s\n' "$remaining" >&2
    rc_any_fail=1
  else
    echo "  none — clean"
  fi
fi

if [ "$rc_any_fail" -ne 0 ]; then
  printf '\nRESULT: FAIL (at least one scenario failed)\n'
  exit 1
fi
printf '\nRESULT: OK (no failures)\n'
