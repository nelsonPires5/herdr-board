#!/usr/bin/env bash
# Deterministic/static safety checks for lib.sh; never starts Herdr or boardd.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

cleanup() {
  [ -z "${child:-}" ] || { kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; }
  [ -z "${E2E_SCENARIO_ROOT:-}" ] || rm -rf -- "$E2E_SCENARIO_ROOT"
  [ "${E2E_RESOURCE_MANIFEST_STANDALONE:-0}" != 1 ] || rm -f -- "${E2E_OWNED_RESOURCE_MANIFEST:-}"
}
trap cleanup EXIT

e2e_scenario_root_ensure
[[ "$E2E_SCENARIO_ROOT" == /tmp/h???????? ]]
[ "$HOME" = "$E2E_SCENARIO_ROOT" ]
[ "$(stat -c %a "$E2E_SCENARIO_ROOT")" = 700 ]
[ -f "$E2E_SCENARIO_ROOT/.disposable" ]
# Even matching-token inherited roots fail: ownership is process-local and the
# exact path/mode/header/owner/token contract is mandatory.
spoof=/tmp/h12345678; mkdir -m 700 "$spoof"
printf 'herdr-board-e2e\nowner=standalone-%s\ntoken=matching\n' "$$" >"$spoof/.disposable"
chmod 600 "$spoof/.disposable"
( unset E2E_LOCAL_SCENARIO_ROOT E2E_LOCAL_INVOCATION_TOKEN; E2E_SCENARIO_ROOT="$spoof"; \
  E2E_INVOCATION_TOKEN=matching; e2e_scenario_root_ensure ) >/dev/null 2>&1 \
  && { echo 'matching-token inherited root was accepted' >&2; exit 1; }
chmod 755 "$spoof"
( unset E2E_LOCAL_SCENARIO_ROOT; E2E_SCENARIO_ROOT="$spoof"; e2e_scenario_root_ensure ) \
  >/dev/null 2>&1 && { echo 'unsafe-mode inherited root was accepted' >&2; exit 1; }
rm -rf "$spoof"

# Injected failure after both early roots are ledgered must leave no root or
# standalone manifest behind.
early_log="$(mktemp /tmp/hb-e2e-early-log.XXXXXX)"
env -u E2E_INVOCATION_TOKEN -u E2E_SCENARIO_ROOT -u E2E_OWNED_RESOURCE_MANIFEST \
  -u E2E_RESOURCE_MANIFEST_STANDALONE E2E_TEST_INJECT_FAKE_SETUP_FAILURE=1 \
  E2E_TEST_EARLY_PATH_LOG="$early_log" \
  bash -c '. "$1/lib.sh"; trap e2e_cleanup EXIT; e2e_enable_fake_pi' _ "$E2E_LIB_DIR" \
  >/dev/null 2>&1 && { echo 'injected early failure unexpectedly passed' >&2; exit 1; }
mapfile -t early_paths <"$early_log"; rm -f "$early_log"
[ "${#early_paths[@]}" -eq 3 ]
for path in "${early_paths[@]}"; do [ ! -e "$path" ] || { echo "early resource leaked: $path" >&2; exit 1; }; done

managed_spoof="$(mktemp -d /tmp/hb-e2e-managed.XXXXXX)"; chmod 700 "$managed_spoof"
printf 'herdr-board fake-managed boundary\nowner=standalone-%s\ntoken=%s\n' \
  "$$" "$E2E_INVOCATION_TOKEN" >"$managed_spoof/.herdr-board-fake-managed"
chmod 600 "$managed_spoof/.herdr-board-fake-managed"
( unset E2E_LOCAL_MANAGED_ROOT; E2E_MANAGED_ROOT="$managed_spoof"; e2e_enable_fake_pi ) \
  >/dev/null 2>&1 && { echo 'matching-token inherited managed root was accepted' >&2; exit 1; }
rm -rf "$managed_spoof"

