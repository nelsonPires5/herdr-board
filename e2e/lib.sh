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
# scenario gets `hb-e2e-<slug>-<pid>-<random64>` (started via
# `herdr --session <name> server &`). The isolated boardd binds to it
# (HERDR_SOCKET_PATH=<its socket>), so its "default session" IS the ephemeral one,
# and every herdr CLI call + hrpc assert targets it too. run-all scrubs inherited
# session state and every scenario boots (and tears down) its own session. Teardown stops+deletes the session
# unless keep mode is on (--keep / E2E_KEEP=1), which also skips workspace close.
#
# Conventions kept from the original scripts/e2e.sh (now e2e/): `set -euo pipefail` in the
# scenario, `step`/`mut` echo narration, every herdr MUTATION prefixed
# "HERDR MUTATION:". Mutations only ever hit disposable workspaces this suite
# created inside the ephemeral session; never a workspace/session you care about.

# run-all passes this once at shell bootstrap. Scrub it before this sourced file
# executes any subprocess so Herdr/board/helper children never inherit the key.
E2E_LOCAL_IDENTITY_KEY="${E2E_IDENTITY_KEY_BOOTSTRAP:-}"
# Assignment preserves an inherited variable's export attribute in Bash. Strip
# it explicitly so even a hostile standalone environment cannot export the key
# generated below to Herdr/board/helper children.
export -n E2E_LOCAL_IDENTITY_KEY
unset E2E_IDENTITY_KEY_BOOTSTRAP
# These guards prove same-shell creation/reuse. Values arriving through the
# environment are attacker-controlled, even when their marker bytes match.
for _e2e_local_guard in E2E_LOCAL_SCENARIO_ROOT E2E_LOCAL_INVOCATION_TOKEN \
  E2E_LOCAL_MANAGED_ROOT E2E_LOCAL_RESOURCE_MANIFEST; do
  if [[ "$(declare -p "$_e2e_local_guard" 2>/dev/null || true)" == "declare -x "* ]]; then
    unset "$_e2e_local_guard"
  fi
done
unset _e2e_local_guard

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
E2E_PROCESS_IDENTITY="$E2E_LIB_DIR/process_identity.py"
export BOARD_BIN

e2e_identity_key_ensure() {
  if [ -z "$E2E_LOCAL_IDENTITY_KEY" ]; then
    E2E_LOCAL_IDENTITY_KEY="$(python3 -c 'import secrets; print(secrets.token_hex(32))')" \
      || fail "cannot generate process-identity signing key"
  fi
  [ "${#E2E_LOCAL_IDENTITY_KEY}" -ge 32 ] || fail "process-identity signing key is too short"
}

e2e_identity_python() {
  [ -n "$E2E_LOCAL_IDENTITY_KEY" ] || return 1
  python3 "$E2E_PROCESS_IDENTITY" "$@" 3<<<"$E2E_LOCAL_IDENTITY_KEY"
}

e2e_stat_mode() { python3 "$E2E_PROCESS_IDENTITY" mode "$1"; }
e2e_realpath() { python3 "$E2E_PROCESS_IDENTITY" realpath "$1"; }
e2e_process_exists() { python3 "$E2E_PROCESS_IDENTITY" exists "$1"; }
e2e_process_state() { python3 "$E2E_PROCESS_IDENTITY" state "$1"; }
e2e_identity_sign_json() { e2e_identity_python sign "$1"; }
e2e_identity_token_validate() { e2e_identity_python validate "$1"; }

e2e_path_is_canonical() {
  python3 - "$1" <<'PY'
import os,sys
path=os.path.abspath(sys.argv[1])
expected=("/private"+path) if sys.platform == "darwin" and path.startswith("/tmp/") else path
raise SystemExit(0 if os.path.realpath(path) == expected else 1)
PY
}

# Scope the checked-in executables named exactly `pi` and `claude` to disposable
# e2e Herdr servers. The candidate board binary is also on PATH so the fakes can
# call `board comment` / `board done` without a custom built-in harness env.
# Managed panes get a separate HOME/ZDOTDIR below. The scenario's own HOME is
# also private and deliberately very short: Herdr nests named-session sockets
# below HOME, so even a bounded session name needs this margin under sun_path.
e2e_marker_shape_verify() {
  local marker="$1" header="$2" owner="$3" token="$4"
  [ -f "$marker" ] && [ ! -L "$marker" ] && [ "$(e2e_stat_mode "$marker")" = 600 ] || return 1
  python3 - "$marker" "$header" "$owner" "$token" <<'PY'
import pathlib,sys
path,header,owner,token=sys.argv[1:]
expected=f"{header}\nowner={owner}\ntoken={token}\n"
raise SystemExit(0 if pathlib.Path(path).read_text(encoding="utf-8") == expected else 1)
PY
}

e2e_private_dir_verify() {
  local path="$1" pattern="$2"
  [[ "$path" == $pattern ]] && [ -d "$path" ] && [ ! -L "$path" ] \
    && [ "$(e2e_stat_mode "$path")" = 700 ] && e2e_path_is_canonical "$path"
}

e2e_artifact_invocation_validate() {
  [ -n "${E2E_SCENARIO_ARTIFACT_DIR:-}" ] || return 0
  [ -n "${E2E_INVOCATION_ARTIFACT_ROOT:-}" ] && [ -n "${E2E_INVOCATION_TOKEN:-}" ] \
    && [ -n "${E2E_INVOCATION_OWNER_ID:-}" ] \
    && e2e_private_dir_verify "$E2E_INVOCATION_ARTIFACT_ROOT" '/tmp/hb-e2e-run.??????' \
    && e2e_marker_shape_verify "$E2E_INVOCATION_ARTIFACT_ROOT/.owned-artifacts" \
      herdr-board-e2e-artifacts "$E2E_INVOCATION_OWNER_ID" "$E2E_INVOCATION_TOKEN" \
    && e2e_private_dir_verify "$E2E_SCENARIO_ARTIFACT_DIR" "$E2E_INVOCATION_ARTIFACT_ROOT/*" \
    && [ "$(dirname "$E2E_SCENARIO_ARTIFACT_DIR")" = "$E2E_INVOCATION_ARTIFACT_ROOT" ] \
    && e2e_marker_shape_verify "$E2E_SCENARIO_ARTIFACT_DIR/.owned-artifact" \
      herdr-board-e2e-scenario-artifact "${E2E_OWNER_ID:-}" "$E2E_INVOCATION_TOKEN" \
    || fail "refusing unowned or unbounded E2E artifact paths"
  E2E_ARTIFACT_INVOCATION_VALIDATED=1
}

e2e_scenario_root_ensure() {
  e2e_identity_key_ensure
  local owner="${E2E_OWNER_ID:-standalone-$$}"
  # Reuse is process-local only. An environment can never nominate a root,
  # even with a self-authored matching token/marker.
  if [ -n "${E2E_SCENARIO_ROOT:-}" ]; then
    [ "${E2E_LOCAL_SCENARIO_ROOT:-}" = "$E2E_SCENARIO_ROOT" ] \
      && [ "${E2E_LOCAL_INVOCATION_TOKEN:-}" = "${E2E_INVOCATION_TOKEN:-}" ] \
      && e2e_private_dir_verify "$E2E_SCENARIO_ROOT" '/tmp/h????????' \
      && e2e_marker_shape_verify "$E2E_SCENARIO_ROOT/.disposable" herdr-board-e2e "$owner" "$E2E_INVOCATION_TOKEN" \
      || fail "refusing inherited or malformed E2E_SCENARIO_ROOT"
    export HOME="$E2E_SCENARIO_ROOT"
    return 0
  fi
  umask 077
  e2e_artifact_invocation_validate
  if [ -n "${E2E_INVOCATION_TOKEN:-}" ]; then
    [ "${E2E_ARTIFACT_INVOCATION_VALIDATED:-0}" = 1 ] \
      || fail "refusing inherited invocation token outside an owned artifact invocation"
  else
    E2E_INVOCATION_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
  fi
  local nonce attempt
  for (( attempt=0; attempt<20; attempt++ )); do
    nonce="$(python3 -c 'import secrets; print(secrets.token_hex(4))')" \
      || fail "cannot generate scenario nonce"
    E2E_SCENARIO_ROOT="/tmp/h$nonce"
    if mkdir -m 700 "$E2E_SCENARIO_ROOT" 2>/dev/null; then break; fi
    E2E_SCENARIO_ROOT=""
  done
  [ -n "$E2E_SCENARIO_ROOT" ] || fail "cannot allocate short private scenario root"
  printf 'herdr-board-e2e\nowner=%s\ntoken=%s\n' "$owner" "$E2E_INVOCATION_TOKEN" >"$E2E_SCENARIO_ROOT/.disposable"
  chmod 600 "$E2E_SCENARIO_ROOT/.disposable"
  E2E_LOCAL_SCENARIO_ROOT="$E2E_SCENARIO_ROOT"
  E2E_LOCAL_INVOCATION_TOKEN="$E2E_INVOCATION_TOKEN"
  export E2E_INVOCATION_TOKEN E2E_SCENARIO_ROOT HOME="$E2E_SCENARIO_ROOT"
  e2e_resource_manifest_init || fail "cannot initialize exact-resource manifest"
  e2e_root_resource_register scenario scenario-root "$E2E_SCENARIO_ROOT" "$E2E_SCENARIO_ROOT/.disposable" \
    || fail "cannot record early scenario root ownership"
  if [ "${E2E_SCENARIO_ROOT_DEFERRED:-0}" != 1 ]; then
    e2e_defer "e2e_scenario_root_remove_owned"
    E2E_SCENARIO_ROOT_DEFERRED=1
  fi
}

