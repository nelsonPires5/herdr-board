#!/usr/bin/env bash
# e2e.sh — end-to-end smoke test of herdr-board against a REAL herdr session.
#
# RUN BY THE ORCHESTRATOR / A HUMAN, not in CI without a live herdr. It:
#   1. builds the board binary,
#   2. spins up an ISOLATED boardd (temp DB + socket) with the herdr spawner,
#   3. registers the fake harness (crates/board-cli/tests/fixtures/fake-agent.sh),
#   4. creates a DISPOSABLE herdr workspace,
#   5. CLI path: creates a card, moves it into an auto column, dispatches a real
#      herdr agent pane running the fake harness, polls until the run ends, and
#      asserts outcome=ok + a "fake:" comment,
#   6. TUI path: launches `board tui` in a pane of the workspace, drives the
#      new-card form via send-keys, and reads the pane to assert the card shows,
#   7. tears everything down (daemon, workspace, temp dir) via an EXIT trap.
#
# Every herdr MUTATION is prefixed "HERDR MUTATION:". Verbose by design.
set -euo pipefail

step() { printf '\n=== %s\n' "$*"; }
mut()  { printf 'HERDR MUTATION: %s\n' "$*"; }
fail() { printf 'E2E FAIL: %s\n' "$*" >&2; exit 1; }

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
herdr_bin="${HERDR_BIN_PATH:-herdr}"
fake_agent="$repo_root/crates/board-cli/tests/fixtures/fake-agent.sh"
rpc="$script_dir/board-rpc.py"

command -v "$herdr_bin" >/dev/null 2>&1 || fail "herdr CLI not found ($herdr_bin)"
command -v python3 >/dev/null 2>&1 || fail "python3 required (JSON parsing / board-rpc.py)"
[ -f "$fake_agent" ] || fail "fake-agent.sh missing at $fake_agent"

# --- pull a JSON value by (possibly nested) key, searching recursively --------
# Program passed via -c (not a heredoc) so the piped JSON stays on stdin.
JGET_PY='
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
jget() { # jget <key>   (reads JSON on stdin, prints first matching value)
  python3 -c "$JGET_PY" "$1"
}

step "Building the board binary"
bash "$repo_root/scripts/build.sh"
BOARD_BIN="$repo_root/target/release/board"
[ -x "$BOARD_BIN" ] || fail "board binary not built"

step "Isolated environment (temp DB + socket + config)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/herdr-board-e2e.XXXXXX")"
export BOARD_DB="$tmp/board.db"
export BOARD_SOCKET="$tmp/boardd.sock"
export HERDR_BOARD_CONFIG="$tmp/config.toml"
export BOARD_SPAWNER=herdr
export BOARD_BIN
cat > "$HERDR_BOARD_CONFIG" <<EOF
[daemon]
spawner = "herdr"
timeout_unit_secs = 1
tick_ms = 200

# BOARD_BIN goes through the argv (env wrapper): herdr agent panes only receive
# the env the daemon passes at agent.start (BOARD_CARD_ID/RUN_ID/SOCKET) — they
# do NOT inherit workspace-level --env from `workspace create`.
[harness.fake]
argv = ["env", "BOARD_BIN=$BOARD_BIN", "bash", "$fake_agent"]
EOF
echo "  tmp=$tmp"
echo "  config:"; sed 's/^/    /' "$HERDR_BOARD_CONFIG"

DAEMON_PID=""
WS_ID=""
cleanup() {
  step "Cleanup"
  if [ -n "$WS_ID" ]; then
    mut "workspace close $WS_ID"
    "$herdr_bin" workspace close "$WS_ID" >/dev/null 2>&1 || true
  fi
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    echo "  stopping daemon (pid $DAEMON_PID)"
    "$BOARD_BIN" status >/dev/null 2>&1 || true
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$tmp"
  echo "  done"
}
trap cleanup EXIT

step "Starting isolated boardd (herdr spawner, foreground)"
"$BOARD_BIN" daemon --foreground >"$tmp/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 30); do
  if "$BOARD_BIN" status >/dev/null 2>&1; then break; fi
  sleep 0.2
done
"$BOARD_BIN" status || fail "daemon did not come up (see $tmp/daemon.log)"

step "HERDR MUTATION: create disposable workspace"
mut "workspace create --label board-e2e --no-focus (env BOARD_BIN, BOARD_SOCKET)"
ws_json="$("$herdr_bin" workspace create --label board-e2e --no-focus \
  --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_SOCKET=$BOARD_SOCKET")"