# Standalone callers cannot redirect ledger or artifact writes.
( E2E_OWNED_RESOURCE_MANIFEST=/tmp/arbitrary-ledger; unset E2E_LOCAL_RESOURCE_MANIFEST; \
  e2e_resource_manifest_init ) >/dev/null 2>&1 \
  && { echo 'inherited manifest redirect was accepted' >&2; exit 1; }
( E2E_SCENARIO_ARTIFACT_DIR=/tmp E2E_INVOCATION_ARTIFACT_ROOT=/tmp; \
  e2e_artifact_invocation_validate ) >/dev/null 2>&1 \
  && { echo 'unbounded artifact redirect was accepted' >&2; exit 1; }

E2E_TEST_SLUG='17-configured-p17-runner.sh'
name="$(e2e_session_name)"
[[ "$name" =~ ^hb-e2e-17-confi-[0-9]+-[0-9a-f]{16}$ ]]
# Herdr's fixed suffix observed by live preflight is 35 bytes. Keep the total
# socket at or below the harness's explicit 92-byte margin.
[ $((${#E2E_SCENARIO_ROOT} + 35 + ${#name})) -le 92 ]
secondary="$(e2e_session_name hb-e2e-b-)"
[[ "$secondary" =~ ^hb-e2e-17-con-b-[0-9]+-[0-9a-f]{16}$ ]]
[ "$secondary" != "$name" ]

owner="$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
env E2E_HERDR_OWNER_TOKEN="$owner" /bin/sleep 30 &
child=$!
identity="$(e2e_process_identity_capture "$child" /bin/sleep 30 /bin/sleep "$owner")"
e2e_process_identity_verify "$child" "$identity"
! e2e_process_identity_capture "$child" /bin/sleep 30 /bin/sleep wrong >/dev/null 2>&1
! e2e_process_identity_verify "$child" "${identity/\"owner_token\":\"$owner\"/\"owner_token\":\"wrong\"}"

# A provisional post-spawn capability is ledgered with exact child/start/exe/
# argv/owner evidence and refuses a mismatched fresh signal.
provisional_owner="$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
env E2E_HERDR_OWNER_TOKEN="$provisional_owner" /bin/sleep 30 & provisional_pid=$!
provisional_token="$(e2e_provisional_child_capture "$provisional_pid" "$provisional_owner")"
e2e_process_resource_register helper provisional-test "$provisional_pid" "$provisional_token"
bad_provisional="${provisional_token/\"owner_token\":\"$provisional_owner\"/\"owner_token\":\"wrong\"}"
e2e_provisional_child_abort provisional-test "$provisional_pid" "$bad_provisional" >/dev/null 2>&1 \
  && { echo 'provisional owner mismatch authorized signal' >&2; exit 1; }
[ -e "/proc/$provisional_pid" ]
e2e_provisional_child_abort provisional-test "$provisional_pid" "$provisional_token"
[ ! -e "/proc/$provisional_pid" ]

# RED regression for the old daemon pre-capture raw kill: force provisional
# capture to use a mismatched owner token, capture the refusal log, and prove
# e2e_daemon_start did not signal the still-live exact fixture process.
daemon_red_dir="$(mktemp -d /tmp/hb-e2e-daemon-red.XXXXXX)"
daemon_red_bin="$daemon_red_dir/board"
daemon_red_pidfile="$daemon_red_dir/pid"
daemon_red_signals="$daemon_red_dir/signals"
daemon_red_log="$daemon_red_dir/capture.log"
cat >"$daemon_red_bin" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$$" >"$E2E_TEST_DAEMON_PIDFILE"
trap 'printf TERM >>"$E2E_TEST_DAEMON_SIGNALS"; exit 0' TERM
trap 'printf HUP >>"$E2E_TEST_DAEMON_SIGNALS"; exit 0' HUP
while :; do sleep 1; done
FAKE
chmod 700 "$daemon_red_bin"
E2E_TEST_DAEMON_PIDFILE="$daemon_red_pidfile" E2E_TEST_DAEMON_SIGNALS="$daemon_red_signals" \
  E2E_TEST_DAEMON_RED_DIR="$daemon_red_dir" E2E_LIB_DIR="$E2E_LIB_DIR" \
  bash -c '
    set -euo pipefail
    . "$E2E_LIB_DIR/lib.sh"
    eval "$(declare -f e2e_provisional_child_capture | sed \
      "1s/e2e_provisional_child_capture/e2e_real_provisional_child_capture/")"
    e2e_provisional_child_capture() {
      e2e_real_provisional_child_capture "$1" definitely-wrong E2E_BOARD_DAEMON_OWNER_TOKEN
    }
    BOARD_BIN="$E2E_TEST_DAEMON_RED_DIR/board"
    E2E_TMP="$E2E_TEST_DAEMON_RED_DIR"
    e2e_daemon_start
  ' >"$daemon_red_log" 2>&1 \
  && { echo 'daemon owner-mismatch capture unexpectedly passed' >&2; exit 1; }
for _ in $(seq 1 50); do [ -s "$daemon_red_pidfile" ] && break; sleep .01; done
[ -s "$daemon_red_pidfile" ]
daemon_red_pid="$(<"$daemon_red_pidfile")"
[ -e "/proc/$daemon_red_pid" ] || { echo 'daemon capture failure signaled its unverified PID' >&2; exit 1; }
[ ! -s "$daemon_red_signals" ] || { echo 'daemon capture failure emitted a raw signal' >&2; exit 1; }
grep -F 'could not capture provisional exact-child evidence' "$daemon_red_log" >/dev/null
kill "$daemon_red_pid" 2>/dev/null || true
for _ in $(seq 1 50); do [ ! -e "/proc/$daemon_red_pid" ] && break; sleep .01; done
rm -rf "$daemon_red_dir"

# T00 regression: exercise the owned-session stop wait with a deterministic
# exact PID/session fixture, without starting Herdr. Under set -u the old
# arithmetic condition parsed `-e` as the unset variable `e`.
t00_name=t00-cleanup-loop
t00_herdr="$E2E_SCENARIO_ROOT/fake-herdr-t00"
cat >"$t00_herdr" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
  'session stop') kill "$E2E_T00_PID" ;;
  'session delete') rm -rf -- "$HOME/.config/herdr/sessions/$3" ;;
  *) exit 1 ;;
esac
FAKE
chmod 700 "$t00_herdr"
e2e_session_owner_marker_create "$t00_name"
(
  set -u
  /bin/sleep 30 &
  t00_pid=$!
  export E2E_T00_PID="$t00_pid"
  e2e_session_identity_verify() {
    local check_pid="$1" expected_name="${3:-}"
    [ "$check_pid" = "$t00_pid" ] && [ "$expected_name" = "$t00_name" ] \
      && [ -e "/proc/$check_pid" ]
  }
  t00_identity="$(python3 - "$t00_pid" "$t00_name" <<'PY'
import json,sys
pid,name=sys.argv[1:]
print(json.dumps({"pid":pid,"name":name,"session":name,"owner_token":"owned",
                  "expected_command":"/bin/sleep",
                  "cmdline":["/bin/sleep","--session",name,"server"]},
                 separators=(",",":"),sort_keys=True))
PY
)"
  mkdir -p "$HOME/.config/herdr/sessions/$t00_name"
  e2e_session_resource_register "$t00_name" "$t00_identity" \
    "$HOME/.config/herdr/sessions/$t00_name" "$E2E_SCENARIO_ROOT/sessions/$t00_name.owner"
  HERDR_BIN="$t00_herdr" e2e_session_abort_owned "$t00_name" "$t00_pid" "$t00_identity"
)

