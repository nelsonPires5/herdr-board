#!/usr/bin/env bash
# lib.sh — shared harness for the herdr-board LIVE e2e scenarios.
#
# SOURCE this from a scenario (01-core.sh, 02-kanban-grid.sh, …); it is not meant
# to be executed directly. It provides:
#   - logging (step / mut / fail / ok / skip),
#   - path + tool resolution and preconditions (e2e_require),
#   - an idempotent release build (e2e_build),
#   - an ISOLATED stack (short /tmp DB + socket + config with the fake harness),
#   - trap-based cleanup you register as you go (e2e_defer),
#   - daemon start/stop by pid, disposable workspace create (auto-closed),
#   - a card-outcome poller (wait_ok) and JSON helpers (jget),
#   - raw herdr RPC via scripts/e2e/hrpc.py (hrpc), honoring HERDR_SOCKET_PATH.
#
# Conventions kept from the original scripts/e2e.sh: `set -euo pipefail` in the
# scenario, `step`/`mut` echo narration, every herdr MUTATION prefixed
# "HERDR MUTATION:". Mutations only ever hit disposable workspaces this suite
# created; never a workspace/session you care about.

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
REPO_ROOT="$(cd "$E2E_LIB_DIR/../.." && pwd)"
HERDR_BIN="${HERDR_BIN_PATH:-herdr}"
BOARD_BIN="${BOARD_BIN:-$REPO_ROOT/target/release/board}"
E2E_FAKE_AGENT="${E2E_FAKE_AGENT:-$E2E_LIB_DIR/fake-agent.sh}"
HRPC="$E2E_LIB_DIR/hrpc.py"
export BOARD_BIN

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
}

# --- isolated stack ---------------------------------------------------------
# e2e_isolate — create a SHORT /tmp temp dir (AF_UNIX socket paths cap at ~108
# chars, so never nest under a long $TMPDIR) and point BOARD_DB/BOARD_SOCKET/
# HERDR_BOARD_CONFIG at it, with a config that registers the fake harness. Sets
# BOARD_SPAWNER=herdr (real herdr panes). Registers temp-dir removal.
e2e_isolate() {
  E2E_TMP="$(mktemp -d /tmp/hb-e2e.XXXXXX)"
  export BOARD_DB="$E2E_TMP/board.db"
  export BOARD_SOCKET="$E2E_TMP/boardd.sock"
  export HERDR_BOARD_CONFIG="$E2E_TMP/config.toml"
  export BOARD_SPAWNER=herdr
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
  echo "  daemon pid $E2E_DAEMON_PID"
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
# THAT session (via HERDR_SOCKET_PATH); otherwise the default session. Sets the
# new id in the global E2E_WS (NOT stdout — capturing via $(...) would run this
# in a subshell and lose the cleanup registration). Use as:
#     e2e_ws_create board-e2e; WS="$E2E_WS"
e2e_ws_create() {
  local label="$1" sock="${2:-}" ws_json
  mut "workspace create --label $label --no-focus${sock:+ (session socket $sock)}"
  ws_json="$(env ${sock:+HERDR_SOCKET_PATH="$sock"} "$HERDR_BIN" workspace create \
    --label "$label" --no-focus \
    --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET")"
  E2E_WS="$(printf '%s' "$ws_json" | jget workspace_id)" \
    || fail "could not parse workspace_id from: $ws_json"
  e2e_ws_defer_close "$E2E_WS" "$sock"
}

# e2e_ws_defer_close <workspace_id> [session_socket] — register a workspace close
# to run at cleanup (targets the given session socket, else the default session).
e2e_ws_defer_close() {
  local ws="$1" sock="${2:-}"
  e2e_defer "mut 'workspace close $ws'; env ${sock:+HERDR_SOCKET_PATH=\"$sock\"} \"$HERDR_BIN\" workspace close '$ws' >/dev/null 2>&1 || true"
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
