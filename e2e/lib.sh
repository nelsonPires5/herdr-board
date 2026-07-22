#!/usr/bin/env bash
# lib.sh — shared harness for the herdr-board LIVE e2e scenarios.
#
# SOURCE this from a scenario (01-core.sh, 02-kanban-grid.sh, …); it is not meant
# to be executed directly. It provides:
#   - logging (step / mut / fail / ok / skip),
#   - path + tool resolution and preconditions (e2e_require),
#   - an idempotent release build (e2e_build),
#   - an EPHEMERAL herdr session per run (never your real sessions — see below),
#   - an ISOLATED stack (short /tmp DB + socket + config with the fake harness),
#   - trap-based cleanup you register as you go (e2e_defer),
#   - daemon start/stop by pid, disposable workspace create (auto-closed),
#   - a card-outcome poller (wait_ok) and JSON helpers (jget),
#   - raw herdr RPC via e2e/hrpc.py (hrpc), honoring HERDR_SOCKET_PATH.
#
# EPHEMERAL SESSION MODEL: the suite NEVER touches your real herdr sessions. Each
# run gets its own throwaway session named `hb-e2e-<pid>-<random>-<random>` (started via
# `herdr --session <name> server &`). The isolated boardd binds to it
# (HERDR_SOCKET_PATH=<its socket>), so its "default session" IS the ephemeral one,
# and every herdr CLI call + hrpc assert targets it too. run-all boots ONE session
# and exports E2E_SESSION / E2E_SESSION_SOCKET for all scenarios; a scenario run
# standalone boots (and tears down) its own. Teardown stops+deletes the session
# unless keep mode is on (--keep / E2E_KEEP=1), which also skips workspace close.
#
# Conventions kept from the original scripts/e2e.sh (now e2e/): `set -euo pipefail` in the
# scenario, `step`/`mut` echo narration, every herdr MUTATION prefixed
# "HERDR MUTATION:". Mutations only ever hit disposable workspaces this suite
# created inside the ephemeral session; never a workspace/session you care about.

# --- logging ----------------------------------------------------------------
step() { printf '\n=== %s\n' "$*"; }
mut()  { printf 'HERDR MUTATION: %s\n' "$*"; }
ok()   { printf '  OK: %s\n' "$*"; }
fail() { printf 'E2E FAIL: %s\n' "$*" >&2; exit 1; }
# skip: bail out of a scenario WITHOUT failing (missing precondition). run-all
# treats exit code 3 as SKIP.
skip() { printf 'SKIP: %s\n' "$*" >&2; exit 3; }

# --- paths & tools ----------------------------------------------------------
E2E_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$E2E_LIB_DIR/.." && pwd)"
HERDR_BIN="${HERDR_BIN_PATH:-herdr}"
BOARD_BIN="${BOARD_BIN:-$REPO_ROOT/target/release/board}"
E2E_FAKE_AGENT="${E2E_FAKE_AGENT:-$E2E_LIB_DIR/fake-agent.sh}"
HRPC="$E2E_LIB_DIR/hrpc.py"
E2E_FAKE_PI_BIN_DIR="$E2E_LIB_DIR/fake-bin"
export BOARD_BIN

