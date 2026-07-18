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
# run gets its own throwaway session named `hb-e2e-<pid>` (started via
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

# Scope the checked-in executable named exactly `pi` to disposable e2e Herdr
# servers. The candidate board binary is also on PATH so fake Pi can call
# `board comment` / `board done` without a custom built-in harness env.
e2e_enable_fake_pi() {
  [ -x "$E2E_FAKE_PI_BIN_DIR/pi" ] || fail "fake pi missing/not executable"
  export PATH="$E2E_FAKE_PI_BIN_DIR:$REPO_ROOT/target/release:$PATH"
}

# e2e_require — verify the tools every scenario needs.
e2e_require() {
  command -v "$HERDR_BIN" >/dev/null 2>&1 || fail "herdr CLI not found ($HERDR_BIN)"
  command -v python3 >/dev/null 2>&1 || fail "python3 required (JSON parsing / hrpc.py)"
  [ -f "$E2E_FAKE_AGENT" ] || fail "fake agent missing at $E2E_FAKE_AGENT"
  [ -f "$HRPC" ] || fail "hrpc.py missing at $HRPC"
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
  local rc=$?
  step "Cleanup"
  local i
  for (( i=${#E2E_CLEANUP[@]}-1; i>=0; i-- )); do
    eval "${E2E_CLEANUP[$i]}" || true
  done
  echo "  done"
  return $rc
}
e2e_init() {
  e2e_require
  trap e2e_cleanup EXIT
  e2e_session_ensure
}

# --- ephemeral herdr session ------------------------------------------------
# e2e_session_boot <name> <sockvar> <pidvar> — start an ephemeral herdr server
# for session <name> (`herdr --session <name> server &`), wait (~15s) for its
# socket to accept a tab-less workspace.list, then assign the socket path to
# $sockvar and the server pid to $pidvar in the CALLER's scope. Do NOT call via
# $(...) — a command-substitution subshell would drop the pid and thus its
# teardown (same gotcha as e2e_ws_create).
e2e_session_boot() {
  local name="$1" sockvar="$2" pidvar="$3" sock="" i _pid
  mut "session boot '$name' (herdr --session $name server &)"
  "$HERDR_BIN" --session "$name" server >/dev/null 2>&1 &
  _pid=$!
  printf -v "$pidvar" '%s' "$_pid"
  for (( i=0; i<75; i++ )); do   # 75 * 0.2s = ~15s
    sock="$("$HERDR_BIN" session list --json 2>/dev/null | python3 -c "
import json, sys
for s in json.load(sys.stdin).get('sessions', []):
    if s.get('name') == '$name':
        print(s.get('socket_path', '')); break
" 2>/dev/null)"
    if [ -n "$sock" ] && [ -S "$sock" ] \
       && HERDR_SOCKET_PATH="$sock" python3 "$HRPC" workspace.list '{}' >/dev/null 2>&1; then
      printf -v "$sockvar" '%s' "$sock"
      return 0
    fi
    sleep 0.2
  done
  fail "ephemeral session '$name' did not answer within ~15s (server pid $_pid)"
}

# e2e_session_teardown <name> [pid] — stop + delete the ephemeral session and,
# as a backstop, kill the server pid we started (session stop normally exits it).
# Under keep mode (E2E_KEEP=1) this is a NO-OP: the session is left running for
# review (run-all / the standalone review block prints the cleanup one-liner).
e2e_session_teardown() {
  local name="$1" pid="${2:-}"
  if [ "${E2E_KEEP:-0}" = "1" ]; then
    echo "  keep: leaving ephemeral session '$name' running (pid ${pid:-?})"
    return 0
  fi
  mut "session stop+delete '$name'"
  "$HERDR_BIN" session stop "$name" >/dev/null 2>&1 || true
  "$HERDR_BIN" session delete "$name" >/dev/null 2>&1 || true
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}

# e2e_session_ensure — guarantee HERDR_SOCKET_PATH points at an ephemeral herdr
# session for this scenario. If run-all exported E2E_SESSION/E2E_SESSION_SOCKET,
# adopt it (run-all owns its teardown). Otherwise boot our own hb-e2e-<pid> and
# register teardown (LIFO: runs AFTER workspaces close + daemon stop). Called by
# e2e_init, BEFORE e2e_daemon_start, so the isolated daemon, herdr CLI, and hrpc
# all treat the ephemeral session as "default".
e2e_session_ensure() {
  if [ -n "${E2E_SESSION:-}" ] && [ -n "${E2E_SESSION_SOCKET:-}" ]; then
    export HERDR_SOCKET_PATH="$E2E_SESSION_SOCKET"
    echo "  ephemeral session (from run-all): $E2E_SESSION"
    echo "  session socket: $E2E_SESSION_SOCKET"
    return 0
  fi
  local name="hb-e2e-$$"
  step "Booting ephemeral herdr session '$name' (standalone, ~2s)"
  e2e_session_boot "$name" E2E_SESSION_SOCKET E2E_SESSION_PID
  export E2E_SESSION="$name"
  export E2E_SESSION_SOCKET HERDR_SOCKET_PATH="$E2E_SESSION_SOCKET"
  e2e_defer "e2e_session_teardown '$name' '${E2E_SESSION_PID:-}'"
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
# goes through an `env` argv wrapper: herdr agent panes only receive the env the
# daemon passes at agent.start (BOARD_CARD_ID/RUN_ID/SOCKET); they do NOT inherit
# workspace-level --env. So BOARD_BIN must be baked into the argv here. A scenario
# may bake extra fake-agent knobs into the wrapper via E2E_FAKE_ENV (a
# space-separated list of KEY=VAL, e.g. "FAKE_AGENT_HOLD=300") — set it BEFORE
# e2e_isolate.
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
# save its pid in E2E_DAEMON_PID, register a stop, and wait until it answers.
e2e_daemon_start() {
  step "Starting isolated boardd (herdr spawner, foreground)"
  "$BOARD_BIN" daemon --foreground >"$E2E_TMP/daemon.log" 2>&1 &
  E2E_DAEMON_PID=$!
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

# e2e_daemon_stop — stop ONLY the daemon we started, by pid (never pattern-kill
# 'board daemon' — that matches our own shell too).
e2e_daemon_stop() {
  if [ -n "${E2E_DAEMON_PID:-}" ] && kill -0 "$E2E_DAEMON_PID" 2>/dev/null; then
    echo "  stopping daemon (pid $E2E_DAEMON_PID)"
    kill "$E2E_DAEMON_PID" 2>/dev/null || true
    wait "$E2E_DAEMON_PID" 2>/dev/null || true
  fi
}

# --- disposable workspace ---------------------------------------------------
# e2e_ws_create <label> [session_socket] — create a disposable herdr workspace
# and register its close. If session_socket is given the create/close target
# THAT session (via HERDR_SOCKET_PATH); otherwise the ephemeral session
# HERDR_SOCKET_PATH already points at (the daemon's "default"). Sets the
# new id in the global E2E_WS (NOT stdout — capturing via $(...) would run this
# in a subshell and lose the cleanup registration). Use as:
#     e2e_ws_create board-e2e; WS="$E2E_WS"
e2e_ws_create() {
  local label="$1" sock="${2:-}" ws_json
  mut "workspace create --label $label --no-focus${sock:+ (session socket $sock)}"
  ws_json="$(env ${sock:+HERDR_SOCKET_PATH="$sock"} "$HERDR_BIN" workspace create \
    --label "$label" --no-focus \
    --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET" \
    --env "BOARD_SCOPE_PATH=$BOARD_SCOPE_PATH")"
  E2E_WS="$(printf '%s' "$ws_json" | jget workspace_id)" \
    || fail "could not parse workspace_id from: $ws_json"
  e2e_ws_defer_close "$E2E_WS" "$sock"
}

# e2e_ws_defer_close <workspace_id> [session_socket] — register a workspace close
# to run at cleanup (targets the given session socket, else the ephemeral session
# HERDR_SOCKET_PATH points at). Under keep mode (E2E_KEEP=1) the close is SKIPPED
# so the workspace stays for review; the review block prints how to clean up.
e2e_ws_defer_close() {
  local ws="$1" sock="${2:-}"
  e2e_defer "if [ \"\${E2E_KEEP:-0}\" = 1 ]; then echo '  keep: leaving workspace $ws for review'; else mut 'workspace close $ws'; env ${sock:+HERDR_SOCKET_PATH=\"$sock\"} \"$HERDR_BIN\" workspace close '$ws' >/dev/null 2>&1 || true; fi"
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