e2e_enable_fake_pi() {
  e2e_scenario_root_ensure
  if [ -n "${E2E_MANAGED_ROOT:-}" ]; then
    [ "${E2E_LOCAL_MANAGED_ROOT:-}" = "$E2E_MANAGED_ROOT" ] \
      && e2e_private_dir_verify "$E2E_MANAGED_ROOT" '/tmp/hb-e2e-managed.??????' \
      && e2e_marker_shape_verify "$E2E_MANAGED_ROOT/.herdr-board-fake-managed" \
        'herdr-board fake-managed boundary' "${E2E_OWNER_ID:-standalone-$$}" "$E2E_INVOCATION_TOKEN" \
      && e2e_private_dir_verify "$E2E_MANAGED_ROOT/home" "$E2E_MANAGED_ROOT/home" \
      && e2e_private_dir_verify "$E2E_MANAGED_ROOT/zdot" "$E2E_MANAGED_ROOT/zdot" \
      || fail "refusing inherited or malformed E2E_MANAGED_ROOT"
  fi
  [ -x "$E2E_FAKE_PI_BIN_DIR/pi" ] || fail "fake pi missing/not executable"
  [ -x "$E2E_FAKE_PI_BIN_DIR/claude" ] || fail "fake claude missing/not executable"
  export E2E_FAKE_PI_BIN_DIR
  export PATH="$E2E_FAKE_PI_BIN_DIR:$REPO_ROOT/target/release:$PATH"
  export E2E_FAKE_MANAGED_FUNCTIONS=1 E2E_FAKE_MANAGED_ZDOT=1
  # Do not trust or reuse a user's HOME, ZDOTDIR, PATH, or shell startup files
  # in a fake-managed pane. This root belongs only to the scenario's primary
  # disposable session.
  if [ -z "${E2E_MANAGED_ROOT:-}" ]; then
    E2E_MANAGED_ROOT="$(mktemp -d /tmp/hb-e2e-managed.XXXXXX)"
    printf 'herdr-board fake-managed boundary\nowner=%s\ntoken=%s\n' \
      "${E2E_OWNER_ID:-standalone-$$}" "$E2E_INVOCATION_TOKEN" >"$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
    chmod 600 "$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
    mkdir -m 700 "$E2E_MANAGED_ROOT/home" "$E2E_MANAGED_ROOT/zdot"
    # This shell alone can reuse and remove the exact marker-owned root.
    E2E_MANAGED_ROOT_CREATOR_PID=$$
    E2E_LOCAL_MANAGED_ROOT="$E2E_MANAGED_ROOT"
    export E2E_MANAGED_ROOT E2E_MANAGED_ROOT_CREATOR_PID
    e2e_resource_manifest_init || fail "cannot initialize exact-resource manifest"
    e2e_root_resource_register managed managed-root "$E2E_MANAGED_ROOT" \
      "$E2E_MANAGED_ROOT/.herdr-board-fake-managed" || fail "cannot record managed root ownership"
    e2e_defer "e2e_managed_root_remove_early_owned"
  fi
  if [ "${E2E_TEST_INJECT_FAKE_SETUP_FAILURE:-0}" = 1 ]; then
    [ -z "${E2E_TEST_EARLY_PATH_LOG:-}" ] \
      || printf '%s\n%s\n%s\n' "$E2E_SCENARIO_ROOT" "$E2E_MANAGED_ROOT" "$E2E_OWNED_RESOURCE_MANIFEST" >"$E2E_TEST_EARLY_PATH_LOG"
    fail "injected fake-managed pre-init failure"
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

# hrpc <method> [json-params] — one-shot read-only herdr RPC (honors
# HERDR_SOCKET_PATH). It never authorizes a mutation.
hrpc() { python3 "$HRPC" "$@"; }

# Execute from an allowlisted environment. In particular, no provider key,
# provider base URL, opt-in, shell function, or inherited Herdr session can
# cross this boundary. Keep only variables required to locate normal runtime
# tools and render useful diagnostics.
e2e_clean_env() {
  env -i PATH="${E2E_STANDARD_PATH:-/usr/local/bin:/usr/bin:/bin}" \
    LANG="${LANG:-C.UTF-8}" LC_ALL="${LC_ALL:-}" TERM="${TERM:-dumb}" \
    TZ="${TZ:-UTC}" "$@"
}

# A Herdr mutation token must itself describe an exact owned server, never a
# generic process token (such as boardd or an unrelated helper).
e2e_session_identity_verify() {
  local pid="$1" token="$2" expected_name="${3:-}"
  e2e_process_identity_verify "$pid" "$token" || return 1
  python3 - "$token" "$expected_name" <<'PY'
import json,sys
try: t=json.loads(sys.argv[1])
except Exception: raise SystemExit(1)
name=t.get("name")
if (not name or t.get("session") != name or not t.get("owner_token")
    or (sys.argv[2] and name != sys.argv[2])
    or t.get("cmdline") != [t.get("expected_command"), "--session", name, "server"]):
    raise SystemExit(1)
PY
}

e2e_session_target_verify() {
  local pid="$1" token="$2" socket="$3" name
  e2e_session_identity_verify "$pid" "$token" || return 1
  name="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["name"])' "$token")" \
    || return 1
  [ -n "$socket" ] && [ "${E2E_SESSION_SOCKETS[$name]:-}" = "$socket" ]
}

# Identity-gated Herdr mutation. Optional ownership is supplied before `--`;
# otherwise the primary session token/socket are used.
e2e_herdr_mutate() {
  local pid="${E2E_SESSION_PID:-}" identity="${E2E_SESSION_IDENTITY:-}" \
    sock="${HERDR_SOCKET_PATH:-${E2E_SESSION_SOCKET:-}}"
  if [ "${1:-}" != "--" ]; then
    pid="$1"; identity="$2"; sock="$3"; shift 3
  fi
  [ "${1:-}" = "--" ] || fail "e2e_herdr_mutate requires -- before argv"
  shift
  e2e_session_target_verify "$pid" "$identity" "$sock" \
    || fail "refusing Herdr mutation '$*': session identity/socket does not match"
  mut "$*" >&2
  HERDR_SOCKET_PATH="$sock" "$HERDR_BIN" "$@"
}

# Gate board commands that may ask boardd to mutate Herdr. Both independently
# captured processes are checked immediately before the board request.
e2e_hrpc_mutate() {
  local pid="${E2E_SESSION_PID:-}" identity="${E2E_SESSION_IDENTITY:-}" \
    sock="${HERDR_SOCKET_PATH:-${E2E_SESSION_SOCKET:-}}"
  if [ "${1:-}" != "--" ]; then pid="$1"; identity="$2"; sock="$3"; shift 3; fi
  [ "${1:-}" = "--" ] || fail "e2e_hrpc_mutate requires -- before method"
  shift
  e2e_session_target_verify "$pid" "$identity" "$sock" \
    || fail "refusing Herdr RPC mutation '$*': session identity/socket does not match"
  mut "rpc $*" >&2
  HERDR_SOCKET_PATH="$sock" python3 "$HRPC" "$@"
}

e2e_board_herdr_mutate() {
  local pid="${E2E_SESSION_PID:-}" identity="${E2E_SESSION_IDENTITY:-}"
  if [ "${1:-}" != "--" ]; then pid="$1"; identity="$2"; shift 2; fi
  [ "${1:-}" = "--" ] || fail "e2e_board_herdr_mutate requires -- before argv"
  shift
  e2e_process_identity_verify "${E2E_DAEMON_PID:-}" "${E2E_DAEMON_IDENTITY:-}" \
    || fail "refusing board mutation '$*': daemon identity does not match"
  e2e_session_identity_verify "$pid" "$identity" \
    || fail "refusing board mutation '$*': target session identity does not match"
  mut "board $* (identity gated)" >&2
  "$BOARD_BIN" "$@"
}

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
e2e_card_failure_diag() {
  local card="$1"
  "$BOARD_BIN" card show "$card" --json 2>/dev/null | python3 -c '
import hashlib,json,sys
try: d=json.load(sys.stdin)
except Exception: print("card diagnostic unavailable",file=sys.stderr); raise SystemExit(0)
c=d.get("card",{}); runs=d.get("runs",[]); r=runs[-1] if runs else {}
p=(r.get("prompt_snapshot") or "").encode(); s=(r.get("system_prompt_snapshot") or "").encode()
print("card diagnostic: id=%s status=%s column_id=%s run_id=%s outcome=%s prompt_len=%d prompt_sha256=%s system_len=%d system_sha256=%s" %
 (c.get("id"),c.get("status"),c.get("column_id"),r.get("id"),r.get("outcome"),len(p),hashlib.sha256(p).hexdigest(),len(s),hashlib.sha256(s).hexdigest()),file=sys.stderr)
' || true
}

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

e2e_suite_verdict() {
  local code="$1" audit="$2" require="$3"
  [ "$audit" -eq 0 ] || { printf FAIL; return; }
  case "$code" in
    0) printf PASS ;;
    3) [ "$require" -eq 1 ] && printf FAIL || printf SKIP ;;
    *) printf FAIL ;;
  esac
}