# Registration is idempotent and updates the token instead of adding a second
# stop/delete action (secondary-session callers repeat registration today).
E2E_CLEANUP=()
e2e_defer_session_teardown test-name "$child" "$identity"
e2e_defer_session_teardown test-name "$child" "$identity"
[ "${#E2E_CLEANUP[@]}" -eq 1 ]

for helper in e2e_herdr_mutate e2e_hrpc_mutate e2e_board_herdr_mutate \
  e2e_session_delete_authorized e2e_audit_owned_manifest e2e_clean_env; do
  declare -F "$helper" >/dev/null || { echo "missing safety helper: $helper" >&2; exit 1; }
done

# A token mentioning the session but lacking the exact
# `--session <exact> server` argv must never identify a Herdr server.
bad_owner=0123456789abcdef0123456789abcdef
env E2E_HERDR_OWNER_TOKEN="$bad_owner" python3 -c 'import time; time.sleep(30)' \
  --session exact not-server &
bad_server=$!
for _ in $(seq 1 20); do
  bad_token="$(e2e_process_identity_capture "$bad_server" exact exact python3 "$bad_owner" 2>/dev/null || true)"
  [ -n "$bad_token" ] && break
  sleep .01
done
kill "$bad_server" 2>/dev/null || true
wait "$bad_server" 2>/dev/null || true
[ -z "${bad_token:-}" ] || { echo 'non-server argv accepted as Herdr server identity' >&2; exit 1; }