# Scope the checked-in executables named exactly `pi` and `claude` to disposable
# e2e Herdr servers. The candidate board binary is also on PATH so the fakes can
# call `board comment` / `board done` without a custom built-in harness env.
# Managed panes get a separate HOME/ZDOTDIR below; the caller's HOME remains
# untouched so Herdr's own session registry/discovery continues to use it.
e2e_enable_fake_pi() {
  [ -x "$E2E_FAKE_PI_BIN_DIR/pi" ] || fail "fake pi missing/not executable"
  [ -x "$E2E_FAKE_PI_BIN_DIR/claude" ] || fail "fake claude missing/not executable"
  export E2E_FAKE_PI_BIN_DIR
  export PATH="$E2E_FAKE_PI_BIN_DIR:$REPO_ROOT/target/release:$PATH"
  export E2E_FAKE_MANAGED_FUNCTIONS=1 E2E_FAKE_MANAGED_ZDOT=1
  # Do not trust or reuse a user's HOME, ZDOTDIR, PATH, or shell startup files
  # in a fake-managed pane. Reuse this directory when run-all's child scenario
  # adopts its already-booted disposable session.
  if [[ "${E2E_MANAGED_ROOT:-}" != /tmp/hb-e2e-managed.* ]] \
    || [ ! -f "${E2E_MANAGED_ROOT:-}/.herdr-board-fake-managed" ] \
    || [ ! -d "${E2E_MANAGED_ROOT:-}/home" ] \
    || [ ! -d "${E2E_MANAGED_ROOT:-}/zdot" ]; then
    E2E_MANAGED_ROOT="$(mktemp -d /tmp/hb-e2e-managed.XXXXXX)"
    printf 'herdr-board fake-managed boundary\n' >"$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
    chmod 600 "$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
    mkdir -m 700 "$E2E_MANAGED_ROOT/home" "$E2E_MANAGED_ROOT/zdot"
    # A child scenario inherits this PID, so it can adopt the root but cannot
    # mistake itself for the shell that is allowed to remove it.
    E2E_MANAGED_ROOT_CREATOR_PID=$$
    export E2E_MANAGED_ROOT E2E_MANAGED_ROOT_CREATOR_PID
  fi
  E2E_MANAGED_HOME="$E2E_MANAGED_ROOT/home"
  E2E_MANAGED_ZDOTDIR="$E2E_MANAGED_ROOT/zdot"
  export E2E_MANAGED_HOME E2E_MANAGED_ZDOTDIR
  # Keep only the checked-in fixtures, board binary, and system utilities in a
  # pane's PATH. The parent PATH is intentionally retained for the Herdr CLI;
  # this narrower one is what managed shells actually receive.
  export E2E_MANAGED_PATH="$E2E_FAKE_PI_BIN_DIR:$REPO_ROOT/target/release:/usr/local/bin:/usr/bin:/bin"
  {
    printf 'export HOME=%q\n' "$E2E_MANAGED_HOME"
    printf 'export PATH=%q\n' "$E2E_MANAGED_PATH"
    printf 'export BASH_ENV=/dev/null ENV=/dev/null\n'
    printf 'pi() { exec %q/pi "$@"; }\n' "$E2E_FAKE_PI_BIN_DIR"
    printf 'claude() { exec %q/claude "$@"; }\n' "$E2E_FAKE_PI_BIN_DIR"
  } >"$E2E_MANAGED_ZDOTDIR/.zshenv"
  cp "$E2E_MANAGED_ZDOTDIR/.zshenv" "$E2E_MANAGED_ZDOTDIR/.zshrc"
  cp "$E2E_MANAGED_ZDOTDIR/.zshenv" "$E2E_MANAGED_HOME/.bashrc"
  cp "$E2E_MANAGED_ZDOTDIR/.zshenv" "$E2E_MANAGED_HOME/.bash_profile"
  cp "$E2E_MANAGED_ZDOTDIR/.zshenv" "$E2E_MANAGED_HOME/.profile"
  chmod 600 "$E2E_MANAGED_ZDOTDIR/.zshenv" "$E2E_MANAGED_ZDOTDIR/.zshrc" \
    "$E2E_MANAGED_HOME/.bashrc" "$E2E_MANAGED_HOME/.bash_profile" "$E2E_MANAGED_HOME/.profile"
  # Interactive .bashrc files can define provider functions that take
  # precedence over PATH. Export exact exec functions too, so Bash resolves
  # these names to the checked-in fixtures even before its controlled rc runs.
  pi() { exec "$E2E_FAKE_PI_BIN_DIR/pi" "$@"; }
  claude() { exec "$E2E_FAKE_PI_BIN_DIR/claude" "$@"; }
  export -f pi claude
}