# Audit only exact resources emitted by this invocation. No session inventory,
# prefix search, process-name search, or synthesized path is consulted. Released
# and replaced generations are still checked: a cleanup callback claiming
# success cannot hide an exact old process/path that remains.
e2e_audit_owned_manifest() {
  local manifest="$1" keep="${2:-0}"
  [ -f "$manifest" ] || { printf 'E2E FAIL: owned-resource manifest missing: %s\n' "$manifest" >&2; return 1; }
  e2e_identity_key_ensure
  # Darwin may lazily recreate $HOME/Library when Python starts. The scenario
  # root is deliberately removed before this final audit, so never let the
  # audit process recreate that just-released root.
  HOME=/var/empty CFFIXED_USER_HOME=/var/empty python3 - "$manifest" "$keep" "$E2E_PROCESS_IDENTITY" \
    3<<<"$E2E_LOCAL_IDENTITY_KEY" <<'PY'
import hashlib, importlib.util, json, os, sys

spec=importlib.util.spec_from_file_location("e2e_process_identity", sys.argv[3])
identity=importlib.util.module_from_spec(spec); sys.modules[spec.name]=identity
spec.loader.exec_module(identity)
with os.fdopen(3, "rb", closefd=True) as key_file:
    identity_key=key_file.read().rstrip(b"\n")
IDENTITY_KEYS = {"version","platform","proof","parent_pid","pid","start_time","exe",
                 "session","name","expected_command","owner_token","cmdline","signature"}
BASE_KEYS = {"version","op","resource_id","logical_id","generation","kind","role"}
ROLE_BY_KIND = {
    "process": {"board-daemon","helper","proxy"},
    "root": {"scenario","managed"},
    "script": {"configured-runner","temp-script"},
    "workspace": {"workspace"},
    "session": {"session"},
}
EXTRA_KEYS = {
    "process": {"pid","identity"},
    "root": {"path","marker","marker_sha256"},
    "script": {"path","content_sha256"},
    "workspace": {"marker","marker_sha256"},
    "session": {"name","pid","identity","registry","owner_marker","marker_sha256"},
}

def exact_identity_present(pid, token):
    if not isinstance(pid, str) or not pid.isdigit() or not isinstance(token, dict):
        raise ValueError("invalid process identity")
    if set(token) != IDENTITY_KEYS or token.get("pid") != pid:
        raise ValueError("invalid process identity fields")
    try:
        identity.validate_token(token, identity_key)
    except identity.IdentityError as exc:
        raise ValueError("invalid signed process identity") from exc
    return identity.verify_identity(int(pid), token, identity_key, audit=True)

def marker_digest(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()

def marker_matches(record, path):
    try: return marker_digest(path) == record["marker_sha256"]
    except OSError: return False

def check_abs(value):
    return isinstance(value, str) and value.startswith("/") and "\0" not in value

failed = False
keep = sys.argv[2] == "1"
active = {}
registrations = []
try:
    lines = open(sys.argv[1], encoding="utf-8")
except OSError as e:
    print(f"E2E FAIL: cannot read owned-resource manifest: {e}", file=sys.stderr)
    raise SystemExit(1)
for number, line in enumerate(lines, 1):
    try:
        r = json.loads(line)
        if not isinstance(r, dict) or r.get("version") != 1:
            raise ValueError("unsupported record")
        op = r.get("op")
        if op == "release":
            if set(r) != {"version","op","resource_id"} or r["resource_id"] not in active:
                raise ValueError("invalid release")
            del active[r["resource_id"]]
            continue
        if op not in ("register", "replace"):
            raise ValueError("invalid operation")
        kind = r.get("kind")
        expected = BASE_KEYS | EXTRA_KEYS.get(kind, set())
        if op == "replace": expected |= {"replaces"}
        if set(r) != expected or kind not in ROLE_BY_KIND or r.get("role") not in ROLE_BY_KIND[kind]:
            raise ValueError("invalid resource shape")
        if (not isinstance(r["resource_id"], str) or not isinstance(r["logical_id"], str)
                or not r["logical_id"] or not isinstance(r["generation"], int)
                or r["generation"] < 1 or r["resource_id"] in active):
            raise ValueError("invalid resource identity")
        if op == "replace":
            old = r["replaces"]
            if old not in active or active[old]["logical_id"] != r["logical_id"]:
                raise ValueError("invalid replacement")
            del active[old]
        elif any(v["logical_id"] == r["logical_id"] and v["kind"] == kind for v in active.values()):
            raise ValueError("duplicate active logical resource")
        if kind in ("process", "session"):
            exact_identity_present(r["pid"], r["identity"])
        if kind == "session":
            t = r["identity"]
            if (r["name"] != t["name"] or t["session"] != r["name"] or not t["owner_token"]
                    or t["cmdline"] != [t["expected_command"], "--session", r["name"], "server"]):
                raise ValueError("invalid session identity")
            if not check_abs(r["registry"]) or not check_abs(r["owner_marker"]):
                raise ValueError("invalid session paths")
        elif kind == "root":
            if not check_abs(r["path"]) or not check_abs(r["marker"]):
                raise ValueError("invalid root paths")
        elif kind == "workspace":
            if not check_abs(r["marker"]): raise ValueError("invalid workspace marker")
        elif kind == "script":
            if not check_abs(r["path"]): raise ValueError("invalid script path")
            if (not isinstance(r["content_sha256"], str) or len(r["content_sha256"]) != 64
                    or any(c not in "0123456789abcdef" for c in r["content_sha256"])):
                raise ValueError("invalid script digest")
        for key in ("marker_sha256",):
            if key in r and (not isinstance(r[key], str) or len(r[key]) != 64
                             or any(c not in "0123456789abcdef" for c in r[key])):
                raise ValueError("invalid marker evidence")
        active[r["resource_id"]] = r
        registrations.append(r)
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as e:
        print(f"E2E FAIL: malformed owned-resource record line {number}: {e}", file=sys.stderr)
        failed = True

for r in registrations:
    if keep: continue
    leaked = []
    marker = r.get("marker") or r.get("owner_marker")
    if marker and os.path.lexists(marker) and not marker_matches(r, marker):
        print(f"E2E FAIL: ownership marker digest changed for {r['logical_id']}", file=sys.stderr)
        failed = True
    if r["kind"] in ("process", "session"):
        try:
            if exact_identity_present(r["pid"], r["identity"]): leaked.append(f"process pid={r['pid']}")
        except ValueError as e:
            print(f"E2E FAIL: malformed identity for {r['logical_id']}: {e}", file=sys.stderr)
            failed = True
    if r["kind"] == "session":
        for label, path in (("registry", r["registry"]), ("owner marker", r["owner_marker"])):
            if os.path.lexists(path): leaked.append(f"{label} {path}")
    elif r["kind"] == "root":
        if os.path.lexists(r["path"]): leaked.append(f"root {r['path']}")
        elif os.path.lexists(r["marker"]): leaked.append(f"marker {r['marker']}")
    elif r["kind"] == "workspace" and os.path.lexists(r["marker"]):
        leaked.append(f"workspace marker {r['marker']}")
    elif r["kind"] == "script" and os.path.lexists(r["path"]):
        try:
            digest = marker_digest(r["path"])
        except OSError:
            digest = ""
        if digest != r["content_sha256"]:
            print(f"E2E FAIL: script content digest changed for {r['logical_id']}", file=sys.stderr)
            failed = True
        leaked.append(f"script {r['path']}")
    for item in leaked:
        print(f"E2E FAIL: exact recorded {r['role']} remains: {item}", file=sys.stderr)
        failed = True
raise SystemExit(1 if failed else 0)
PY
}

# --- cleanup registry -------------------------------------------------------
# Register teardown commands as you create things; e2e_cleanup runs them in
# REVERSE (LIFO) on EXIT so workspaces close before the daemon stops. Call
# e2e_init once, early, to install the trap.
E2E_CLEANUP=()
e2e_defer() { E2E_CLEANUP+=("$*"); }
e2e_cleanup() {
  local rc=$? cleanup_rc=0 audit_rc=0 i
  step "Cleanup"
  for (( i=${#E2E_CLEANUP[@]}-1; i>=0; i-- )); do
    # Keep running every LIFO cleanup after a failure. A scenario failure remains
    # authoritative, but a successful scenario must expose cleanup failures.
    if ! eval "${E2E_CLEANUP[$i]}"; then
      cleanup_rc=1
    fi
  done
  if [ -n "${E2E_OWNED_RESOURCE_MANIFEST:-}" ]; then
    e2e_audit_owned_manifest "$E2E_OWNED_RESOURCE_MANIFEST" "${E2E_KEEP:-0}" || audit_rc=$?
    [ "$audit_rc" -eq 0 ] || cleanup_rc=1
    if [ "${E2E_RESOURCE_MANIFEST_STANDALONE:-0}" = 1 ]; then
      rm -f -- "$E2E_OWNED_RESOURCE_MANIFEST"
    fi
  fi
  echo "  done"
  [ "$rc" -ne 0 ] && return "$rc"
  return "$cleanup_rc"
}
e2e_init_common() {
  trap e2e_cleanup EXIT
  e2e_scenario_root_ensure
  e2e_resource_manifest_init || fail "cannot initialize exact-resource manifest"
  if [ -z "${E2E_RESOURCE_CURRENT_IDS[root:scenario-root]:-}" ]; then
    e2e_root_resource_register scenario scenario-root "$E2E_SCENARIO_ROOT" "$E2E_SCENARIO_ROOT/.disposable" \
      || fail "cannot record scenario root ownership"
  fi
  if [ -n "${E2E_MANAGED_ROOT:-}" ] && [ -z "${E2E_RESOURCE_CURRENT_IDS[root:managed-root]:-}" ]; then
    e2e_root_resource_register managed managed-root "$E2E_MANAGED_ROOT" \
      "$E2E_MANAGED_ROOT/.herdr-board-fake-managed" || fail "cannot record managed root ownership"
  fi
  if [ "${E2E_SCENARIO_ROOT_DEFERRED:-0}" != 1 ]; then
    e2e_defer "e2e_scenario_root_remove_owned"
    E2E_SCENARIO_ROOT_DEFERRED=1
  fi
  E2E_STANDALONE_SESSION="$(e2e_session_name)"
  export E2E_STANDALONE_SESSION E2E_MANAGED_ROOT_OWNER="$E2E_STANDALONE_SESSION"
  e2e_defer "e2e_managed_root_remove_owned '${E2E_MANAGED_ROOT_OWNER}'"
  e2e_require
}

e2e_start_reserved_session() {
  e2e_session_ensure
  e2e_protocol_preflight
}

e2e_init() {
  e2e_init_common
  e2e_start_reserved_session
}

# Scenario 19 deliberately starts boardd before its reserved Herdr session.
# This installs the identical ownership/cleanup boundary but performs no Herdr
# mutation until e2e_start_reserved_session is called explicitly.
e2e_init_late_session() {
  e2e_init_common
}

# --- ephemeral herdr session ------------------------------------------------
# A PID alone is not process ownership. Tokens bind the exact process instance,
# executable and complete argv to a private per-invocation HMAC. Linux also
# re-verifies the owner token in /proc/environ; Darwin establishes ownership
# through the signed exact direct-child capability.
e2e_process_identity_capture() {
  local pid="$1" session="$2" name="$3" expected_command="${4:-}" owner_token="${5:-}" \
    owner_env="${6:-E2E_HERDR_OWNER_TOKEN}" provisional="${7:-}"
  [ -n "$provisional" ] || return 1
  e2e_identity_python stable-capture "$pid" "$session" "$name" "$expected_command" \
    "$owner_token" "$owner_env" "$provisional"
}

e2e_process_identity_verify() {
  local pid="$1" token="$2"
  [ -n "$token" ] && e2e_identity_python verify "$pid" "$token"
}

e2e_provisional_child_capture() {
  local pid="$1" owner_token="$2" owner_env="${3:-E2E_HERDR_OWNER_TOKEN}"
  e2e_identity_python provisional-capture "$pid" "$owner_token" "$$" "$owner_env"
}

# Verify the stable spawn capability across the one permitted identity change:
# the exact launcher may exec either the requested Herdr server or board-daemon
# argv. PID/start/parent and the owner environment remain unchanged throughout.
e2e_provisional_child_transition_verify() {
  local pid="$1" token="$2" expected_command="${3:-}" name="${4:-}" \
    transition="${5:-session}" owner_env="${6:-E2E_HERDR_OWNER_TOKEN}"
  e2e_identity_python transition-verify "$pid" "$token" "$$" "$expected_command" \
    "$name" "$transition" "$owner_env"
}

declare -Ag E2E_PROVISIONAL_CHILD_ARMED=()
e2e_provisional_child_abort() {
  local logical="$1" pid="$2" token="$3" expected_command="${4:-}" name="${5:-}" \
    transition="${6:-session}" owner_env="${7:-E2E_HERDR_OWNER_TOKEN}"
  # A known, explicitly disarmed capability is a no-op. Calls outside session
  # boot remain usable by focused deterministic tests.
  if [ "${E2E_PROVISIONAL_CHILD_ARMED[$logical]+set}" = set ] \
     && [ "${E2E_PROVISIONAL_CHILD_ARMED[$logical]}" != 1 ]; then return 0; fi
  if ! e2e_process_exists "$pid"; then
    wait "$pid" 2>/dev/null || true
    e2e_process_resource_release "$logical"
    E2E_PROVISIONAL_CHILD_ARMED["$logical"]=0
    return 0
  fi
  e2e_provisional_child_transition_verify "$pid" "$token" "$expected_command" "$name" "$transition" "$owner_env" || {
    printf 'E2E FAIL: refusing provisional child signal: spawn capability mismatch\n' >&2
    return 1
  }
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  e2e_process_resource_release "$logical"
  E2E_PROVISIONAL_CHILD_ARMED["$logical"]=0
}

# Session cleanup is registered inside e2e_session_boot immediately after the
# first owned token is captured. Callers may repeat registration; this updates
# the token but never adds a duplicate stop/delete action.
declare -Ag E2E_SESSION_IDENTITIES=() E2E_SESSION_PIDS=() E2E_SESSION_SOCKETS=() E2E_SESSION_TEARDOWN_REGISTERED=()
e2e_defer_session_teardown() {
  local name="$1" pid="$2" identity="$3" command
  E2E_SESSION_IDENTITIES["$name"]="$identity"
  E2E_SESSION_PIDS["$name"]="$pid"
  [ "${E2E_SESSION_TEARDOWN_REGISTERED[$name]:-0}" = 1 ] && return 0
  E2E_SESSION_TEARDOWN_REGISTERED["$name"]=1
  printf -v command 'e2e_session_teardown_registered %q' "$name"
  e2e_defer "$command"
}
e2e_session_teardown_registered() {
  local name="$1" pid="${E2E_SESSION_PIDS[$1]:-}" identity="${E2E_SESSION_IDENTITIES[$1]:-}"
  e2e_session_teardown "$name" "$pid" "$identity"
}

# Collision-resistant, bounded name: hb-e2e-<slug:8>-<pid>-<random64>.
e2e_session_name() {
  local slug nonce hint="${1:-}"
  slug="$(printf '%s' "${E2E_TEST_SLUG:-${E2E_TEST_FILE:-standalone}}" | tr '[:upper:]' '[:lower:]' | sed -E 's/\.sh$//; s/[^a-z0-9]+/-/g; s/^-+//; s/-+$//; s/-+/-/g' | cut -c1-8)"
  [ -n "$slug" ] || slug=scenario
  [[ "$hint" == *-b-* ]] && slug="${slug:0:6}-b"
  nonce="$(python3 -c 'import secrets; print(secrets.token_hex(8))')" || fail "cannot generate session nonce"
  printf 'hb-e2e-%s-%s-%s' "$slug" "$$" "$nonce"
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
e2e_scenario_root_remove_owned() {
  local root="${E2E_SCENARIO_ROOT:-}"
  if [ "${E2E_KEEP:-0}" = 1 ]; then
    echo "  keep: leaving marked scenario root $root"
    return 0
  fi
  [ -e "$root" ] || return 0
  case "$root" in
    /tmp/h????????)
      [ -d "$root" ] && e2e_marker_resource_verify root scenario-root "$root/.disposable" || {
        printf 'E2E FAIL: refusing changed/unowned scenario-root cleanup: %s\n' "$root" >&2; return 1;
      }
      # Release serialization starts Python, which may lazily create macOS
      # $HOME/Library caches. Ledger first, then make removal the final action.
      e2e_root_resource_release scenario-root || return 1
      rm -rf -- "$root" ;;
    *) printf 'E2E FAIL: refusing scenario-root cleanup outside /tmp: %s\n' "$root" >&2; return 1 ;;
  esac
}

# Append-only exact resource ledger shared by standalone scenarios and run-all.
# Payloads are structural ownership evidence only; prompt/system-prompt paths and
# content must never be passed to these helpers.
declare -Ag E2E_RESOURCE_GENERATIONS=() E2E_RESOURCE_CURRENT_IDS=()
e2e_resource_manifest_init() {
  if [ -n "${E2E_OWNED_RESOURCE_MANIFEST:-}" ]; then
    [ "${E2E_LOCAL_RESOURCE_MANIFEST:-}" = "$E2E_OWNED_RESOURCE_MANIFEST" ] \
      || { printf 'E2E FAIL: refusing inherited resource manifest\n' >&2; return 1; }
    return 0
  fi
  if [ -n "${E2E_SCENARIO_ARTIFACT_DIR:-}" ]; then
    e2e_artifact_invocation_validate
    E2E_OWNED_RESOURCE_MANIFEST="$E2E_SCENARIO_ARTIFACT_DIR/owned-resources.ndjson"
    [ ! -e "$E2E_OWNED_RESOURCE_MANIFEST" ] \
      || { printf 'E2E FAIL: refusing pre-existing resource manifest\n' >&2; return 1; }
    E2E_RESOURCE_MANIFEST_STANDALONE=0
  else
    [ -z "${E2E_INVOCATION_ARTIFACT_ROOT:-}" ] \
      || { printf 'E2E FAIL: artifact root requires an owned scenario artifact\n' >&2; return 1; }
    E2E_OWNED_RESOURCE_MANIFEST="$(mktemp /tmp/hb-e2e-owned.XXXXXX)" || return 1
    E2E_RESOURCE_MANIFEST_STANDALONE=1
  fi
  : >"$E2E_OWNED_RESOURCE_MANIFEST"
  chmod 600 "$E2E_OWNED_RESOURCE_MANIFEST"
  E2E_LOCAL_RESOURCE_MANIFEST="$E2E_OWNED_RESOURCE_MANIFEST"
  export E2E_OWNED_RESOURCE_MANIFEST E2E_RESOURCE_MANIFEST_STANDALONE
}

e2e_marker_sha256() { python3 - "$1" <<'PY'
import hashlib,sys
with open(sys.argv[1],"rb") as f: print(hashlib.sha256(f.read()).hexdigest())
PY
}

# Verify the marker against the active ledger generation immediately before a
# destructive operation. Marker existence alone never proves ownership.
e2e_marker_resource_verify() {
  local kind="$1" logical="$2" marker="$3" rid="${E2E_RESOURCE_CURRENT_IDS[$1:$2]:-}"
  [ -n "$rid" ] && [ -f "$marker" ] || return 1
  python3 - "$E2E_OWNED_RESOURCE_MANIFEST" "$rid" "$marker" <<'PY'
import hashlib,json,sys
manifest,rid,marker=sys.argv[1:]
record=None
for line in open(manifest,encoding="utf-8"):
    value=json.loads(line)
    if value.get("resource_id")==rid: record=value
if not record or record.get("marker") != marker and record.get("owner_marker") != marker:
    raise SystemExit(1)
with open(marker,"rb") as f: digest=hashlib.sha256(f.read()).hexdigest()
raise SystemExit(0 if digest == record.get("marker_sha256") else 1)
PY
}

e2e_resource_register_json() {
  local kind="$1" role="$2" logical="$3" payload="$4" key
  key="$kind:$logical"
  local generation=$(( ${E2E_RESOURCE_GENERATIONS[$key]:-0} + 1 )) old="${E2E_RESOURCE_CURRENT_IDS[$key]:-}"
  local resource_id="$key:g$generation" line
  e2e_resource_manifest_init || return 1
  line="$(python3 - "$kind" "$role" "$logical" "$generation" "$resource_id" "$old" "$payload" <<'PY'
import json,sys
kind,role,logical,generation,rid,old,payload=sys.argv[1:]
r={"version":1,"op":"replace" if old else "register","resource_id":rid,
   "logical_id":logical,"generation":int(generation),"kind":kind,"role":role}
if old: r["replaces"]=old
p=json.loads(payload)
if not isinstance(p,dict) or set(r)&set(p): raise SystemExit(1)
r.update(p)
print(json.dumps(r,separators=(",",":"),sort_keys=True))
PY
)" || return 1
  printf '%s\n' "$line" >>"$E2E_OWNED_RESOURCE_MANIFEST" || return 1
  E2E_RESOURCE_GENERATIONS["$key"]="$generation"
  E2E_RESOURCE_CURRENT_IDS["$key"]="$resource_id"
}

e2e_resource_release() {
  local kind="$1" logical="$2" key resource_id line
  key="$kind:$logical"
  resource_id="${E2E_RESOURCE_CURRENT_IDS[$key]:-}"
  [ -n "$resource_id" ] || return 0
  line="$(python3 - "$resource_id" <<'PY'
import json,sys
print(json.dumps({"version":1,"op":"release","resource_id":sys.argv[1]},separators=(",",":"),sort_keys=True))
PY
)" || return 1
  printf '%s\n' "$line" >>"$E2E_OWNED_RESOURCE_MANIFEST" || return 1
  unset 'E2E_RESOURCE_CURRENT_IDS[$key]'
}

e2e_process_resource_register() {
  local role="$1" logical="$2" pid="$3" identity="$4" payload
  case "$role" in board-daemon|helper|proxy) ;; *) return 1 ;; esac
  e2e_process_identity_verify "$pid" "$identity" || return 1
  payload="$(python3 - "$pid" "$identity" <<'PY'