# Inherited/shared sessions are refused, while a standalone path calls the same
# boot API and publishes only its newly returned socket/token.
( E2E_SESSION=inherited E2E_SESSION_SOCKET=/tmp/inherited; e2e_session_ensure ) \
  >/dev/null 2>&1 && { echo 'inherited session was adopted' >&2; exit 1; }
(
  unset E2E_SESSION E2E_SESSION_SOCKET E2E_SESSION_PID E2E_SESSION_IDENTITY
  E2E_STANDALONE_SESSION=standalone-exact
  e2e_session_boot() {
    [ "$1" = standalone-exact ] || return 1
    printf -v "$2" /tmp/standalone.sock
    printf -v "$3" 424242
    printf -v "$4" standalone-token
  }
  e2e_session_ensure >/dev/null
  [ "$E2E_SESSION" = standalone-exact ]
  [ "$E2E_SESSION_SOCKET" = /tmp/standalone.sock ]
  [ "$E2E_SESSION_IDENTITY" = standalone-token ]
)

# Cleanup traps precede fake-managed root creation in both managed scenarios.
python3 - "$E2E_LIB_DIR/11-pi-harness.sh" "$E2E_LIB_DIR/16-managed-p17.sh" <<'PY'
import sys
for path in sys.argv[1:]:
    text=open(path,encoding='utf-8').read()
    assert text.index('trap e2e_cleanup EXIT') < text.index('e2e_enable_fake_pi')
PY

# Spawn cleanup is armed/deferred before each race-prone full registration;
# session cleanup also precedes every readiness/publication check.
python3 - "$E2E_LIB_DIR/lib.sh" <<'PY'
import sys
s=open(sys.argv[1], encoding='utf-8').read()
body=s[s.index('e2e_session_boot() {'):s.index('\n# e2e_session_teardown', s.index('e2e_session_boot() {'))]
assert body.index('E2E_PROVISIONAL_CHILD_ARMED["$logical"]=1') < body.index("printf -v command 'e2e_provisional_child_abort") < body.index('e2e_defer "$command"') < body.index('e2e_process_resource_register helper "$logical"')
assert body.index('e2e_defer_session_teardown') < body.index('for (( i=0; i<75; i++ ))')
daemon=s[s.index('e2e_daemon_start() {'):s.index('\n# e2e_daemon_stop', s.index('e2e_daemon_start() {'))]
assert 'kill "$E2E_DAEMON_PID"' not in daemon
assert daemon.index('E2E_PROVISIONAL_CHILD_ARMED["$logical"]=1') < daemon.index("printf -v command 'e2e_provisional_child_abort") < daemon.index('e2e_defer "$command"') < daemon.index('e2e_process_resource_register helper "$logical"') < daemon.index('e2e_process_resource_register board-daemon board-daemon')
assert 'E2E_BOARD_DAEMON_OWNER_TOKEN="$owner_token"' in daemon
PY

