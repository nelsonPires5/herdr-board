#!/usr/bin/env bash
# run-all.sh — build once, run every live e2e scenario in order, print a
# PASS/FAIL/SKIP summary, and exit non-zero if ANY scenario FAILED.
#
# Scenario exit convention: 0 = PASS, 3 = SKIP (missing precondition, e.g. no
# second running session), anything else = FAIL.
#
# This is the stable entrypoint (scripts/e2e.sh forwards here). Live: needs a
# running herdr. NOT part of CI — see docs/testing.md.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
. "$DIR/lib.sh"

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

declare -a NAMES RESULTS
rc_any_fail=0

for s in "${SCENARIOS[@]}"; do
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

if [ "$rc_any_fail" -ne 0 ]; then
  printf '\nRESULT: FAIL (at least one scenario failed)\n'
  exit 1
fi
printf '\nRESULT: OK (no failures)\n'