import json,sys
print(json.dumps({"pid":sys.argv[1],"identity":json.loads(sys.argv[2])},separators=(",",":"),sort_keys=True))
PY
)" || return 1
  e2e_resource_register_json process "$role" "$logical" "$payload"
}
e2e_process_resource_release() { e2e_resource_release process "$1"; }

# Start a bounded scenario helper/proxy as an exact direct child and ledger its
# complete /proc identity before returning. Arguments identifying its listen
# and control sockets are included in the identity token; payloads are never
# logged. Sets E2E_OWNED_PROCESS_PID / E2E_OWNED_PROCESS_IDENTITY.
declare -Ag E2E_OWNED_PROCESS_PIDS=() E2E_OWNED_PROCESS_IDENTITIES=()
e2e_owned_process_start() {
  local role="$1" logical="$2" identity_session="$3" identity_name="$4" log="$5" command="$6"
  shift 6
  local owner_token pid provisional identity i deferred provisional_logical="process-provisional-$logical"
  case "$role" in helper|proxy) ;; *) fail "invalid owned process role: $role" ;; esac
  [[ "$command" == /* ]] && [ -x "$command" ] || fail "owned process command must be absolute"
  owner_token="$(python3 -c 'import secrets; print(secrets.token_hex(16))')" \
    || fail "cannot generate helper ownership token"
  env E2E_HERDR_OWNER_TOKEN="$owner_token" "$command" "$@" >"$log" 2>&1 &
  pid=$!
  provisional=""
  for (( i=0; i<20; i++ )); do
    provisional="$(e2e_provisional_child_capture "$pid" "$owner_token")" && break
    e2e_process_exists "$pid" || break
    sleep 0.005
  done
  [ -n "$provisional" ] || fail "cannot capture provisional exact-child for $logical"
  E2E_PROVISIONAL_CHILD_ARMED["$provisional_logical"]=1
  printf -v deferred 'e2e_provisional_child_abort %q %q %q' \
    "$provisional_logical" "$pid" "$provisional"
  e2e_defer "$deferred"
  e2e_process_resource_register helper "$provisional_logical" "$pid" "$provisional" \
    || fail "cannot ledger provisional process $logical"
  identity=""
  for (( i=0; i<30; i++ )); do
    identity="$(e2e_process_identity_capture "$pid" "$identity_session" "$identity_name" \
      "$command" "$owner_token" E2E_HERDR_OWNER_TOKEN "$provisional")" && break
    sleep 0.01
  done
  if [ -z "$identity" ]; then
    e2e_provisional_child_abort "$provisional_logical" "$pid" "$provisional" \
      || fail "refusing unsafe provisional cleanup for $logical"
    fail "cannot capture exact process identity for $logical"
  fi
  e2e_process_resource_register "$role" "$logical" "$pid" "$identity" \
    || fail "cannot ledger exact process $logical"
  e2e_process_resource_release "$provisional_logical" \
    || fail "cannot release provisional process $logical"
  E2E_PROVISIONAL_CHILD_ARMED["$provisional_logical"]=0
  E2E_OWNED_PROCESS_PIDS["$logical"]="$pid"
  E2E_OWNED_PROCESS_IDENTITIES["$logical"]="$identity"
  printf -v deferred 'e2e_owned_process_stop %q' "$logical"
  e2e_defer "$deferred"
  E2E_OWNED_PROCESS_PID="$pid"
  E2E_OWNED_PROCESS_IDENTITY="$identity"
}

e2e_owned_process_stop() {
  local logical="$1" pid="${E2E_OWNED_PROCESS_PIDS[$1]:-}" identity="${E2E_OWNED_PROCESS_IDENTITIES[$1]:-}"
  [ -n "$pid" ] || return 0
  e2e_process_identity_verify "$pid" "$identity" || {
    printf 'E2E FAIL: refusing helper stop for %s: identity changed\n' "$logical" >&2; return 1;
  }
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  e2e_process_resource_release "$logical" || return 1
  unset 'E2E_OWNED_PROCESS_PIDS[$logical]' 'E2E_OWNED_PROCESS_IDENTITIES[$logical]'
}

e2e_proxy_start() {
  local listen="$1" control="$2" target="$3" python
  python="$(type -P python3)"
  e2e_owned_process_start proxy herdr-proxy "$listen" "$control" "$E2E_TMP/proxy.log" \
    "$python" "$E2E_LIB_DIR/herdr-proxy.py" --listen "$listen" --control "$control" --target "$target"
  local i
  for (( i=0; i<50; i++ )); do
    [ -S "$listen" ] && [ -S "$control" ] && break
    sleep 0.02
  done
  [ -S "$listen" ] && [ -S "$control" ] || fail "Herdr proxy did not create owned sockets"
  E2E_PROXY_SOCKET="$listen" E2E_PROXY_CONTROL="$control"
  export E2E_PROXY_SOCKET E2E_PROXY_CONTROL
}

e2e_proxy_command() {
  local command="$1"
  e2e_process_identity_verify "${E2E_OWNED_PROCESS_PIDS[herdr-proxy]:-}" \
    "${E2E_OWNED_PROCESS_IDENTITIES[herdr-proxy]:-}" \
    || fail "refusing proxy control: identity changed"
  python3 - "$E2E_PROXY_CONTROL" "$command" <<'PY'
import json,socket,sys
s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(sys.argv[1])
s.sendall(json.dumps({"command":sys.argv[2]}).encode()+b"\n")
f=s.makefile(); response=json.loads(f.readline())
if not response.get("ok"): raise SystemExit(response.get("error","proxy command failed"))
print(json.dumps(response,separators=(",",":"),sort_keys=True))
PY
}

e2e_session_resource_register() {
  local name="$1" identity="$2" registry="$3" owner_marker="$4" pid digest payload
  e2e_identity_token_validate "$identity" || return 1
  pid="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["pid"])' "$identity")" || return 1
  # Live production callers must prove the complete exact Herdr server token.
  # Deterministic dead-PID tests still pass the same semantic token shape.
  if e2e_process_exists "$pid"; then e2e_session_identity_verify "$pid" "$identity" "$name" || return 1; fi
  [ -f "$owner_marker" ] || return 1
  digest="$(e2e_marker_sha256 "$owner_marker")" || return 1
  payload="$(python3 - "$name" "$pid" "$identity" "$registry" "$owner_marker" "$digest" <<'PY'
import json,sys
name,pid,identity,registry,marker,digest=sys.argv[1:]
print(json.dumps({"name":name,"pid":pid,"identity":json.loads(identity),
 "registry":registry,"owner_marker":marker,"marker_sha256":digest},separators=(",",":"),sort_keys=True))
PY
)" || return 1
  e2e_resource_register_json session session "$name" "$payload"
}
e2e_session_resource_release() { e2e_resource_release session "$1"; }

e2e_root_resource_register() {
  local role="$1" logical="$2" path="$3" marker="$4" digest payload
  case "$role" in scenario|managed) ;; *) return 1 ;; esac
  [ -d "$path" ] && [ -f "$marker" ] || return 1
  digest="$(e2e_marker_sha256 "$marker")" || return 1
  payload="$(python3 - "$path" "$marker" "$digest" <<'PY'
import json,sys
print(json.dumps({"path":sys.argv[1],"marker":sys.argv[2],"marker_sha256":sys.argv[3]},separators=(",",":"),sort_keys=True))
PY
)" || return 1
  e2e_resource_register_json root "$role" "$logical" "$payload"
}
e2e_root_resource_release() { e2e_resource_release root "$1"; }

e2e_workspace_resource_register() {
  local logical="$1" marker="$2" digest payload
  [ -f "$marker" ] || return 1
  digest="$(e2e_marker_sha256 "$marker")" || return 1
  payload="$(python3 - "$marker" "$digest" <<'PY'
import json,sys
print(json.dumps({"marker":sys.argv[1],"marker_sha256":sys.argv[2]},separators=(",",":"),sort_keys=True))
PY
)" || return 1
  e2e_resource_register_json workspace workspace "$logical" "$payload"
}
e2e_workspace_resource_release() { e2e_resource_release workspace "$1"; }

e2e_exact_child_path() {
  local path="$1" root="$2"
  [ -n "$root" ] && [[ "$path" == "$root"/* ]] && [ "$(dirname "$path")" = "$root" ] \
    && e2e_path_is_canonical "$path"
}

e2e_script_resource_register() {
  local role="$1" logical="$2" path="$3" digest payload
  case "$role" in configured-runner|temp-script) ;; *) return 1 ;; esac
  e2e_exact_child_path "$path" "${E2E_TMP:-}" && [ -f "$path" ] && [ ! -L "$path" ] || return 1
  digest="$(e2e_marker_sha256 "$path")" || return 1
  payload="$(python3 - "$path" "$digest" <<'PY'
import json,sys
print(json.dumps({"path":sys.argv[1],"content_sha256":sys.argv[2]},separators=(",",":"),sort_keys=True))
PY
)" || return 1
  e2e_resource_register_json script "$role" "$logical" "$payload"
}
e2e_script_resource_release() { e2e_resource_release script "$1"; }
e2e_script_resource_verify() {
  local logical="$1" path="$2" rid="${E2E_RESOURCE_CURRENT_IDS[script:$1]:-}"
  [ -n "$rid" ] && e2e_exact_child_path "$path" "${E2E_TMP:-}" && [ -f "$path" ] && [ ! -L "$path" ] || return 1
  python3 - "$E2E_OWNED_RESOURCE_MANIFEST" "$rid" "$path" <<'PY'
import hashlib,json,sys
manifest,rid,path=sys.argv[1:]
record=None
for line in open(manifest,encoding="utf-8"):
    value=json.loads(line)
    if value.get("resource_id")==rid: record=value
with open(path,"rb") as f: digest=hashlib.sha256(f.read()).hexdigest()
raise SystemExit(0 if record and record.get("path")==path and record.get("content_sha256")==digest else 1)
PY
}
e2e_script_remove_owned() {
  local logical="$1" path="$2"
  e2e_script_resource_verify "$logical" "$path" \
    || { printf 'E2E FAIL: refusing changed/unowned script cleanup: %s\n' "$path" >&2; return 1; }
  rm -f -- "$path" || return 1
  e2e_script_resource_release "$logical"
}

e2e_manifest_event() {
  local event="$1" name="${2:-}" detail="${3:-}" line
  line="$(python3 - "$event" "$name" "$detail" <<'PY'
import datetime,json,sys
print(json.dumps({"time":datetime.datetime.now(datetime.timezone.utc).isoformat(),
 "task":"T00","event":sys.argv[1],"owned_name":sys.argv[2],"detail":sys.argv[3]},
 separators=(",",":"),sort_keys=True))
PY
)" || return 1
  [ -z "${E2E_SCENARIO_ARTIFACT_DIR:-}" ] || printf '%s\n' "$line" >>"$E2E_SCENARIO_ARTIFACT_DIR/manifest-events.ndjson"
  local execution_manifest="${E2E_INVOCATION_ARTIFACT_ROOT:-}/manifest-events.ndjson"
  [ -z "${E2E_INVOCATION_ARTIFACT_ROOT:-}" ] || [ ! -f "$execution_manifest" ] \
    || printf '%s\n' "$line" >>"$execution_manifest"
}

e2e_managed_root_remove_early_owned() {
  local root="${E2E_MANAGED_ROOT:-}"
  [ -e "$root" ] || return 0
  [ "${E2E_MANAGED_ROOT_CREATOR_PID:-}" = "$$" ] \
    && e2e_private_dir_verify "$root" '/tmp/hb-e2e-managed.??????' \
    && e2e_marker_resource_verify root managed-root "$root/.herdr-board-fake-managed" \
    || { printf 'E2E FAIL: refusing early managed-root cleanup: %s\n' "$root" >&2; return 1; }
  rm -rf -- "$root" || return 1
  e2e_root_resource_release managed-root
}

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
      [ -d "$root" ] && e2e_marker_resource_verify root managed-root "$root/.herdr-board-fake-managed" \
        || { printf 'E2E FAIL: refusing changed/unowned managed-root cleanup: %s\n' "$root" >&2; return 1; }
      rm -rf -- "$root"
      e2e_root_resource_release managed-root
      ;;
    *)
      printf 'E2E FAIL: refusing managed-root cleanup outside /tmp: %s\n' "$root" >&2
      return 1
      ;;
  esac
}

e2e_session_owner_marker_create() {
  local name="$1" marker
  marker="$E2E_SCENARIO_ROOT/sessions/$name.owner"
  mkdir -m 700 -p "$E2E_SCENARIO_ROOT/sessions"
  printf 'name=%s\nregistry=%s\n' "$name" "$HOME/.config/herdr/sessions/$name" >"$marker"
  chmod 600 "$marker"
}

e2e_session_delete_authorized() {
  local name="$1" pid="$2" identity="$3" marker registry
  marker="$E2E_SCENARIO_ROOT/sessions/$name.owner"
  registry="$HOME/.config/herdr/sessions/$name"
  ! e2e_process_exists "$pid" || { printf 'E2E FAIL: session process still exists: %s\n' "$name" >&2; return 1; }
  e2e_identity_token_validate "$identity" || { printf 'E2E FAIL: unsigned/changed identity for post-stop delete: %s\n' "$name" >&2; return 1; }
  python3 - "$identity" "$name" "$pid" <<'PY' || {
import json,sys
try: t=json.loads(sys.argv[1])
except Exception: raise SystemExit(1)
name,pid=sys.argv[2:]
if (t.get("pid") != pid or t.get("name") != name or t.get("session") != name
    or not t.get("owner_token")
    or t.get("cmdline") != [t.get("expected_command"), "--session", name, "server"]):
    raise SystemExit(1)
PY
    printf 'E2E FAIL: invalid captured identity for post-stop delete: %s\n' "$name" >&2
    return 1
  }
  e2e_marker_resource_verify session "$name" "$marker" \
    || { printf 'E2E FAIL: changed/missing session owner marker: %s\n' "$name" >&2; return 1; }
  [ "$(sed -n 's/^name=//p' "$marker")" = "$name" ] \
    && [ "$(sed -n 's/^registry=//p' "$marker")" = "$registry" ] \
    && [[ "$registry" == "$E2E_SCENARIO_ROOT/.config/herdr/sessions/$name" ]] \
    || { printf 'E2E FAIL: invalid session owner marker: %s\n' "$name" >&2; return 1; }
  mut "session delete '$name' (private marker authorized)"
  "$HERDR_BIN" session delete "$name" >/dev/null 2>&1
}

# Stop requires a fresh full process token. Delete is separately authorized only
# after the exact captured process is proved gone and the exact private registry
# marker/name is revalidated. A surviving process is reverified and fails closed.
e2e_session_abort_owned() {
  local name="$1" pid="$2" identity="${3:-}" cleanup_rc=0 i
  e2e_session_identity_verify "$pid" "$identity" "$name" || {
    echo "  refusing session cleanup for '$name': server identity does not match" >&2; return 1;
  }
  mut "session stop '$name'"
  if ! "$HERDR_BIN" session stop "$name" >/dev/null 2>&1; then
    if e2e_process_exists "$pid"; then
      e2e_session_identity_verify "$pid" "$identity" "$name" || \
        printf 'E2E FAIL: owner changed after failed stop: %s\n' "$name" >&2
    fi
    printf 'E2E FAIL: session stop failed for %s; delete refused\n' "$name" >&2
    return 1
  fi
  for (( i=0; i<30; i++ )); do
    e2e_process_exists "$pid" || break
    # Bash may retain an exited direct child as a zombie until wait(2). Reap
    # only after its exact token still matches; disappearance is then proven.
    if e2e_session_identity_verify "$pid" "$identity" "$name" \
      && [ "$(e2e_process_state "$pid" 2>/dev/null || true)" = Z ]; then
      wait "$pid" 2>/dev/null || true
      break
    fi
    sleep 0.1
  done
  if e2e_process_exists "$pid"; then
    e2e_session_identity_verify "$pid" "$identity" "$name" || \
      printf 'E2E FAIL: owner changed while waiting for stop: %s\n' "$name" >&2
    printf 'E2E FAIL: exact session process remains after stop: %s\n' "$name" >&2
    return 1
  fi
  wait "$pid" 2>/dev/null || true
  e2e_session_delete_authorized "$name" "$pid" "$identity" || cleanup_rc=1
  local registry_clean=0 registry_dir="$HOME/.config/herdr/sessions/$name"
  # HERDR_SOCKET_PATH still names the just-stopped session, so `session list`
  # may synthesize that exact entry after delete. Audit the exact private
  # registry directory and captured process instead; never inspect a prefix or
  # the caller's registry.
  for (( i=0; i<30; i++ )); do
    if [ ! -e "$registry_dir" ] && ! e2e_process_exists "$pid"; then
      registry_clean=1
      break
    fi
    sleep 0.1
  done
  [ "$registry_clean" = 1 ] || {
    printf 'E2E FAIL: owned session cleanup incomplete: %s\n' "$name" >&2
    cleanup_rc=1
  }
  e2e_manifest_event session_cleanup "$name" "verdict=$([ "$cleanup_rc" = 0 ] && echo clean || echo failed)"
  [ "$cleanup_rc" -ne 0 ] || e2e_session_resource_release "$name" || cleanup_rc=1
  if ! e2e_managed_root_remove_owned "$name"; then cleanup_rc=1; fi
  return "$cleanup_rc"
}

# e2e_session_boot <name> <sockvar> <pidvar> — start an ephemeral herdr server
# for session <name> (`herdr --session <name> server &`), wait (~15s) for its
# socket to accept a tab-less workspace.list, then assign the socket path to
# $sockvar and the server pid to $pidvar in the CALLER's scope. Do NOT call via
# $(...) — a command-substitution subshell would drop the pid and thus its
# teardown (same gotcha as e2e_ws_create).
e2e_session_boot() {
  local name="$1" sockvar="$2" pidvar="$3" identityvar="${4:-}" sock="" i _pid identity owner_token stable provisional logical command
  e2e_session_name_absent "$name"
  e2e_session_owner_marker_create "$name"
  owner_token="$(python3 -c 'import secrets; print(secrets.token_hex(16))')" \
    || fail "cannot generate server ownership token"
  mut "session boot '$name' (herdr --session $name server &)"
  if [ "${E2E_FAKE_MANAGED_ZDOT:-0}" = "1" ]; then
    # Herdr itself keeps the caller's HOME so its disposable session remains
    # discoverable through the normal session registry. Only shells inside
    # that session receive the generated, self-contained startup environment;
    # it never sources $HOME/.zshrc or any other user rc file.
    env -u BASH_ENV -u ENV E2E_HERDR_OWNER_TOKEN="$owner_token" ZDOTDIR="$E2E_MANAGED_ZDOTDIR" \
      "$HERDR_BIN" --session "$name" server >/dev/null 2>&1 &
  else
    env E2E_HERDR_OWNER_TOKEN="$owner_token" \
      "$HERDR_BIN" --session "$name" server >/dev/null 2>&1 &
  fi
  _pid=$!
  printf -v "$pidvar" '%s' "$_pid"
  logical="session-provisional-$name"
  provisional=""
  for (( i=0; i<20; i++ )); do
    provisional="$(e2e_provisional_child_capture "$_pid" "$owner_token")" && break
    e2e_process_exists "$_pid" || break
    sleep 0.005
  done
  if [ -z "$provisional" ]; then
    ! e2e_process_exists "$_pid" && wait "$_pid" 2>/dev/null || true
    fail "refusing ephemeral session '$name': could not capture provisional exact-child evidence"
  fi
  # Arm and defer the stable spawn capability before fresh ledger validation:
  # the launcher may exec Herdr between capture and registration. Any failure
  # from this point terminates/reaps only this exact owner-token child.
  E2E_PROVISIONAL_CHILD_ARMED["$logical"]=1
  printf -v command 'e2e_provisional_child_abort %q %q %q %q %q' \
    "$logical" "$_pid" "$provisional" "$HERDR_BIN" "$name"
  e2e_defer "$command"
  e2e_process_resource_register helper "$logical" "$_pid" "$provisional" \
    || fail "cannot ledger provisional exact-child evidence for '$name'"
  # Capture and register before readiness, registry, socket-path, or protocol
  # checks. The capability permits safe recapture if Herdr's launcher execs and
  # changes /proc identity; no unrelated process can be published by PID/name.
  identity=""
  for (( i=0; i<50; i++ )); do
    identity="$(e2e_process_identity_capture "$_pid" "$name" "$name" "$HERDR_BIN" "$owner_token" \
      E2E_HERDR_OWNER_TOKEN "$provisional")" && break
    sleep 0.02
  done
  if [ -z "$identity" ]; then
    e2e_provisional_child_abort "$logical" "$_pid" "$provisional" "$HERDR_BIN" "$name" \
      || fail "refusing unsafe provisional session cleanup for '$name'"
    e2e_managed_root_remove_owned "$name" || true
    fail "refusing ephemeral session '$name': could not capture server identity"
  fi
  e2e_defer_session_teardown "$name" "$_pid" "$identity"
  e2e_session_resource_register "$name" "$identity" "$HOME/.config/herdr/sessions/$name" \
    "$E2E_SCENARIO_ROOT/sessions/$name.owner" \
    || fail "cannot record exact session ownership for '$name'"
  e2e_process_resource_release "$logical" || fail "cannot release provisional session evidence"
  E2E_PROVISIONAL_CHILD_ARMED["$logical"]=0
  # Capture the real, settled exec identity rather than a transient launcher.
  stable=""
  for (( i=0; i<30; i++ )); do
    sleep 0.1
    stable="$(e2e_process_identity_capture "$_pid" "$name" "$name" "$HERDR_BIN" "$owner_token" \
      E2E_HERDR_OWNER_TOKEN "$provisional")" || continue
    if [ "$stable" = "$identity" ]; then break; fi
    identity="$stable"
    e2e_defer_session_teardown "$name" "$_pid" "$identity"
    e2e_session_resource_register "$name" "$identity" "$HOME/.config/herdr/sessions/$name" \
      "$E2E_SCENARIO_ROOT/sessions/$name.owner" \
      || fail "cannot record replacement session identity for '$name'"
    stable=""
  done
  [ -n "$stable" ] || fail "refusing ephemeral session '$name': server exec identity did not settle"
  [ -z "$identityvar" ] || printf -v "$identityvar" '%s' "$identity"
  for (( i=0; i<75; i++ )); do   # 75 * 0.2s = ~15s
    # A coincident/replacement session is never ours. Check the exact spawned
    # PID before each possible socket publication and once more before returning.
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
      [ "${#sock}" -le 92 ] || fail "session socket path lacks required AF_UNIX margin (${#sock} bytes; max 92)"
      if e2e_process_identity_verify "$_pid" "$identity"; then
        # Re-verify immediately before publishing the socket to the caller.
        printf -v "$sockvar" '%s' "$sock"
        E2E_SESSION_SOCKETS["$name"]="$sock"
        e2e_manifest_event session_boot "$name" "owned pid=$_pid socket_bytes=${#sock}"
        [ -z "${E2E_SCENARIO_ARTIFACT_DIR:-}" ] || printf '%s\n' "$name" >"$E2E_SCENARIO_ARTIFACT_DIR/session.name"
        return 0
      fi
      e2e_managed_root_remove_owned "$name" || true
      fail "ephemeral session '$name' server pid $_pid failed identity check before socket publication"
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
  if ! e2e_session_identity_verify "$pid" "$identity" "$name"; then
    printf "E2E FAIL: refusing session teardown for '%s': server identity does not match\n" "$name" >&2
    return 1
  fi
  e2e_session_abort_owned "$name" "$pid" "$identity"
}

# e2e_session_ensure — guarantee HERDR_SOCKET_PATH points at a newly booted,
# invocation-owned ephemeral Herdr session. Inherited/shared sessions are always
# rejected; run-all children follow the same standalone ownership path. Called by
# e2e_init, BEFORE e2e_daemon_start, so the isolated daemon, herdr CLI, and hrpc
# all treat the ephemeral session as "default".
e2e_session_ensure() {
  # Standard scenarios always reject inherited sessions. run-all scrubs these
  # variables; fail closed as well so standalone parity cannot be bypassed.
  if [ -n "${E2E_SESSION:-}" ] || [ -n "${E2E_SESSION_SOCKET:-}" ]; then
    fail "refusing inherited/shared E2E session; every scenario must boot its own"
  fi
  local name="${E2E_STANDALONE_SESSION:-}"
  [ -n "$name" ] || name="$(e2e_session_name)"
  # If fake-managed panes are enabled, this session is the sole owner of the
  # generated HOME/ZDOTDIR root (never a secondary session).
  export E2E_MANAGED_ROOT_OWNER="$name"
  step "Booting ephemeral herdr session '$name' (standalone, ~2s)"
  e2e_session_boot "$name" E2E_SESSION_SOCKET E2E_SESSION_PID E2E_SESSION_IDENTITY
  export E2E_SESSION="$name"
  export E2E_SESSION_SOCKET E2E_SESSION_PID E2E_SESSION_IDENTITY HERDR_SOCKET_PATH="$E2E_SESSION_SOCKET"
  # e2e_session_boot registered teardown before its first post-spawn check.
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
  # Rust tempfile honors TMPDIR. Keep configured-harness bridge scripts (and
  # sensitive prompt tempfiles, which are intentionally never individually
  # manifested) inside this exact marker-owned root.
  export TMPDIR="$E2E_TMP"
  mkdir -p "$BOARD_SCOPE_PATH"
  BOARD_SCOPE_PATH="$(cd "$BOARD_SCOPE_PATH" && pwd -P)"
  export BOARD_SCOPE_PATH
  e2e_write_config "$HERDR_BOARD_CONFIG"
  printf 'herdr-board scenario temp\n' >"$E2E_TMP/.disposable"
  chmod 600 "$E2E_TMP/.disposable"
  e2e_root_resource_register scenario scenario-temp "$E2E_TMP" "$E2E_TMP/.disposable" \
    || fail "cannot record isolated scenario temp root"
  e2e_defer "e2e_scenario_temp_remove_owned"
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
  local i owner_token provisional logical=daemon-provisional command
  step "Starting isolated boardd (herdr spawner, foreground)"
  owner_token="$(python3 -c 'import secrets; print(secrets.token_hex(16))')" \
    || fail "cannot generate board-daemon ownership token"
  # Keep the generic provisional token too: it lets the existing manifest token
  # verifier ledger the launcher state, while the daemon-specific variable is
  # the capability used for every transition/full-identity check.
  E2E_HERDR_OWNER_TOKEN="$owner_token" E2E_BOARD_DAEMON_OWNER_TOKEN="$owner_token" \
    "$BOARD_BIN" daemon --foreground >"$E2E_TMP/daemon.log" 2>&1 &
  E2E_DAEMON_PID=$!
  E2E_DAEMON_IDENTITY=""
  provisional=""
  for (( i=0; i<20; i++ )); do
    provisional="$(e2e_provisional_child_capture "$E2E_DAEMON_PID" "$owner_token" E2E_BOARD_DAEMON_OWNER_TOKEN)" && break
    e2e_process_exists "$E2E_DAEMON_PID" || break
    sleep 0.005
  done
  if [ -z "$provisional" ]; then
    # No signal is authorized without stable PID/start/parent/exe/argv and owner
    # evidence. Reap only an already-gone child; a live mismatch fails closed.
    ! e2e_process_exists "$E2E_DAEMON_PID" && wait "$E2E_DAEMON_PID" 2>/dev/null || true
    e2e_manifest_event daemon_boot board-daemon "refused provisional-capture"
    fail "refusing isolated daemon: could not capture provisional exact-child evidence"
  fi
  E2E_PROVISIONAL_CHILD_ARMED["$logical"]=1
  printf -v command 'e2e_provisional_child_abort %q %q %q %q %q %q %q' \
    "$logical" "$E2E_DAEMON_PID" "$provisional" "$BOARD_BIN" "" daemon E2E_BOARD_DAEMON_OWNER_TOKEN
  e2e_defer "$command"
  e2e_process_resource_register helper "$logical" "$E2E_DAEMON_PID" "$provisional" \
    || fail "cannot ledger provisional board-daemon evidence"
  # The launcher may exec between provisional capture and full registration.
  # Only the same PID/start/parent/owner may transition to this exact daemon argv.
  for (( i=0; i<25; i++ )); do
    E2E_DAEMON_IDENTITY="$(e2e_process_identity_capture "$E2E_DAEMON_PID" daemon --foreground \
      "$BOARD_BIN" "$owner_token" E2E_BOARD_DAEMON_OWNER_TOKEN "$provisional")" && break
    sleep 0.02
  done
  if [ -z "$E2E_DAEMON_IDENTITY" ]; then
    e2e_provisional_child_abort "$logical" "$E2E_DAEMON_PID" "$provisional" "$BOARD_BIN" "" \
      daemon E2E_BOARD_DAEMON_OWNER_TOKEN \
      || fail "refusing unsafe provisional daemon cleanup"
    fail "refusing isolated daemon: could not capture process identity"
  fi
  export E2E_DAEMON_PID E2E_DAEMON_IDENTITY
  e2e_process_resource_register board-daemon board-daemon "$E2E_DAEMON_PID" "$E2E_DAEMON_IDENTITY" \
    || fail "cannot record exact board-daemon identity"
  e2e_defer "e2e_daemon_stop"
  e2e_process_resource_release "$logical" || fail "cannot release provisional board-daemon evidence"
  E2E_PROVISIONAL_CHILD_ARMED["$logical"]=0
  e2e_manifest_event daemon_boot board-daemon "owned pid=$E2E_DAEMON_PID"
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
  e2e_process_resource_release board-daemon
  E2E_DAEMON_PID="" E2E_DAEMON_IDENTITY=""
}

# Crash/restart scenarios use SIGKILL only after a fresh exact identity check.
e2e_daemon_kill_owned() {
  [ -n "${E2E_DAEMON_PID:-}" ] || return 0
  e2e_process_identity_verify "$E2E_DAEMON_PID" "${E2E_DAEMON_IDENTITY:-}" || {
    printf 'E2E FAIL: refusing daemon kill for pid %s: identity changed\n' "$E2E_DAEMON_PID" >&2; return 1;
  }
  kill -KILL "$E2E_DAEMON_PID" 2>/dev/null || true
  wait "$E2E_DAEMON_PID" 2>/dev/null || true
  e2e_process_resource_release board-daemon || return 1
  E2E_DAEMON_PID="" E2E_DAEMON_IDENTITY=""
}

# Remove only the exact marker-bearing scenario temp root created by
# e2e_isolate; never widen this to a prefix cleanup.
e2e_scenario_temp_remove_owned() {
  local root="${E2E_TMP:-}"
  [ -e "$root" ] || { e2e_root_resource_release scenario-temp; return 0; }
  [[ "$root" == /tmp/hb-e2e.* ]] && [ -d "$root" ] \
    && e2e_marker_resource_verify root scenario-temp "$root/.disposable" || {
    printf 'E2E FAIL: refusing changed/unowned scenario-temp cleanup: %s\n' "$root" >&2; return 1;
  }
  rm -rf -- "$root" || return 1
  e2e_root_resource_release scenario-temp
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
  e2e_session_target_verify "$pid" "$identity" "${sock:-${E2E_SESSION_SOCKET:-}}" \
    || fail "refusing workspace create: session identity/socket does not match"
  mut "workspace create --label $label --no-focus${sock:+ (owned secondary session)}"
  ws_json="$(env ${sock:+HERDR_SOCKET_PATH="$sock"} "$HERDR_BIN" workspace create \
    --label "$label" --no-focus \
    --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET" \
    --env "BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH" "${extra_env[@]}")"
  E2E_WS="$(printf '%s' "$ws_json" | jget workspace_id)" \
    || fail "could not parse workspace_id from workspace-create response"
  mkdir -p "$E2E_SCENARIO_ROOT/workspaces"
  printf 'session=%s\nsocket=%s\n' "${sock:+secondary}" "${sock:-$E2E_SESSION_SOCKET}" >"$E2E_SCENARIO_ROOT/workspaces/$E2E_WS.owned"
  chmod 600 "$E2E_SCENARIO_ROOT/workspaces/$E2E_WS.owned"
  e2e_manifest_event workspace_create "$E2E_WS" owned
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
  e2e_marker_resource_verify workspace "$ws" "$E2E_SCENARIO_ROOT/workspaces/$ws.owned" || {
    printf 'E2E FAIL: refusing changed/unowned workspace close %s\n' "$ws" >&2; return 1;
  }
  if ! e2e_session_target_verify "$pid" "$identity" "${sock:-${E2E_SESSION_SOCKET:-}}"; then
    printf 'E2E FAIL: refusing workspace close %s: session identity/socket does not match\n' "$ws" >&2
    return 1
  fi
  mut "workspace close $ws"
  if [ -n "$sock" ]; then
    HERDR_SOCKET_PATH="$sock" "$HERDR_BIN" workspace close "$ws" >/dev/null 2>&1
  else
    "$HERDR_BIN" workspace close "$ws" >/dev/null 2>&1
  fi
  rm -f -- "$E2E_SCENARIO_ROOT/workspaces/$ws.owned"
  e2e_workspace_resource_release "$ws"
  e2e_manifest_event workspace_close "$ws" owned
}

# e2e_ws_defer_close <workspace_id> [session_socket [session_pid
# [session_identity]]] — register an identity-gated workspace close. Omitted pid
# and token use the primary E2E_SESSION_PID/E2E_SESSION_IDENTITY. Keep mode skips
# the close so the workspace remains available for review.
e2e_ws_defer_close() {
  local ws="$1" sock="${2:-}" pid="${3:-${E2E_SESSION_PID:-}}" identity="${4:-${E2E_SESSION_IDENTITY:-}}" command marker
  # Explicit registration is the ownership assertion for daemon-created
  # workspaces. Persist it before adding cleanup so the close remains gated.
  mkdir -p "$E2E_SCENARIO_ROOT/workspaces"
  marker="$E2E_SCENARIO_ROOT/workspaces/$ws.owned"
  if [ ! -f "$marker" ]; then
    printf 'session=%s\nsocket=%s\n' "${sock:+secondary}" "${sock:-$E2E_SESSION_SOCKET}" >"$marker"
    chmod 600 "$marker"
    e2e_manifest_event workspace_register "$ws" owned
  fi
  if [ -z "${E2E_RESOURCE_CURRENT_IDS[workspace:$ws]:-}" ]; then
    e2e_workspace_resource_register "$ws" "$marker" \
      || fail "cannot record exact workspace marker for $ws"
  fi
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