# Cleanup failure propagates from an otherwise successful scenario.
( E2E_CLEANUP=('false'); true; e2e_cleanup ) >/dev/null 2>&1 \
  && { echo 'cleanup failure was hidden' >&2; exit 1; }

# Primary/secondary and daemon/session mismatches fail before fake commands run.
wrong_identity="${identity/\"owner_token\":\"$owner\"/\"owner_token\":\"wrong\"}"
( HERDR_BIN=/bin/true; e2e_herdr_mutate "$child" "$wrong_identity" /tmp/no.sock -- pane close p ) \
  >/dev/null 2>&1 && { echo 'secondary token mismatch authorized mutation' >&2; exit 1; }
( E2E_DAEMON_PID="$child" E2E_DAEMON_IDENTITY="$wrong_identity" BOARD_BIN=/bin/true; \
  e2e_board_herdr_mutate "$child" "$identity" -- move 1 Execute ) \
  >/dev/null 2>&1 && { echo 'daemon mismatch authorized board mutation' >&2; exit 1; }

# Post-stop delete authorization proves the exact PID is gone and requires the
# captured server identity plus exact private marker/name.
e2e_session_owner_marker_create exact-owned
dead_pid=99999999
dead_identity="$(python3 - "$identity" "$dead_pid" exact-owned <<'PY'
import json,sys
t=json.loads(sys.argv[1]); pid,name=sys.argv[2:]
t.update(pid=pid, session=name, name=name, expected_command='/fake/herdr', owner_token='owned')
t['cmdline']=['/fake/herdr','--session',name,'server']
print(json.dumps(t,separators=(',',':'),sort_keys=True))
PY
)"
mkdir -p "$HOME/.config/herdr/sessions/exact-owned"
e2e_session_resource_register exact-owned "$dead_identity" \
  "$HOME/.config/herdr/sessions/exact-owned" "$E2E_SCENARIO_ROOT/sessions/exact-owned.owner"
HERDR_BIN=/bin/true e2e_session_delete_authorized exact-owned "$dead_pid" "$dead_identity"
e2e_session_resource_release exact-owned
live_identity="$(python3 - "$dead_identity" "$child" <<'PY'
import json,sys
t=json.loads(sys.argv[1]); t['pid']=sys.argv[2]; print(json.dumps(t,separators=(',',':'),sort_keys=True))
PY
)"
HERDR_BIN=/bin/true e2e_session_delete_authorized exact-owned "$child" "$live_identity" >/dev/null 2>&1 \
  && { echo 'delete was authorized while exact PID remained' >&2; exit 1; }
e2e_session_owner_marker_create wrong
sed -i 's/^name=wrong$/name=other/' "$E2E_SCENARIO_ROOT/sessions/wrong.owner"
wrong_identity="$(python3 - "$dead_identity" <<'PY'
import json,sys
t=json.loads(sys.argv[1]); t.update(session='wrong',name='wrong'); t['cmdline']=['/fake/herdr','--session','wrong','server']; print(json.dumps(t,separators=(',',':'),sort_keys=True))
PY
)"
mkdir -p "$HOME/.config/herdr/sessions/wrong"
e2e_session_resource_register wrong "$wrong_identity" \
  "$HOME/.config/herdr/sessions/wrong" "$E2E_SCENARIO_ROOT/sessions/wrong.owner"
HERDR_BIN=/bin/true e2e_session_delete_authorized wrong "$dead_pid" "$wrong_identity" >/dev/null 2>&1 \
  && { echo 'mismatched delete marker was accepted' >&2; exit 1; }
e2e_session_resource_release wrong

# require-all turns SKIP into failure and audits propagate independently.
[ "$(e2e_suite_verdict 3 0 0)" = SKIP ]
[ "$(e2e_suite_verdict 3 0 1)" = FAIL ]
[ "$(e2e_suite_verdict 0 1 0)" = FAIL ]

