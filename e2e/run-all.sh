#!/usr/bin/env bash
# run-all.sh — build once and run each live scenario in its own owned Herdr session.
# macOS ships Bash 3.2. Re-exec before parsing/sourcing any code that needs the
# associative arrays used by lib.sh; fixed absolute candidates remain available
# even when the caller intentionally supplies a system-only PATH.
if [ "${BASH_VERSINFO[0]:-0}" -lt 4 ]; then
  for _bash_candidate in "${BASH:-}" "$(command -v bash 2>/dev/null || true)" \
    /opt/homebrew/bin/bash /usr/local/bin/bash; do
    [ "${_bash_candidate#/}" != "$_bash_candidate" ] && [ -x "$_bash_candidate" ] || continue
    "$_bash_candidate" -c '(( BASH_VERSINFO[0] >= 4 ))' 2>/dev/null || continue
    exec "$_bash_candidate" "$0" "$@"
  done
  echo 'run-all.sh: an absolute Bash >= 4 is required' >&2
  exit 2
fi
unset _bash_candidate

set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$DIR/.." && pwd)"
# Pure helpers only; sourcing does not initialize or contact Herdr.
. "$DIR/lib.sh"

KEEP=0
REQUIRE_ALL=0
FILTERS=()
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    --require-all) REQUIRE_ALL=1 ;;
    -h|--help)
      echo "usage: e2e/run-all.sh [--keep] [--require-all] [scenario-filter ...]"
      exit 0 ;;
    -*) echo "run-all.sh: unknown flag: $arg" >&2; exit 2 ;;
    *) FILTERS+=("$arg") ;;
  esac
done
[ "${E2E_KEEP:-0}" = 1 ] && KEEP=1

# Artifact ownership is process-local. Never adopt, inspect, chmod, or write an
# inherited path: every invocation creates its own exact private root.
if [ "${E2E_ARTIFACT_ROOT+x}" = x ]; then
  echo 'run-all.sh: refusing inherited E2E_ARTIFACT_ROOT; artifact roots are always newly created' >&2
  exit 2
fi