# Resolve Herdr while the caller's PATH is still available. Managed panes use a
# deliberately hermetic PATH, so pass them this resolved path rather than adding
# a user's bin directory to E2E_MANAGED_PATH. Leave an unresolved value intact so
# e2e_require retains its actionable missing-binary error.
e2e_resolve_herdr_bin() {
  [ "${HERDR_BIN_RESOLVED:-0}" = "1" ] && return 0
  local configured="$HERDR_BIN" resolved=""
  # `command -v` reports shell functions, which would let an inherited function
  # replace the real Herdr CLI. `type -P` deliberately searches executables only.
  # An explicit absolute HERDR_BIN_PATH is retained verbatim for auditability.
  if [[ "$configured" == /* ]]; then
    resolved="$configured"
  else
    resolved="$(type -P "$configured" 2>/dev/null || true)"
  fi
  if [[ "$resolved" == /* ]] && [ -x "$resolved" ]; then
    HERDR_BIN="$resolved"
  fi
  HERDR_BIN_RESOLVED=1
}

# e2e_require — verify the tools every scenario needs.
e2e_require() {
  e2e_resolve_herdr_bin
  [[ "$HERDR_BIN" == /* ]] && [ -x "$HERDR_BIN" ] \
    || fail "herdr CLI must resolve to an absolute executable ($HERDR_BIN)"
  command -v python3 >/dev/null 2>&1 || fail "python3 required (JSON parsing / hrpc.py)"
  [ -f "$E2E_FAKE_AGENT" ] || fail "fake agent missing at $E2E_FAKE_AGENT"
  [ -f "$HRPC" ] || fail "hrpc.py missing at $HRPC"
}

# Protocol-17 preflight. Keep this exact and early: no scenario may dispatch
# against a protocol-16 or unknown/future Herdr, since the wire launch contract
# is intentionally not backward compatible. The ping evidence is printed so a
# RED run records both the CLI version and the socket's negotiated protocol.
e2e_protocol_preflight() {
  local version ping protocol reported_version
  version="$($HERDR_BIN --version 2>&1 || true)"
  printf '  Herdr preflight: %s\n' "$version"
  [ "$version" = "herdr 0.7.5" ] \
    || fail "requires exactly Herdr 0.7.5 (got: $version)"
  ping="$(hrpc ping '{}')" \
    || fail "Herdr protocol preflight ping failed"
  protocol="$(printf '%s' "$ping" | python3 -c 'import json,sys; print(json.load(sys.stdin)["protocol"])' 2>/dev/null || true)"
  reported_version="$(printf '%s' "$ping" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version", ""))' 2>/dev/null || true)"
  printf '  Herdr ping evidence: version=%s protocol=%s payload=%s\n' \
    "$reported_version" "$protocol" "$ping"
  [ "$reported_version" = "0.7.5" ] \
    || fail "requires Herdr 0.7.5 from ping (got: $reported_version)"
  [ "$protocol" = "17" ] \
    || fail "requires Herdr protocol 17 (got: ${protocol:-missing})"
}

# e2e_build — build the release binary if absent (or E2E_FORCE_BUILD=1). cargo is
# a no-op when nothing changed; run-all.sh builds once up front.
e2e_build() {
  if [ -x "$BOARD_BIN" ] && [ "${E2E_FORCE_BUILD:-0}" != "1" ]; then
    return 0
  fi
  step "Building the board binary"
  bash "$REPO_ROOT/scripts/build.sh"
  [ -x "$BOARD_BIN" ] || fail "board binary not built at $BOARD_BIN"
}

# --- JSON helpers -----------------------------------------------------------
# jget <key> — read JSON on stdin, print the first value found for <key>
# (recursive, scalar-only). Program passed via -c so piped JSON stays on stdin.
_JGET_PY='
import json, sys
key = sys.argv[1]
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(1)
def walk(o):
    if isinstance(o, dict):
        if key in o and not isinstance(o[key], (dict, list)):
            return o[key]
        for v in o.values():
            r = walk(v)
            if r is not None:
                return r
    elif isinstance(o, list):
        for v in o:
            r = walk(v)
            if r is not None:
                return r
    return None
r = walk(data)
if r is None:
    sys.exit(1)
print(r)
'
jget() { python3 -c "$_JGET_PY" "$1"; }

# hrpc <method> [json-params] — one-shot herdr RPC (honors HERDR_SOCKET_PATH).
# Prints the raw `result` JSON on stdout.
hrpc() { python3 "$HRPC" "$@"; }

# --- boardd RPC (columns have no CLI verb) ----------------------------------
# brpc <method> [json-params] — one-shot boardd RPC via scripts/board-rpc.py,
# with the protocol ENVELOPE stripped: prints just the `result` payload as one
# JSON line (board-rpc.py otherwise prints the whole {"id":..,"result":..} line,
# whose top-level "id" is the request id "rpc", not the column's numeric id).
BOARD_RPC_BIN="$REPO_ROOT/scripts/board-rpc.py"
brpc() {
  python3 "$BOARD_RPC_BIN" "$@" | python3 -c '
import json, sys
line = sys.stdin.readline()
try:
    r = json.loads(line)
except Exception:
    sys.stdout.write(line); sys.exit(0)
print(json.dumps(r.get("result", r) if isinstance(r, dict) else r))'
}

# col_create <json-params> — create a column via column.create and print its
# numeric id. Pass the full params object, e.g.
#   FAIL=$(col_create '{"name":"Backlog","trigger":"manual"}')
#   col_create "{\"name\":\"Execute\",\"trigger\":\"auto\",\"on_fail_column_id\":$FAIL}"
col_create() {
  local params
  params="$(python3 -c '
import json, sys
p = json.loads(sys.argv[1])
p.setdefault("board_id", int(sys.argv[2]))
print(json.dumps(p))
' "$1" "${E2E_BOARD_ID:?e2e_daemon_start must run before col_create}")"
  brpc column.create "$params" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])'
}

# --- chained-run poller -----------------------------------------------------
# wait_runs <card_id> <min_runs> [max_halfsecs] — poll `board card show --json`
# until the card has at least <min_runs> run rows AND the LAST run has ended
# (outcome set). Prints the last run's outcome; returns 0 regardless of outcome
# (callers assert the specific outcome themselves). Use for chained auto columns
# or retries where more than one run row is expected (wait_ok only sees the first
# run's outcome via jget).
wait_runs() {
  local card="$1" min="$2" tries="${3:-80}" i
  for (( i=0; i<tries; i++ )); do
    "$BOARD_BIN" card show "$card" --json 2>/dev/null | python3 -c '
import json, sys
try:
    runs = json.load(sys.stdin).get("runs", [])
except Exception:
    sys.exit(1)
need = int(sys.argv[1])
if len(runs) >= need and runs[-1].get("outcome") not in (None, "null"):
    print(runs[-1]["outcome"]); sys.exit(0)
sys.exit(1)
' "$min" && return 0
    sleep 0.5
  done
  printf '<timeout>'
  return 1
}

# card_field <card_id> <dotted.path> — print a scalar from `card card show --json`.
# Supports card.* (e.g. status, column_id) and runs[-1].* (last run). Returns
# non-zero if absent.
card_field() {
  "$BOARD_BIN" card show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
path = sys.argv[1]
if path.startswith("runs[-1]."):
    runs = d.get("runs", [])
    if not runs: sys.exit(1)
    v = runs[-1].get(path.split(".",1)[1])
elif path.startswith("card."):
    v = d.get("card", {}).get(path.split(".",1)[1])
else:
    v = d.get(path)
if v is None: sys.exit(1)
print(v)
' "$2"
}

# --- cleanup registry -------------------------------------------------------
# Register teardown commands as you create things; e2e_cleanup runs them in
# REVERSE (LIFO) on EXIT so workspaces close before the daemon stops. Call
# e2e_init once, early, to install the trap.
E2E_CLEANUP=()
e2e_defer() { E2E_CLEANUP+=("$*"); }
e2e_cleanup() {
  local rc=$? cleanup_rc=0 i
  step "Cleanup"
  for (( i=${#E2E_CLEANUP[@]}-1; i>=0; i-- )); do
    # Keep running every LIFO cleanup after a failure. A scenario failure remains
    # authoritative, but a successful scenario must expose cleanup failures.
    if ! eval "${E2E_CLEANUP[$i]}"; then
      cleanup_rc=1
    fi
  done
  echo "  done"
  [ "$rc" -ne 0 ] && return "$rc"
  return "$cleanup_rc"
}
e2e_init() {
  # Install cleanup before any precondition or session work.  A standalone
  # fake-managed run may fail in e2e_require, before it has a server teardown
  # to remove the root it just created.
  trap e2e_cleanup EXIT
  if [ -z "${E2E_SESSION:-}" ] && [ -z "${E2E_SESSION_SOCKET:-}" ] \
     && [ -z "${E2E_MANAGED_ROOT_OWNER:-}" ]; then
    E2E_STANDALONE_SESSION="$(e2e_session_name 'hb-e2e-')"
    export E2E_STANDALONE_SESSION
    export E2E_MANAGED_ROOT_OWNER="$E2E_STANDALONE_SESSION"
  fi
  if [ -n "${E2E_MANAGED_ROOT_OWNER:-}" ]; then
    e2e_defer "e2e_managed_root_remove_owned '${E2E_MANAGED_ROOT_OWNER}'"
  fi
  e2e_require
  e2e_session_ensure
  e2e_protocol_preflight
}

# --- ephemeral herdr session ------------------------------------------------
# A PID alone is not process ownership: it can be reused. Keep a JSON token
# bound to Linux /proc's start time, executable, expected argv identity, and
# complete argv. JSON also keeps the token safe to quote through deferred cleanup.
e2e_process_identity_capture() {
  local pid="$1" session="$2" name="$3" expected_command="${4:-}"
  [ -r "/proc/$pid/stat" ] && [ -r "/proc/$pid/cmdline" ] && [ -L "/proc/$pid/exe" ] || return 1
  python3 - "$pid" "$session" "$name" "$expected_command" <<'PY'
import json, os, sys
pid, session, name, expected_command = sys.argv[1:]
try:
    stat = open(f"/proc/{pid}/stat", encoding="utf-8").read()
    # comm can contain spaces/parentheses; after its final ')' index 19 is field 22.
    start_time = stat[stat.rfind(")") + 2:].split()[19]
    exe = os.readlink(f"/proc/{pid}/exe")
    argv = [part.decode("utf-8", "surrogateescape")
            for part in open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0") if part]
except (IndexError, OSError, UnicodeError):
    raise SystemExit(1)
# Both expected values must be argv elements: a PID-only token is insufficient.
# For a server launched through `env`, wait until it has exec'd the command; a
# script's shebang leaves the script path at argv[1], hence the first-two rule.
if (session not in argv or name not in argv
        or (expected_command and expected_command not in argv[:2])):
    raise SystemExit(1)
print(json.dumps({"pid": pid, "start_time": start_time, "exe": exe,
                  "session": session, "name": name, "cmdline": argv},
                 sort_keys=True, ensure_ascii=True, separators=(",", ":")))
PY
}

e2e_process_identity_verify() {
  local pid="$1" token="$2"
  [ -n "$token" ] && [ -r "/proc/$pid/stat" ] && [ -r "/proc/$pid/cmdline" ] && [ -L "/proc/$pid/exe" ] || return 1
  python3 - "$pid" "$token" <<'PY'
import json, os, sys
pid, token = sys.argv[1:]
try:
    recorded = json.loads(token)
    required = {"pid", "start_time", "exe", "session", "name", "cmdline"}
    if set(recorded) != required or recorded["pid"] != pid:
        raise ValueError("invalid identity token")
    if not all(isinstance(recorded[key], str) for key in required - {"cmdline"}):
        raise ValueError("invalid identity fields")
    if not isinstance(recorded["cmdline"], list) or not all(isinstance(v, str) for v in recorded["cmdline"]):
        raise ValueError("invalid argv")
    stat = open(f"/proc/{pid}/stat", encoding="utf-8").read()
    start_time = stat[stat.rfind(")") + 2:].split()[19]
    exe = os.readlink(f"/proc/{pid}/exe")
    argv = [part.decode("utf-8", "surrogateescape")
            for part in open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0") if part]
    if (start_time != recorded["start_time"] or exe != recorded["exe"]
            or argv != recorded["cmdline"]
            or recorded["session"] not in argv or recorded["name"] not in argv):
        raise ValueError("identity changed")
except (IndexError, OSError, UnicodeError, ValueError, TypeError, json.JSONDecodeError):
    raise SystemExit(1)
PY
}

# Defer with shell-escaped JSON, not interpolation inside an eval string.
e2e_defer_session_teardown() {
  local name="$1" pid="$2" identity="$3" command
  printf -v command 'e2e_session_teardown %q %q %q' "$name" "$pid" "$identity"
  e2e_defer "$command"
}

# e2e_session_name <prefix> — make a collision-resistant name while retaining
# the hb-e2e-* prefix used by the explicit cleanup audit.
e2e_session_name() {
  printf '%s%s-%s-%s' "$1" "$$" "$RANDOM" "$RANDOM"
}

# e2e_session_name_absent <name> — fail closed if session enumeration fails or
# an exact name already exists. This check must run before starting a server.
# Names are random, so even a stale registry entry is a collision rather than a
# reason to probe or bypass the registry.
e2e_session_name_absent() {
  local name="$1" sessions found
  sessions="$("$HERDR_BIN" session list --json 2>/dev/null)" \
    || fail "could not enumerate Herdr sessions before booting '$name'"
  found="$(printf '%s' "$sessions" | python3 -c '
import json, sys
name = sys.argv[1]
try:
    sessions = json.load(sys.stdin).get("sessions", [])
except Exception:
    sys.exit(2)
print(int(any(session.get("name") == name for session in sessions)))
' "$name")" \
    || fail "could not parse Herdr sessions before booting '$name'"
  [ "$found" = 1 ] \
    && fail "refusing to boot: Herdr session '$name' already exists"
  return 0
}

# Remove only the fake-managed root owned by this exact primary session and
# created by this shell. The marker and narrow /tmp path prevent a malformed
# inherited env from broadening cleanup; a child scenario inherits a different
# creator PID and can therefore never remove run-all's root. Missing roots are
# already-cleaned, not an error, so the early guard and normal teardown compose.
e2e_managed_root_remove_owned() {
  local name="$1" root="${E2E_MANAGED_ROOT:-}" creator="${E2E_MANAGED_ROOT_CREATOR_PID:-}"
  [ "${E2E_MANAGED_ROOT_OWNER:-}" = "$name" ] || return 0
  if [ -n "$creator" ]; then
    [ "$creator" = "$$" ] || return 0
  else
    # Compatibility for the original non-random standalone name only. Current
    # roots always carry the creator PID above, so an inherited random root
    # without it is never considered ours.
    [ "$name" = "hb-e2e-$$" ] || return 0
  fi
  [ -e "$root" ] || return 0
  case "$root" in
    /tmp/hb-e2e-managed.*)
      [ -d "$root" ] && [ -f "$root/.herdr-board-fake-managed" ] \
        || { printf 'E2E FAIL: refusing unmarked managed-root cleanup: %s\n' "$root" >&2; return 1; }
      rm -rf -- "$root"
      ;;
    *)
      printf 'E2E FAIL: refusing managed-root cleanup outside /tmp: %s\n' "$root" >&2
      return 1
      ;;
  esac
}

# e2e_session_abort_owned <name> <pid> <identity> — process-owned cleanup.
# Never stops, deletes, or signals unless /proc still exactly matches the token.
e2e_session_abort_owned() {
  local name="$1" pid="$2" identity="${3:-}" cleanup_rc=0
  if ! e2e_process_identity_verify "$pid" "$identity"; then
    echo "  refusing session cleanup for '$name': server identity does not match" >&2
    return 1
  fi
  mut "session stop+delete '$name'"
  if ! "$HERDR_BIN" session stop "$name" >/dev/null 2>&1; then
    printf 'E2E FAIL: session stop failed for %s\n' "$name" >&2
    cleanup_rc=1
  fi
  if ! "$HERDR_BIN" session delete "$name" >/dev/null 2>&1; then
    printf 'E2E FAIL: session delete failed for %s\n' "$name" >&2
    cleanup_rc=1
  fi
  # This one exact verification authorizes the contiguous teardown. session stop
  # commonly exits the server before a second /proc verification is possible.
  if e2e_process_identity_verify "$pid" "$identity"; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  if ! e2e_managed_root_remove_owned "$name"; then
    cleanup_rc=1
  fi
  return "$cleanup_rc"
}

# e2e_session_boot <name> <sockvar> <pidvar> — start an ephemeral herdr server
# for session <name> (`herdr --session <name> server &`), wait (~15s) for its
# socket to accept a tab-less workspace.list, then assign the socket path to
# $sockvar and the server pid to $pidvar in the CALLER's scope. Do NOT call via
# $(...) — a command-substitution subshell would drop the pid and thus its
# teardown (same gotcha as e2e_ws_create).
e2e_session_boot() {
  local name="$1" sockvar="$2" pidvar="$3" identityvar="${4:-}" sock="" i _pid identity
  e2e_session_name_absent "$name"
  mut "session boot '$name' (herdr --session $name server &)"
  if [ "${E2E_FAKE_MANAGED_ZDOT:-0}" = "1" ]; then
    # Herdr itself keeps the caller's HOME so its disposable session remains
    # discoverable through the normal session registry. Only shells inside
    # that session receive the generated, self-contained startup environment;
    # it never sources $HOME/.zshrc or any other user rc file.
    env -u BASH_ENV -u ENV ZDOTDIR="$E2E_MANAGED_ZDOTDIR" \
      "$HERDR_BIN" --session "$name" server >/dev/null 2>&1 &
  else
    "$HERDR_BIN" --session "$name" server >/dev/null 2>&1 &
  fi
  _pid=$!
  printf -v "$pidvar" '%s' "$_pid"
  # Capture before readiness probing, while this shell's child identity is fresh.
  # `env ... herdr` can briefly expose env's argv, so wait only for the spawned
  # PID to exec the requested command (never adopt another process/session).
  identity=""
  for (( i=0; i<25; i++ )); do
    identity="$(e2e_process_identity_capture "$_pid" "$name" "$name" "$HERDR_BIN")" && break
    sleep 0.02
  done
  if [ -z "$identity" ]; then
    # This is still our direct child: kill/wait it without asking Herdr to
    # mutate a session whose process identity was never captured. The root
    # cleanup is separately marker/owner checked and does not use the name.
    kill "$_pid" 2>/dev/null || true
    wait "$_pid" 2>/dev/null || true
    e2e_managed_root_remove_owned "$name" || true
    fail "refusing ephemeral session '$name': could not capture server identity"
  fi
  [ -z "$identityvar" ] || printf -v "$identityvar" '%s' "$identity"
  for (( i=0; i<75; i++ )); do   # 75 * 0.2s = ~15s
    # A coincident/replacement session is never ours. Check the exact spawned
    # PID before each possible socket adoption and once more before returning.
    if ! e2e_process_identity_verify "$_pid" "$identity"; then
      e2e_managed_root_remove_owned "$name" || true
      fail "ephemeral session '$name' server pid $_pid failed identity check before readiness"
    fi
    sock="$("$HERDR_BIN" session list --json 2>/dev/null | python3 -c '
import json, sys
name = sys.argv[1]
try:
    sessions = json.load(sys.stdin).get("sessions", [])
except Exception:
    sys.exit(1)
for s in sessions:
    if s.get("name") == name:
        print(s.get("socket_path", ""))
        break
' "$name" 2>/dev/null || true)"
    if [ -n "$sock" ] && [ -S "$sock" ] \
       && HERDR_SOCKET_PATH="$sock" python3 "$HRPC" workspace.list '{}' >/dev/null 2>&1; then
      if e2e_process_identity_verify "$_pid" "$identity"; then
        # Re-verify immediately before publishing the socket to the caller.
        printf -v "$sockvar" '%s' "$sock"
        return 0
      fi
      e2e_managed_root_remove_owned "$name" || true
      fail "ephemeral session '$name' server pid $_pid failed identity check before socket adoption"
    fi
    sleep 0.2
  done
  if e2e_process_identity_verify "$_pid" "$identity"; then
    e2e_session_abort_owned "$name" "$_pid" "$identity" || true
  else
    # Do not stop/delete a name after the owner died: it may now be a replacement.
    e2e_managed_root_remove_owned "$name" || true
  fi
  fail "ephemeral session '$name' did not answer within ~15s (server pid $_pid)"
}

# e2e_session_teardown <name> <pid> <identity> — stop + delete only while the
# exact server process we started still has the recorded /proc identity.
e2e_session_teardown() {
  local name="$1" pid="${2:-}" identity="${3:-}"
  if [ "${E2E_KEEP:-0}" = "1" ]; then
    echo "  keep: leaving ephemeral session '$name' running (pid ${pid:-?})"
    return 0
  fi
  if ! e2e_process_identity_verify "$pid" "$identity"; then
    printf "E2E FAIL: refusing session teardown for '%s': server identity does not match\n" "$name" >&2
    return 1
  fi
  e2e_session_abort_owned "$name" "$pid" "$identity"
}

# e2e_session_ensure — guarantee HERDR_SOCKET_PATH points at an ephemeral herdr
# session for this scenario. If run-all exported E2E_SESSION/E2E_SESSION_SOCKET,
# adopt it (run-all owns its teardown). Otherwise boot our own collision-resistant
# hb-e2e-<pid>-<random>-<random> session and
# register teardown (LIFO: runs AFTER workspaces close + daemon stop). Called by
# e2e_init, BEFORE e2e_daemon_start, so the isolated daemon, herdr CLI, and hrpc
# all treat the ephemeral session as "default".
e2e_session_ensure() {
  if [ -n "${E2E_SESSION:-}" ] && [ -n "${E2E_SESSION_SOCKET:-}" ]; then
    if ! e2e_process_identity_verify "${E2E_SESSION_PID:-}" "${E2E_SESSION_IDENTITY:-}"; then
      fail "refusing to adopt '$E2E_SESSION': run-all server identity does not match"
    fi
    export HERDR_SOCKET_PATH="$E2E_SESSION_SOCKET"
    echo "  ephemeral session (from run-all): $E2E_SESSION"
    echo "  session socket: $E2E_SESSION_SOCKET"
    return 0
  fi
  local name="${E2E_STANDALONE_SESSION:-}"
  [ -n "$name" ] || name="$(e2e_session_name 'hb-e2e-')"
  # If fake-managed panes are enabled, this session is the sole owner of the
  # generated HOME/ZDOTDIR root (never a secondary session).
  export E2E_MANAGED_ROOT_OWNER="$name"
  step "Booting ephemeral herdr session '$name' (standalone, ~2s)"
  e2e_session_boot "$name" E2E_SESSION_SOCKET E2E_SESSION_PID E2E_SESSION_IDENTITY
  export E2E_SESSION="$name"
  export E2E_SESSION_SOCKET E2E_SESSION_PID E2E_SESSION_IDENTITY HERDR_SOCKET_PATH="$E2E_SESSION_SOCKET"
  e2e_defer_session_teardown "$name" "$E2E_SESSION_PID" "$E2E_SESSION_IDENTITY"
  echo "  session socket: $E2E_SESSION_SOCKET (server pid ${E2E_SESSION_PID:-?})"
}

# --- isolated stack ---------------------------------------------------------
# e2e_isolate — create a SHORT /tmp temp dir (AF_UNIX socket paths cap at ~108
# chars, so never nest under a long $TMPDIR) and point BOARD_DB/BOARD_SOCKET/
# HERDR_BOARD_CONFIG at it, with a config that registers the fake harness. Sets
# BOARD_SCOPE_PATH to a deterministic disposable non-Git directory and
# BOARD_SPAWNER=herdr (real herdr panes). Registers temp-dir removal.
e2e_isolate() {
  E2E_TMP="$(mktemp -d /tmp/hb-e2e.XXXXXX)"
  export BOARD_DB="$E2E_TMP/board.db"
  export BOARD_SOCKET="$E2E_TMP/boardd.sock"
  export HERDR_BOARD_CONFIG="$E2E_TMP/config.toml"
  export BOARD_SCOPE_PATH="$E2E_TMP/scope"
  export BOARD_SPAWNER=herdr
  mkdir -p "$BOARD_SCOPE_PATH"
  BOARD_SCOPE_PATH="$(cd "$BOARD_SCOPE_PATH" && pwd -P)"
  export BOARD_SCOPE_PATH
  e2e_write_config "$HERDR_BOARD_CONFIG"
  e2e_defer "rm -rf '$E2E_TMP'"
  echo "  tmp=$E2E_TMP"
  echo "  config:"; sed 's/^/    /' "$HERDR_BOARD_CONFIG"
}

# e2e_write_config <path> — write the isolated daemon config. The fake harness
# is a configured (unmanaged) command launched through the protocol-17 pane-run
# bridge, not `agent.start`. Its `env` argv wrapper pins BOARD_BIN independently
# of pane/workspace environment and carries optional fake-agent knobs from
# E2E_FAKE_ENV (a space-separated list of KEY=VAL, e.g.
# "FAKE_AGENT_HOLD=300") — set it BEFORE e2e_isolate.
e2e_write_config() {
  local extra="" tok
  for tok in ${E2E_FAKE_ENV:-}; do extra+=", \"$tok\""; done
  cat > "$1" <<EOF
[daemon]
spawner = "herdr"
timeout_unit_secs = 1
tick_ms = 200

[harness.fake]
argv = ["env", "BOARD_BIN=$BOARD_BIN"$extra, "bash", "$E2E_FAKE_AGENT"]
EOF
}

# --- daemon -----------------------------------------------------------------
# e2e_daemon_start — start an isolated boardd in the foreground (backgrounded),
# capture its exact /proc identity in E2E_DAEMON_IDENTITY, register a stop, and
# wait until it answers.
e2e_daemon_start() {
  step "Starting isolated boardd (herdr spawner, foreground)"
  "$BOARD_BIN" daemon --foreground >"$E2E_TMP/daemon.log" 2>&1 &
  E2E_DAEMON_PID=$!
  local i
  E2E_DAEMON_IDENTITY=""
  # `daemon` and `--foreground` are required argv elements of this direct child;
  # BOARD_BIN must also have exec'd into argv[0]/argv[1] before we own it.
  for (( i=0; i<25; i++ )); do
    E2E_DAEMON_IDENTITY="$(e2e_process_identity_capture "$E2E_DAEMON_PID" daemon --foreground "$BOARD_BIN")" && break
    sleep 0.02
  done
  if [ -z "$E2E_DAEMON_IDENTITY" ]; then
    # Identity capture failed, but this is still the child this shell spawned.
    # Reap it directly; never leave an unowned daemon behind.
    kill "$E2E_DAEMON_PID" 2>/dev/null || true
    wait "$E2E_DAEMON_PID" 2>/dev/null || true
    fail "refusing isolated daemon: could not capture process identity"
  fi
  export E2E_DAEMON_PID E2E_DAEMON_IDENTITY
  e2e_defer "e2e_daemon_stop"
  local _
  for _ in $(seq 1 30); do
    if "$BOARD_BIN" status >/dev/null 2>&1; then break; fi
    sleep 0.2
  done
  "$BOARD_BIN" status >/dev/null 2>&1 || fail "daemon did not come up (see $E2E_TMP/daemon.log)"
  local scope_params opened
  scope_params="$(python3 -c 'import json,sys; print(json.dumps({"scope_path":sys.argv[1]}))' "$BOARD_SCOPE_PATH")"
  opened="$(brpc board.open "$scope_params")"
  E2E_BOARD_ID="$(printf '%s' "$opened" | python3 -c 'import json,sys; print(json.load(sys.stdin)["board"]["id"])')"
  export E2E_BOARD_ID
  echo "  daemon pid $E2E_DAEMON_PID"
  echo "  board scope $BOARD_SCOPE_PATH (id $E2E_BOARD_ID)"
}

# e2e_daemon_stop — stop ONLY the exact daemon process we started (never
# pattern-kill 'board daemon', which can match our own shell).
e2e_daemon_stop() {
  [ -n "${E2E_DAEMON_PID:-}" ] || return 0
  if ! e2e_process_identity_verify "$E2E_DAEMON_PID" "${E2E_DAEMON_IDENTITY:-}"; then
    printf 'E2E FAIL: refusing daemon stop for pid %s: identity does not match\n' "$E2E_DAEMON_PID" >&2
    return 1
  fi
  echo "  stopping daemon (pid $E2E_DAEMON_PID)"
  # The exact verification above authorizes this contiguous signal/reap. A
  # natural exit race is harmless and wait still reaps our direct child.
  kill "$E2E_DAEMON_PID" 2>/dev/null || true
  wait "$E2E_DAEMON_PID" 2>/dev/null || true
}

# --- disposable workspace ---------------------------------------------------
# e2e_ws_create <label> [session_socket [session_pid [session_identity]]] — create
# a disposable workspace and register its identity-gated close. Omitted identity
# values use the primary E2E_SESSION_PID/E2E_SESSION_IDENTITY; a secondary socket
# must pass that secondary server's pid/token. Sets the new id in E2E_WS (NOT
# stdout — capturing via $(...) would run this in a subshell and lose cleanup).
e2e_ws_create() {
  local label="$1" sock="${2:-}" pid="${3:-${E2E_SESSION_PID:-}}" identity="${4:-${E2E_SESSION_IDENTITY:-}}" ws_json
  local extra_env=()
  if [ "${E2E_FAKE_MANAGED_FUNCTIONS:-0}" = "1" ]; then
    extra_env+=(
      --env "HOME=$E2E_MANAGED_HOME"
      --env "ZDOTDIR=$E2E_MANAGED_ZDOTDIR"
      --env "PATH=$E2E_MANAGED_PATH"
      --env "BASH_ENV=/dev/null"
      --env "ENV=/dev/null"
      --env "BASH_FUNC_pi%%=() { exec \"$E2E_FAKE_PI_BIN_DIR/pi\" \"\$@\"; }"
      --env "BASH_FUNC_claude%%=() { exec \"$E2E_FAKE_PI_BIN_DIR/claude\" \"\$@\"; }"
    )
  fi
  mut "workspace create --label $label --no-focus${sock:+ (session socket $sock)}"
  ws_json="$(env ${sock:+HERDR_SOCKET_PATH="$sock"} "$HERDR_BIN" workspace create \
    --label "$label" --no-focus \
    --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET" \
    --env "BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH" "${extra_env[@]}")"
  E2E_WS="$(printf '%s' "$ws_json" | jget workspace_id)" \
    || fail "could not parse workspace_id from: $ws_json"
  e2e_ws_defer_close "$E2E_WS" "$sock" "$pid" "$identity"
}

# e2e_ws_close_owned <workspace_id> <session_socket> <session_pid>
# <session_identity> — close only while that session server still exactly matches
# its token. This check is immediately before the Herdr mutation.
e2e_ws_close_owned() {
  local ws="$1" sock="$2" pid="$3" identity="$4"
  if [ "${E2E_KEEP:-0}" = "1" ]; then
    echo "  keep: leaving workspace $ws for review"
    return 0
  fi
  mut "workspace close $ws"
  if ! e2e_process_identity_verify "$pid" "$identity"; then
    printf 'E2E FAIL: refusing workspace close %s: session identity does not match\n' "$ws" >&2
    return 1
  fi
  if [ -n "$sock" ]; then
    HERDR_SOCKET_PATH="$sock" "$HERDR_BIN" workspace close "$ws" >/dev/null 2>&1
  else
    "$HERDR_BIN" workspace close "$ws" >/dev/null 2>&1
  fi
}

# e2e_ws_defer_close <workspace_id> [session_socket [session_pid
# [session_identity]]] — register an identity-gated workspace close. Omitted pid
# and token use the primary E2E_SESSION_PID/E2E_SESSION_IDENTITY. Keep mode skips
# the close so the workspace remains available for review.
e2e_ws_defer_close() {
  local ws="$1" sock="${2:-}" pid="${3:-${E2E_SESSION_PID:-}}" identity="${4:-${E2E_SESSION_IDENTITY:-}}" command
  printf -v command 'e2e_ws_close_owned %q %q %q %q' "$ws" "$sock" "$pid" "$identity"
  e2e_defer "$command"
}

# --- card outcome poller ----------------------------------------------------
# wait_ok <card_id> [max_halfsecs] — poll `board card show --json` until the
# card's run reports an outcome (or timeout). Prints the outcome; returns 0 only
# when it is "ok". On failure the caller can dump card state / daemon.log.
wait_ok() {
  local card="$1" tries="${2:-60}" outcome="" show i
  for (( i=0; i<tries; i++ )); do
    show="$("$BOARD_BIN" card show "$card" --json 2>/dev/null || true)"
    outcome="$(printf '%s' "$show" | jget outcome || true)"
    if [ -n "$outcome" ] && [ "$outcome" != "None" ] && [ "$outcome" != "null" ]; then
      break
    fi
    sleep 0.5
  done
  printf '%s' "${outcome:-<none>}"
  [ "$outcome" = "ok" ]
}