# Exact-resource accounting covers every supported kind. Each fake leak must
# fail while present and pass after removal; no prefix/global inventory is used.
audit_dir="$(mktemp -d /tmp/hb-e2e-audit.XXXXXX)"
audit="$audit_dir/owned.ndjson"
E2E_OWNED_RESOURCE_MANIFEST="$audit"
E2E_LOCAL_RESOURCE_MANIFEST="$audit"
E2E_RESOURCE_GENERATIONS=(); E2E_RESOURCE_CURRENT_IDS=()

assert_audit_fails() {
  e2e_audit_owned_manifest "$audit" 0 >/dev/null 2>&1 \
    && { echo "recorded $1 leak escaped audit" >&2; exit 1; }
  return 0
}
reset_audit() {
  : >"$audit"
  E2E_RESOURCE_GENERATIONS=(); E2E_RESOURCE_CURRENT_IDS=()
}

# Boardd/helper/proxy processes carry the complete captured identity token.
for role in board-daemon helper proxy; do
  reset_audit
  e2e_process_resource_register "$role" "$role-main" "$child" "$identity"
  assert_audit_fails "$role process"
  e2e_process_resource_release "$role-main"
  kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; child=""
  e2e_audit_owned_manifest "$audit" 0
  env E2E_HERDR_OWNER_TOKEN="$owner" /bin/sleep 30 & child=$!
  identity="$(e2e_process_identity_capture "$child" /bin/sleep 30 /bin/sleep "$owner")"
done

# Session records include the exact server identity and exact registry/marker.
reset_audit
session_registry="$audit_dir/session-registry"
session_marker="$audit_dir/session.owner"
mkdir "$session_registry"; printf 'session-owner\n' >"$session_marker"
session_identity="$(python3 - "$identity" exact-session <<'PY'
import json,sys
t=json.loads(sys.argv[1]); name=sys.argv[2]
t.update(session=name,name=name,expected_command='/fake/herdr',owner_token='owned')
t['cmdline']=['/fake/herdr','--session',name,'server']
print(json.dumps(t,separators=(',',':'),sort_keys=True))
PY
)"
e2e_session_resource_register exact-session "${session_identity/\"pid\":\"$child\"/\"pid\":\"99999999\"}" \
  "$session_registry" "$session_marker"
assert_audit_fails 'session registry/marker'
e2e_session_resource_release exact-session
rm -rf "$session_registry" "$session_marker"
e2e_audit_owned_manifest "$audit" 0

# Scenario/managed roots require disposable ownership-marker evidence.
for role in scenario managed; do
  reset_audit
  root="$audit_dir/$role-root"; mkdir "$root"
  marker="$root/.owned"; printf '%s-owner\n' "$role" >"$marker"
  e2e_root_resource_register "$role" "$role-root" "$root" "$marker"
  assert_audit_fails "$role root"
  e2e_root_resource_release "$role-root"
  rm -rf "$root"
  e2e_audit_owned_manifest "$audit" 0
done

# Workspace ownership is exact marker evidence, not a workspace inventory scan.
reset_audit
workspace_marker="$audit_dir/workspace-exact.owned"
printf 'workspace exact evidence\n' >"$workspace_marker"
e2e_workspace_resource_register workspace-exact "$workspace_marker"
assert_audit_fails 'workspace marker'
printf 'tampered\n' >"$workspace_marker"
! e2e_marker_resource_verify workspace workspace-exact "$workspace_marker" \
  || { echo 'replacement marker authorized destructive cleanup' >&2; exit 1; }
assert_audit_fails 'tampered workspace marker'
e2e_workspace_resource_release workspace-exact
rm "$workspace_marker"
e2e_audit_owned_manifest "$audit" 0

# Configured runners and daemon-generated scripts carry a non-sensitive content
# digest; replacement is refused immediately before deletion.
E2E_TMP="$audit_dir"
for role in configured-runner temp-script; do
  reset_audit
  script="$audit_dir/$role.sh"; printf '#!/bin/sh\n' >"$script"
  e2e_script_resource_register "$role" "$role-main" "$script"
  assert_audit_fails "$role"
  printf '#!/bin/sh\n# replacement\n' >"$script"
  e2e_script_remove_owned "$role-main" "$script" >/dev/null 2>&1 \
    && { echo 'replacement script was deleted' >&2; exit 1; }
  [ -f "$script" ]
  e2e_script_resource_release "$role-main"
  rm "$script"
  e2e_audit_owned_manifest "$audit" 0