# Resolve required non-system tools before entering the controlled standard PATH.
BOARD_BIN="${BOARD_BIN:-$REPO_ROOT/target/release/board}"
HERDR_BIN_PATH="${HERDR_BIN_PATH:-$(type -P herdr 2>/dev/null || true)}"
[[ "$HERDR_BIN_PATH" == /* ]] && [ -x "$HERDR_BIN_PATH" ] \
  || { echo 'run-all.sh: herdr must resolve to an absolute executable' >&2; exit 2; }
E2E_STANDARD_PATH=/usr/local/bin:/usr/bin:/bin
resolve_bash4() {
  local candidate
  for candidate in "${BASH:-}" "$(type -P bash 2>/dev/null || true)" \
    /opt/homebrew/bin/bash /usr/local/bin/bash; do
    [[ "$candidate" == /* ]] && [ -x "$candidate" ] || continue
    "$candidate" -c '(( BASH_VERSINFO[0] >= 4 ))' 2>/dev/null || continue
    printf '%s\n' "$candidate"
    return 0
  done
  return 1
}
E2E_BASH="$(resolve_bash4)" \
  || { echo 'run-all.sh: an absolute Bash >= 4 is required' >&2; exit 2; }
export E2E_STANDARD_PATH E2E_BASH
if [ ! -x "$BOARD_BIN" ] || [ "${E2E_FORCE_BUILD:-0}" = 1 ]; then
  "$E2E_BASH" "$REPO_ROOT/scripts/build.sh" || exit $?
fi

SCENARIOS=(
  01-core.sh 02-kanban-grid.sh 03-sessions.sh 04-fail-on-fail.sh
  05-retry.sh 06-silent-exit.sh 07-cancel.sh 08-column-timeout.sh
  09-comment-context.sh 10-archive-filter-title.sh 11-pi-harness.sh
  12-cwd-boards.sh 13-jump-to-pane.sh 14-column-config.sh 15-awaiting.sh
  16-managed-p17.sh 17-configured-p17-runner.sh 18-nullable-clear.sh
  19-daemon-before-herdr.sh 20-herdr-recovery.sh 21-active-run-timer.sh
)
run_this() {
  [ "${#FILTERS[@]}" -eq 0 ] && return 0
  local f
  for f in "${FILTERS[@]}"; do [[ "$1" == *"$f"* ]] && return 0; done
  return 1
}

umask 077
RUN_ROOT="$(mktemp -d /tmp/hb-e2e-run.XXXXXX)" \
  || { echo 'run-all.sh: cannot create private artifact root' >&2; exit 2; }
chmod 700 "$RUN_ROOT"
OWNER_ID="run-$$-$(python3 -c 'import secrets; print(secrets.token_hex(8))')"
INVOCATION_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
IDENTITY_KEY="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
printf 'herdr-board-e2e-artifacts\nowner=%s\ntoken=%s\n' "$OWNER_ID" "$INVOCATION_TOKEN" >"$RUN_ROOT/.owned-artifacts"
chmod 600 "$RUN_ROOT/.owned-artifacts"
: >"$RUN_ROOT/manifest-events.ndjson"
chmod 600 "$RUN_ROOT/manifest-events.ndjson"
declare -a NAMES RESULTS
rc_any_fail=0

for s in "${SCENARIOS[@]}"; do
  run_this "$s" || continue
  slug="${s%.sh}"
  artifact="$RUN_ROOT/$slug"
  mkdir -m 700 "$artifact"
  scenario_owner="$OWNER_ID/$slug"
  printf 'herdr-board-e2e-scenario-artifact\nowner=%s\ntoken=%s\n' \
    "$scenario_owner" "$INVOCATION_TOKEN" >"$artifact/.owned-artifact"
  chmod 600 "$artifact/.owned-artifact"
  printf '\n############################################################\n# SCENARIO: %s\n############################################################\n' "$s"
  child_env=(
    PATH="$E2E_STANDARD_PATH" LANG="${LANG:-C.UTF-8}"
    TERM="${TERM:-dumb}" TZ="${TZ:-UTC}"
    E2E_KEEP="$KEEP" E2E_TEST_FILE="$s" E2E_TEST_SLUG="$slug"
    E2E_OWNER_ID="$scenario_owner" E2E_SCENARIO_ARTIFACT_DIR="$artifact"
    E2E_INVOCATION_ARTIFACT_ROOT="$RUN_ROOT" E2E_INVOCATION_TOKEN="$INVOCATION_TOKEN"
    E2E_INVOCATION_OWNER_ID="$OWNER_ID" E2E_IDENTITY_KEY_BOOTSTRAP="$IDENTITY_KEY"
    E2E_STANDARD_PATH="$E2E_STANDARD_PATH" E2E_BASH="$E2E_BASH"
    BOARD_BIN="$BOARD_BIN" HERDR_BIN_PATH="$HERDR_BIN_PATH"
  )
  set +e
  env -i "${child_env[@]}" "$E2E_BASH" "$DIR/$s" 2>&1 | tee "$artifact/scenario.log"
  code=${PIPESTATUS[0]}
  set -e 2>/dev/null || true
  audit_code=0
  # Retained only in this parent shell; scenario bootstrap scrubbed its exported
  # copy before any Herdr/board/helper process was spawned.
  E2E_LOCAL_IDENTITY_KEY="$IDENTITY_KEY"
  e2e_audit_owned_manifest "$artifact/owned-resources.ndjson" "$KEEP" \
    >"$artifact/audit.log" 2>&1 || audit_code=$?
  printf '%s\n' "$audit_code" >"$artifact/audit.status"
  if [ "$audit_code" -ne 0 ]; then
    cat "$artifact/audit.log" >&2
    rc_any_fail=1
  fi
  verdict="$(e2e_suite_verdict "$code" "$audit_code" "$REQUIRE_ALL")"
  [ "$verdict" != FAIL ] || rc_any_fail=1
  NAMES+=("$s"); RESULTS+=("$verdict")
  printf '%s\n' "$code" >"$artifact/status"
  printf '\n>>> %s: %s (exit %d)\n' "$s" "$verdict" "$code"

done

printf '\n============================================================\n# E2E SUMMARY\n============================================================\n'
for i in "${!NAMES[@]}"; do printf '  %-6s %s\n' "${RESULTS[$i]}" "${NAMES[$i]}"; done
printf '  artifacts: %s\n' "$RUN_ROOT"
[ "$rc_any_fail" -eq 0 ] || { echo 'RESULT: FAIL'; exit 1; }
echo 'RESULT: OK (no failures)'