echo "  -> $ws_json"
WS_ID="$(printf '%s' "$ws_json" | jget workspace_id)" || fail "could not parse workspace_id"
echo "  workspace: $WS_ID"

# ============================================================================
step "CLI PATH"
# ----------------------------------------------------------------------------
step "Create an auto column 'Execute' (raw protocol — no CLI verb for columns)"
col_json="$(python3 "$rpc" column.create '{"name":"Execute","trigger":"auto"}')"
echo "  -> $col_json"

step "Create a card on the fake harness targeting the workspace"
card_json="$("$BOARD_BIN" card new --title "E2E CLI Card" \
  -d "e2e cli card" --harness fake \
  --space-kind workspace --space-ref "$WS_ID" --json)"
echo "  -> $card_json"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"

step "Move card into 'Execute' (auto) — this dispatches a real herdr agent pane"
mut "board move $CARD_ID Execute -> daemon calls herdr agent.start in $WS_ID"
"$BOARD_BIN" move "$CARD_ID" Execute --json
echo "  moved; polling for the run to finish (fake harness sleeps then reports)..."

outcome=""
for i in $(seq 1 60); do
  show="$("$BOARD_BIN" card show "$CARD_ID" --json 2>/dev/null || true)"
  outcome="$(printf '%s' "$show" | jget outcome || true)"
  if [ -n "$outcome" ] && [ "$outcome" != "None" ] && [ "$outcome" != "null" ]; then
    break
  fi
  sleep 0.5
done
echo "  run outcome: ${outcome:-<none>}"
if [ "$outcome" != "ok" ]; then
  echo "--- card state:";   "$BOARD_BIN" card show "$CARD_ID" --json || true
  echo "--- daemon log:";   tail -30 "$tmp/daemon.log" || true
  fail "expected run outcome 'ok', got '${outcome:-<none>}'"
fi

step "Assert a 'fake:' comment landed"
show="$("$BOARD_BIN" card show "$CARD_ID" --json)"
printf '%s' "$show" | grep -q "fake:" || fail "no 'fake:' comment on card $CARD_ID"
echo "  OK: card $CARD_ID ran the fake harness (outcome ok, 'fake:' comment present)"

# ============================================================================
step "TUI PATH"
# ----------------------------------------------------------------------------
step "HERDR MUTATION: open a tab in the workspace and launch 'board tui' in it"
mut "tab create --workspace $WS_ID --label board-tui --no-focus"
tab_json="$("$herdr_bin" tab create --workspace "$WS_ID" --label board-tui --no-focus)"
echo "  -> $tab_json"
TAB_ID="$(printf '%s' "$tab_json" | jget tab_id)" || fail "could not parse tab_id"

# The tab-create response already carries the new tab's root pane.
PANE_ID="$(printf '%s' "$tab_json" | jget pane_id)"
[ -n "$PANE_ID" ] || fail "could not find pane for tab $TAB_ID"
echo "  tui pane: $PANE_ID"

# Pane shells do NOT inherit workspace --env; pass the isolated env inline so
# the TUI talks to THIS test's daemon, not the default socket.
mut "pane run $PANE_ID '<board> tui' (isolated BOARD_SOCKET/BOARD_DB)"
"$herdr_bin" pane run "$PANE_ID" \
  "BOARD_SOCKET=$BOARD_SOCKET BOARD_DB=$BOARD_DB HERDR_BOARD_CONFIG=$HERDR_BOARD_CONFIG $BOARD_BIN tui"
echo "  waiting for the TUI to come up..."
sleep 3

step "Drive the new-card form via send-keys (n, type title, Enter)"
mut "pane send-keys $PANE_ID n"
"$herdr_bin" pane send-keys "$PANE_ID" n
sleep 0.5
mut "pane send-text $PANE_ID 'E2E TUI Card'"
"$herdr_bin" pane send-text "$PANE_ID" "E2E TUI Card"
sleep 0.5
mut "pane send-keys $PANE_ID Enter (submit)"
"$herdr_bin" pane send-keys "$PANE_ID" enter
sleep 2

step "Read the TUI pane and assert the new card appears"
screen="$("$herdr_bin" pane read "$PANE_ID" --source recent-unwrapped --lines 200 || true)"
printf '%s\n' "$screen" | grep -q "E2E TUI Card" \
  || fail "new card 'E2E TUI Card' not visible in the TUI pane"
echo "  OK: card created through the TUI is visible on the board"

# Confirm it also exists via the CLI (belt and suspenders).
"$BOARD_BIN" card list --json | grep -q "E2E TUI Card" \
  || fail "TUI-created card not found via CLI card list"

step "ALL E2E CHECKS PASSED"