done

# Replacement/restart accounting retains both generations, catches a leaked
# prior generation, and allows a clean replacement plus release.
reset_audit
env E2E_HERDR_OWNER_TOKEN="$owner" /bin/sleep 30 & replacement=$!
replacement_identity="$(e2e_process_identity_capture "$replacement" /bin/sleep 30 /bin/sleep "$owner")"
e2e_process_resource_register helper restartable "$child" "$identity"
e2e_process_resource_register helper restartable "$replacement" "$replacement_identity"
assert_audit_fails 'secondary/restart replacement'
e2e_process_resource_release restartable
kill "$replacement" "$child" 2>/dev/null || true
wait "$replacement" 2>/dev/null || true; wait "$child" 2>/dev/null || true
replacement=""; child=""
e2e_audit_owned_manifest "$audit" 0

# Malformed records and invalid lifecycle transitions fail closed.
for malformed in \
  'not-json' \
  '{"version":1,"op":"register","kind":"process"}' \
  '{"version":1,"op":"release","resource_id":"unknown"}'; do
  printf '%s\n' "$malformed" >"$audit"
  assert_audit_fails malformed
 done
rm -f "$audit"
assert_audit_fails 'missing registry'

# Production creation/cleanup sites are wired to the same ledger, including
# settled session replacement, daemon, roots, workspace markers, and runner.
python3 - "$E2E_LIB_DIR/lib.sh" "$E2E_LIB_DIR/17-configured-p17-runner.sh" <<'PY'
import sys
lib=open(sys.argv[1],encoding='utf-8').read()
runner=open(sys.argv[2],encoding='utf-8').read()
assert lib.count('e2e_session_resource_register "$name"') >= 2
assert 'e2e_process_resource_register board-daemon board-daemon' in lib
assert 'e2e_root_resource_register scenario scenario-root' in lib
assert 'e2e_root_resource_register managed managed-root' in lib
assert 'e2e_root_resource_register scenario scenario-temp' in lib
assert 'e2e_workspace_resource_register "$ws" "$marker"' in lib
assert 'export TMPDIR="$E2E_TMP"' in lib
assert 'e2e_script_resource_register configured-runner p17-configured-runner' in runner
assert 'p17-runner-script' not in runner
PY

# Standalone and run-all select the same manifest-backed accounting path.
reset_audit
(
  unset E2E_SCENARIO_ARTIFACT_DIR E2E_INVOCATION_ARTIFACT_ROOT E2E_OWNED_RESOURCE_MANIFEST \
    E2E_LOCAL_RESOURCE_MANIFEST E2E_RESOURCE_MANIFEST_STANDALONE
  e2e_resource_manifest_init
  [ -f "$E2E_OWNED_RESOURCE_MANIFEST" ]
  e2e_audit_owned_manifest "$E2E_OWNED_RESOURCE_MANIFEST" 0
  rm -f -- "$E2E_OWNED_RESOURCE_MANIFEST"
)
rm -rf "$audit_dir"

# Failure diagnostics and Python assertion messages must never retain payload
# objects or prompt/system-prompt values in scenario.log. In particular, do not
# print a cached full `card show --json` object on an assertion branch.
! rg -n '\$BOARD_BIN" card show.*--json.*(>&2|\|\| true|; fail)|json\.tool.*record|prompt_snapshot.*\\n.*snap' \
  "$E2E_LIB_DIR"/[0-9][0-9]-*.sh >/dev/null \
  || { echo 'unsafe sensitive diagnostic remains' >&2; exit 1; }
python3 - "$E2E_LIB_DIR/04-fail-on-fail.sh" <<'PY'
import re,sys
s=open(sys.argv[1],encoding='utf-8').read()
assert not re.search(r'''printf\s+'%s(?:\\n)?'\s+"\$show"''',s)
PY
! rg -n '^\s*assert\b.*,[[:space:]]*(x(\[[^]]+\])?|prompt|system_prompt|show\[[^]]+\])[[:space:]]*$' \
  "$E2E_LIB_DIR"/[0-9][0-9]-*.sh "$E2E_LIB_DIR"/real-*-smoke.sh "$E2E_LIB_DIR"/fake-bin/{pi,claude} >/dev/null \
  || { echo 'unsafe sensitive assertion message remains' >&2; exit 1; }
python3 - "$E2E_LIB_DIR" <<'PY'
import ast,pathlib,re,sys
root=pathlib.Path(sys.argv[1])
for path in [*root.glob('[0-9][0-9]-*.sh'), *root.glob('real-*-smoke.sh'), root/'fake-bin/pi', root/'fake-bin/claude']:
    text=path.read_text(encoding='utf-8')
    for match in re.finditer(r"<<'PY'\n(.*?)\nPY(?:\n|$)", text, re.S):
        tree=ast.parse(match.group(1))
        assert not any(isinstance(n,ast.Assert) and n.msg is not None for n in ast.walk(tree)), path
# Scanner self-test: all specifically prohibited payload message forms match.
unsafe=['assert ok, x','assert ok, x["prompt"]','assert ok, prompt','assert ok, system_prompt','assert ok, show["runs"][-1]']
pattern=re.compile(r'^\s*assert\b.*,[ \t]*(?:x(?:[ \t]|$|\[)|prompt\b|system_prompt\b|show\[)')
assert all(pattern.search(value) for value in unsafe)
PY

# The allowlist scrubs arbitrary supported/future provider variables and opt-ins.
OPENAI_API_KEY=x ANTHROPIC_API_KEY=x GOOGLE_API_KEY=x AWS_ACCESS_KEY_ID=x \
AZURE_OPENAI_API_KEY=x COHERE_API_KEY=x MISTRAL_API_KEY=x GROQ_API_KEY=x \
OPENROUTER_API_KEY=x TOGETHER_API_KEY=x XAI_API_KEY=x E2E_REAL_PI=1 \
E2E_REAL_CLAUDE_HAIKU=1 e2e_clean_env bash -c '
  test -z "${OPENAI_API_KEY:-}${ANTHROPIC_API_KEY:-}${GOOGLE_API_KEY:-}${AWS_ACCESS_KEY_ID:-}"
  test -z "${AZURE_OPENAI_API_KEY:-}${COHERE_API_KEY:-}${MISTRAL_API_KEY:-}${GROQ_API_KEY:-}"
  test -z "${OPENROUTER_API_KEY:-}${TOGETHER_API_KEY:-}${XAI_API_KEY:-}${E2E_REAL_PI:-}${E2E_REAL_CLAUDE_HAIKU:-}"
'

# Standard scenarios may keep read-only probes direct, but all known Herdr and
# daemon-triggering mutation verbs must pass through the wrappers.
python3 - "$E2E_LIB_DIR" <<'PY'
import pathlib,re,sys
root=pathlib.Path(sys.argv[1])
files=sorted(root.glob('[0-9][0-9]-*.sh'))
herdr=re.compile(r'\$HERDR_BIN" (?:workspace (?:create|close|focus)|tab create|pane (?:run|send-keys|send-text|report-agent)|plugin pane open|--session .* plugin link)')
board=re.compile(r'\$BOARD_BIN" (?:move|retry|cancel|done)')
for p in files:
    text=p.read_text()
    # Configured-agent callback fixture is not scenario-side infrastructure.
    if "cat >\"$RUNNER\" <<'RUNNER'" in text:
        before, rest=text.split("cat >\"$RUNNER\" <<'RUNNER'", 1)
        _, after=rest.split("\nRUNNER\n", 1)
        text=before+after
    assert not herdr.search(text), f'direct Herdr mutation in {p}'
    assert not board.search(text), f'direct board/Herdr mutation in {p}'
PY

echo 'e2e harness static safety checks: PASS'
