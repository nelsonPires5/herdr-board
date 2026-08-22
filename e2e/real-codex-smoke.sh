#!/usr/bin/env bash
# Opt-in, fail-closed REAL Codex smoke. Intentionally not in run-all.sh.
#
# One authorized attempt with the real Codex CLI through the real Herdr codex
# integration: no retry, no fallback. It stages ONLY the codex auth, config,
# and the installed current Herdr agent-state hook under a disposable CODEX_HOME
# so startup dialogs cannot consume `agent.prompt`; no broad personal Codex
# state is copied. The independent /proc identity implementation is Linux-only
# and outside the portable provider-free gate (same design as
# real-claude-haiku-smoke.sh).
set -euo pipefail
umask 077

if [ "${E2E_REAL_CODEX:-}" != "1" ]; then
  echo "real-codex-smoke: refusing real provider call; set E2E_REAL_CODEX=1 exactly" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd -P)"
RUN_ID="$$"
SESSION="hb-codex-$RUN_ID"
EVIDENCE="/tmp/herdr-board-real-codex-evidence-$RUN_ID"
TMP=""
WORKSPACE_DIR=""
STAGED_CODEX_HOME=""
DB=""
SOCKET=""
CONFIG=""
TARGET=""
BOARD_BIN=""
CODEX_BIN=""
HERDR_BIN="${HERDR_BIN_PATH:-herdr}"
CARGO_BIN=""
REAL_CODEX_DIR="${CODEX_HOME:-$HOME/.codex}"
REAL_AUTH=""
REAL_CONFIG=""
REAL_HOOK=""
REAL_HASHES_BEFORE=""
BASE_STATUS=""
SERVER_PID=""
SERVER_IDENTITY=""
DAEMON_PID=""
DAEMON_IDENTITY=""
SESSION_STARTED=0
WS_ID=""
CARD_ID=""
RESULT_FILE=""
MARKER="HERDR_BOARD_REAL_CODEX_OK"
TASK=""
RUN_SUCCEEDED=0
LAST_ERROR="preflight did not complete"

mkdir -m 700 "$EVIDENCE"
printf 'RUNNING\n' >"$EVIDENCE/result.txt"

# --- exact Linux /proc identity (independent implementation, like the
# --- real-Claude smoke; documented as Linux-only) ---------------------------
e2e_process_identity_capture() {
  local pid="$1" session="$2" name="$3" expected_command="${4:-}"
  [ -r "/proc/$pid/stat" ] && [ -r "/proc/$pid/cmdline" ] && [ -L "/proc/$pid/exe" ] || return 1
  python3 - "$pid" "$session" "$name" "$expected_command" <<'PY'
import json, os, sys
pid, session, name, expected_command = sys.argv[1:]
try:
    stat = open(f"/proc/{pid}/stat", encoding="utf-8").read()
    start_time = stat[stat.rfind(")") + 2:].split()[19]
    exe = os.readlink(f"/proc/{pid}/exe")
    argv = [part.decode("utf-8", "surrogateescape")
            for part in open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0") if part]
except (IndexError, OSError, UnicodeError):
    raise SystemExit(1)
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

hash_real_files() {
  python3 - "$REAL_AUTH" "$REAL_CONFIG" "$REAL_HOOK" <<'PY'
import hashlib, json, pathlib, sys
out = {}
for raw in sys.argv[1:]:
    path = pathlib.Path(raw)
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    out[str(path)] = h.hexdigest()
print(json.dumps(out, sort_keys=True, separators=(",", ":")))
PY
}

write_state() {
  [ -n "$TMP" ] || return 0
  {
    printf 'SESSION=%q\n' "$SESSION"
    printf 'TMP=%q\n' "$TMP"
    printf 'EVIDENCE=%q\n' "$EVIDENCE"
    printf 'WORKSPACE_DIR=%q\n' "$WORKSPACE_DIR"
    printf 'BOARD_BIN=%q\n' "$BOARD_BIN"
    printf 'BOARD_DB=%q\n' "$DB"
    printf 'BOARD_SOCKET=%q\n' "$SOCKET"
    printf 'HERDR_BOARD_CONFIG=%q\n' "$CONFIG"
    printf 'HERDR_SOCKET_PATH=%q\n' "${SOCK:-}"
    printf 'SESSION_PID=%q\n' "$SERVER_PID"
    printf 'SERVER_IDENTITY=%q\n' "$SERVER_IDENTITY"
    printf 'DAEMON_PID=%q\n' "$DAEMON_PID"
    printf 'DAEMON_IDENTITY=%q\n' "$DAEMON_IDENTITY"
    printf 'WS_ID=%q\n' "$WS_ID"
    printf 'CARD_ID=%q\n' "$CARD_ID"
  } >"$STATE"
  chmod 600 "$STATE"
}

fail() {
  LAST_ERROR="$*"
  printf 'real-codex-smoke: %s\n' "$*" >&2
  exit 1
}

capture_runtime_evidence() {
  local pane_id=""
  if [ -n "$CARD_ID" ] && [ -x "$BOARD_BIN" ]; then
    "$BOARD_BIN" card show "$CARD_ID" --json >"$EVIDENCE/card-final.json" 2>/dev/null || true
  fi
  if [ -n "${SOCK:-}" ] && [ -S "${SOCK:-}" ]; then
    HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" api snapshot >"$EVIDENCE/herdr-snapshot.json" 2>/dev/null || true
    if [ -s "$EVIDENCE/card-final.json" ]; then
      pane_id="$(jq -r '.runs[-1].herdr_pane_id // empty' "$EVIDENCE/card-final.json" 2>/dev/null)"
      if [ -n "$pane_id" ]; then
        HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" pane read "$pane_id" \
          --source recent-unwrapped --lines 200 --format text \
          >"$EVIDENCE/pane-final.txt" 2>"$EVIDENCE/pane-final.err" || true
      fi
    fi
  fi
  [ -z "$TMP" ] || [ ! -f "$TMP/daemon.log" ] || cp "$TMP/daemon.log" "$EVIDENCE/daemon.log"
  [ -z "$TMP" ] || [ ! -f "$TMP/herdr-server.log" ] || cp "$TMP/herdr-server.log" "$EVIDENCE/herdr-server.log"
}

cleanup() {
  local incoming_rc=$? final_rc cleanup_error="" final_status="" hashes_after=""
  trap - EXIT ERR INT TERM
  set +e

  capture_runtime_evidence

  local server_identity_ok=0
  if ! e2e_process_identity_verify "$SERVER_PID" "$SERVER_IDENTITY"; then
    cleanup_error+="server_identity_mismatch;"
  else
    server_identity_ok=1
    if [ -n "$WS_ID" ] && [ -n "${SOCK:-}" ]; then
      printf 'HERDR MUTATION: close disposable workspace %s on recorded socket\n' "$WS_ID"
      HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" workspace close "$WS_ID" >/dev/null 2>&1 \
        || cleanup_error+="workspace_close_failed;"
    fi
  fi

  if [ -n "$DAEMON_PID" ]; then
    if e2e_process_identity_verify "$DAEMON_PID" "$DAEMON_IDENTITY"; then
      kill "$DAEMON_PID" 2>/dev/null || cleanup_error+="daemon_kill_failed;"
      wait "$DAEMON_PID" 2>/dev/null || true
    else
      cleanup_error+="daemon_identity_mismatch;"
    fi
  fi

  if [ "$server_identity_ok" = 1 ] && [ "$SESSION_STARTED" = 1 ]; then
    printf 'HERDR MUTATION: stop/delete disposable session %s\n' "$SESSION"
    "$HERDR_BIN" session stop "$SESSION" >/dev/null 2>&1 || cleanup_error+="session_stop_failed;"
    "$HERDR_BIN" session delete "$SESSION" >/dev/null 2>&1 || cleanup_error+="session_delete_failed;"
  fi
  if [ "$server_identity_ok" = 1 ] && [ -n "$SERVER_PID" ] \
      && e2e_process_identity_verify "$SERVER_PID" "$SERVER_IDENTITY"; then
    kill "$SERVER_PID" 2>/dev/null || cleanup_error+="server_kill_failed;"
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  [ -z "$TMP" ] || rm -rf -- "$TMP"
  [ -z "${STATE:-}" ] || rm -f -- "$STATE"

  local sessions_json=""
  if ! sessions_json="$("$HERDR_BIN" session list --json 2>/dev/null)"; then
    cleanup_error+="session_absence_unverified;"
  elif ! printf '%s' "$sessions_json" | jq -e '.sessions | type == "array"' >/dev/null 2>&1; then
    cleanup_error+="session_list_invalid;"
  elif printf '%s' "$sessions_json" \
      | jq -e --arg session "$SESSION" '.sessions[]? | select(.name == $session)' >/dev/null 2>&1; then
    cleanup_error+="session_remains;"
  fi
  if [ -n "$TMP" ] && [ -e "$TMP" ]; then cleanup_error+="temp_remains;"; fi
  if [ -n "${SOCK:-}" ] && [ -e "${SOCK:-}" ]; then cleanup_error+="socket_remains;"; fi
  [ -z "${STATE:-}" ] || [ ! -e "$STATE" ] || cleanup_error+="state_remains;"

  if [ -n "$BASE_STATUS" ] || [ -d "$ROOT/.git" ]; then
    if ! final_status="$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all 2>/dev/null)"; then
      cleanup_error+="repo_status_recheck_failed;"
    else
      printf '%s\n' "$final_status" >"$EVIDENCE/git-status-after.txt"
      if [ "$final_status" != "$BASE_STATUS" ]; then cleanup_error+="repo_status_changed;"; fi
    fi
  else
    cleanup_error+="repo_status_baseline_missing;"
  fi
  if [ -n "$REAL_HASHES_BEFORE" ] && [ -f "$REAL_AUTH" ] && [ -f "$REAL_CONFIG" ] \
      && [ -f "$REAL_HOOK" ]; then
    hashes_after="$(hash_real_files 2>/dev/null)"
    printf '%s\n' "$hashes_after" | jq . >"$EVIDENCE/real-codex-hashes-after.json" 2>/dev/null
    if [ "$hashes_after" != "$REAL_HASHES_BEFORE" ]; then cleanup_error+="real_codex_files_changed;"; fi
  else
    cleanup_error+="real_codex_hash_recheck_failed;"
  fi

  if [ "$incoming_rc" -ne 0 ] || [ "$RUN_SUCCEEDED" -ne 1 ] || [ -n "$cleanup_error" ]; then
    final_rc=1
    {
      echo "FAIL"
      printf 'reason=%s\n' "$LAST_ERROR"
      printf 'cleanup_errors=%s\n' "${cleanup_error:-none}"
      printf 'session=%s\n' "$SESSION"
      printf 'cleanup_verified=%s\n' "$([ -z "$cleanup_error" ] && echo yes || echo no)"
      printf 'evidence=%s\n' "$EVIDENCE"
    } >"$EVIDENCE/result.txt"
    cat "$EVIDENCE/result.txt" >&2
  else
    final_rc=0
    {
      echo "PASS"
      printf 'candidate_board=%s\n' "$BOARD_BIN"
      printf 'session=%s\n' "$SESSION"
      printf 'card_id=%s\n' "$CARD_ID"
      printf 'effort=low\n'
      printf 'exactly_one_run=yes\n'
      printf 'repo_status_unchanged=yes\n'
      printf 'real_codex_files_unchanged=yes\n'
      printf 'cleanup_verified=yes\n'
      printf 'evidence=%s\n' "$EVIDENCE"
    } >"$EVIDENCE/result.txt"
    cat "$EVIDENCE/result.txt"
  fi
  exit "$final_rc"
}
trap 'LAST_ERROR="command failed at line $LINENO"' ERR
trap cleanup EXIT
trap 'LAST_ERROR="interrupted"; exit 130' INT TERM

# --- preflight: tools, real codex, current Herdr codex integration -----------
command -v jq >/dev/null 2>&1 || fail "jq is required"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
CARGO_BIN="$(command -v cargo || true)"
[ -n "$CARGO_BIN" ] || fail "cargo is required"
command -v "$HERDR_BIN" >/dev/null 2>&1 || fail "herdr is required ($HERDR_BIN)"
CODEX_BIN="$(type -P codex || true)"
[ -n "$CODEX_BIN" ] || fail "real codex executable not found"
CODEX_BIN="$(readlink -f -- "$CODEX_BIN")"
case "$CODEX_BIN" in
  "$ROOT/e2e/fake-bin/codex"|*/e2e/fake-bin/codex)
    fail "refusing checked-in e2e fake codex: $CODEX_BIN"
    ;;
esac
if declare -F codex >/dev/null 2>&1; then
  fail "refusing shell function named codex; a physical real Codex executable is required"
fi

HERDR_VERSION="$($HERDR_BIN --version 2>&1)"
[ "$HERDR_VERSION" = "herdr 0.8.2" ] \
  || fail "requires exactly Herdr 0.8.2 (got: $HERDR_VERSION)"
HERDR_SCHEMA="$($HERDR_BIN api schema --json)"
printf '%s' "$HERDR_SCHEMA" | jq -e '.protocol == 20' >/dev/null \
  || fail "requires Herdr schema protocol 20"
CODEX_VERSION="$($CODEX_BIN --version 2>&1)"
INTEGRATION_LINE="$($HERDR_BIN integration status | awk '$1 == "codex:" {print; exit}')"
printf '%s\n' "$INTEGRATION_LINE" \
  | grep -Eq '^codex:[[:space:]]+current([[:space:]]+\(.+\))?$' \
  || fail "Codex Herdr integration must be current (got: ${INTEGRATION_LINE:-missing})"

if "$HERDR_BIN" session list --json | jq -e --arg session "$SESSION" \
    '.sessions[]? | select(.name == $session)' >/dev/null; then
  fail "generated session already exists: $SESSION"
fi

TMP="$(mktemp -d /tmp/hb-codex.XXXXXX)"
STATE="$TMP/state.env"
WORKSPACE_DIR="$TMP/workspace"
STAGED_CODEX_HOME="$TMP/codex-home"
DB="$TMP/board.db"
SOCKET="$TMP/board.sock"
CONFIG="$TMP/config.toml"
TARGET="$TMP/target"
RESULT_FILE="$WORKSPACE_DIR/result.txt"
mkdir -m 700 "$WORKSPACE_DIR" "$STAGED_CODEX_HOME" "$TARGET" "$TMP/zdot"
WORKSPACE_DIR="$(cd "$WORKSPACE_DIR" && pwd -P)"
write_state

REAL_AUTH="$REAL_CODEX_DIR/auth.json"
REAL_CONFIG="$REAL_CODEX_DIR/config.toml"
REAL_HOOK="$REAL_CODEX_DIR/herdr-agent-state.sh"
[ -f "$REAL_AUTH" ] || fail "missing real Codex auth: $REAL_AUTH"
[ -f "$REAL_CONFIG" ] || fail "missing real Codex config: $REAL_CONFIG"
[ -f "$REAL_HOOK" ] || fail "missing the current Herdr Codex agent-state hook: $REAL_HOOK"

# Stage only the credentials, the config, and the installed current Herdr hook
# under a disposable CODEX_HOME — no history, models cache, or other personal
# Codex state crosses the boundary.
cp "$REAL_AUTH" "$STAGED_CODEX_HOME/auth.json"
cp "$REAL_CONFIG" "$STAGED_CODEX_HOME/config.toml"
cp "$REAL_HOOK" "$STAGED_CODEX_HOME/herdr-agent-state.sh"
chmod 600 "$STAGED_CODEX_HOME/auth.json" "$STAGED_CODEX_HOME/config.toml" \
  "$STAGED_CODEX_HOME/herdr-agent-state.sh"

BASE_STATUS="$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all)"
printf '%s\n' "$BASE_STATUS" >"$EVIDENCE/git-status-before.txt"
REAL_HASHES_BEFORE="$(hash_real_files)"
printf '%s\n' "$REAL_HASHES_BEFORE" | jq . >"$EVIDENCE/real-codex-hashes-before.json"

# Auth preflight through the staged CODEX_HOME (read-only; no launch yet).
if ! CODEX_HOME="$STAGED_CODEX_HOME" "$CODEX_BIN" login status >"$TMP/codex-login-status.txt" 2>&1; then
  fail "staged Codex auth status command failed"
fi
grep -qiE 'logged in|Logged in' "$TMP/codex-login-status.txt" \
  || fail "real Codex auth is not logged in through staged credentials"
printf 'logged_in=yes\nconfig=staged\n' >"$EVIDENCE/auth-preflight.txt"

{
  printf 'herdr_version=%s\n' "$HERDR_VERSION"
  printf 'herdr_schema_protocol=20\n'
  printf 'codex_version=%s\n' "$CODEX_VERSION"
  printf 'codex_binary=%s\n' "$CODEX_BIN"
  printf 'codex_integration=%s\n' "$INTEGRATION_LINE"
  printf 'codex_hook=%s\n' "$REAL_HOOK"
  printf 'requested_effort=low\n'
} >"$EVIDENCE/preflight.txt"
printf '%s\n' "$INTEGRATION_LINE" >"$EVIDENCE/integration.txt"

# Build and test this checkout's candidate in an isolated target. --locked
# prevents dependency resolution from changing the checkout.
CARGO_TARGET_DIR="$TARGET" "$CARGO_BIN" test --locked \
  --manifest-path "$ROOT/Cargo.toml" -p board-core --test managed_spawn \
  >"$EVIDENCE/protocol19-test.log" 2>&1
CARGO_TARGET_DIR="$TARGET" "$CARGO_BIN" build --locked --release \
  --manifest-path "$ROOT/Cargo.toml" -p board-cli \
  >"$EVIDENCE/candidate-build.log" 2>&1
BOARD_BIN="$TARGET/release/board"
[ -x "$BOARD_BIN" ] || fail "candidate release board was not built"
EXPECTED_VERSION="$(CARGO_TARGET_DIR="$TARGET" "$CARGO_BIN" metadata --locked --no-deps \
  --format-version 1 --manifest-path "$ROOT/Cargo.toml" \
  | jq -er '.packages[] | select(.name == "board-cli") | .version')"
BOARD_VERSION="$($BOARD_BIN --version)"
[ "$BOARD_VERSION" = "board $EXPECTED_VERSION" ] \
  || fail "candidate version assertion failed: expected board $EXPECTED_VERSION, got $BOARD_VERSION"
{
  printf 'candidate_binary=%s\n' "$BOARD_BIN"
  printf 'candidate_version=%s\n' "$BOARD_VERSION"
  printf 'expected_workspace_version=%s\n' "$EXPECTED_VERSION"
} >"$EVIDENCE/board-version.txt"
write_state

cat >"$CONFIG" <<'EOF'
[daemon]
spawner = "herdr"
tick_ms = 200
EOF
chmod 600 "$CONFIG"

CANDIDATE_PATH="$(dirname "$BOARD_BIN"):$(dirname "$CODEX_BIN"):$PATH"
printf 'export PATH=%q\nexport CODEX_HOME=%q\n' \
  "$CANDIDATE_PATH" "$STAGED_CODEX_HOME" >"$TMP/zdot/.zshrc"
cp "$TMP/zdot/.zshrc" "$TMP/zdot/.zprofile"
chmod 600 "$TMP/zdot/.zshrc" "$TMP/zdot/.zprofile"

# --- disposable session, workspace, daemon, and ONE card ---------------------
printf 'HERDR MUTATION: boot exact disposable real-Codex session %s\n' "$SESSION"
env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID \
  -u HERDR_SOCKET_PATH -u 'BASH_FUNC_codex%%' \
  BOARD_DB="$DB" BOARD_SOCKET="$SOCKET" HERDR_BOARD_CONFIG="$CONFIG" \
  CODEX_HOME="$STAGED_CODEX_HOME" ZDOTDIR="$TMP/zdot" \
  PATH="$CANDIDATE_PATH" \
  "$HERDR_BIN" --session "$SESSION" server >"$TMP/herdr-server.log" 2>&1 &
SERVER_PID=$!
SERVER_IDENTITY=""
for _ in $(seq 1 25); do
  SERVER_IDENTITY="$(e2e_process_identity_capture "$SERVER_PID" "$SESSION" "$SESSION" "$HERDR_BIN")" && break
  sleep 0.02
done
if [ -z "$SERVER_IDENTITY" ]; then
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  fail "could not capture disposable Herdr server identity"
fi
SESSION_STARTED=1
write_state

SOCK=""
for _ in $(seq 1 75); do
  if ! e2e_process_identity_verify "$SERVER_PID" "$SERVER_IDENTITY"; then
    fail "disposable Herdr server failed identity check before readiness"
  fi
  SOCK="$($HERDR_BIN session list --json 2>/dev/null \
    | jq -r --arg session "$SESSION" \
      '.sessions[]? | select(.name == $session and .running == true) | .socket_path' \
    | head -1)"
  [ -n "$SOCK" ] && [ -S "$SOCK" ] && break
  sleep 0.2
done
[ -n "$SOCK" ] && [ -S "$SOCK" ] || fail "disposable Herdr session failed to boot"
e2e_process_identity_verify "$SERVER_PID" "$SERVER_IDENTITY" \
  || fail "disposable Herdr server failed identity check before socket publication"
write_state
PING="$(HERDR_SOCKET_PATH="$SOCK" python3 "$ROOT/e2e/hrpc.py" ping '{}')"
printf '%s' "$PING" | jq -e '.version == "0.8.2" and .protocol == 20' >/dev/null \
  || fail "disposable session ping is not Herdr 0.8.2 protocol 20"
printf '%s\n' "$PING" >"$EVIDENCE/herdr-ping.json"

printf 'HERDR MUTATION: create one disposable workspace in %s\n' "$SESSION"
WS_JSON="$(HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" workspace create \
  --cwd "$WORKSPACE_DIR" --label "real-codex-$RUN_ID" --no-focus \
  --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_DB=$DB" --env "BOARD_SOCKET=$SOCKET" \
  --env "HERDR_BOARD_CONFIG=$CONFIG" --env "BOARD_SCOPE_PATH=$WORKSPACE_DIR" \
  --env "CODEX_HOME=$STAGED_CODEX_HOME" --env "PATH=$CANDIDATE_PATH")"
WS_ID="$(printf '%s' "$WS_JSON" | jq -er '.result.workspace.workspace_id // .workspace.workspace_id')"
printf '%s\n' "$WS_JSON" >"$EVIDENCE/workspace-created.json"
write_state

export BOARD_DB="$DB" BOARD_SOCKET="$SOCKET" HERDR_BOARD_CONFIG="$CONFIG"
export HERDR_SOCKET_PATH="$SOCK" BOARD_SPAWNER=herdr BOARD_SCOPE_PATH="$WORKSPACE_DIR"
env CODEX_HOME="$STAGED_CODEX_HOME" PATH="$CANDIDATE_PATH" \
  "$BOARD_BIN" daemon --foreground >"$TMP/daemon.log" 2>&1 &
DAEMON_PID=$!
DAEMON_IDENTITY=""
for _ in $(seq 1 25); do
  DAEMON_IDENTITY="$(e2e_process_identity_capture "$DAEMON_PID" "$BOARD_BIN" daemon "$BOARD_BIN")" && break
  sleep 0.02
done
if [ -z "$DAEMON_IDENTITY" ]; then
  kill "$DAEMON_PID" 2>/dev/null || true
  wait "$DAEMON_PID" 2>/dev/null || true
  fail "could not capture candidate board daemon identity"
fi
write_state
for _ in $(seq 1 50); do
  "$BOARD_BIN" daemon status >/dev/null 2>&1 && break
  sleep 0.2
done
"$BOARD_BIN" daemon status >/dev/null 2>&1 || fail "candidate daemon did not become ready"

BOARD_ID="$(python3 "$ROOT/scripts/board-rpc.py" board.open \
  "$(python3 -c 'import json,sys; print(json.dumps({"scope_path":sys.argv[1]}))' "$WORKSPACE_DIR")" \
  | jq -er '.result.board.id')"
EXEC_ID="$(python3 "$ROOT/scripts/board-rpc.py" column.create \
  "$(python3 -c 'import json,sys; print(json.dumps({"board_id":int(sys.argv[1]),"name":"Execute","trigger":"auto","system_prompt":"Perform only the trusted static smoke task in the disposable workspace; follow the herdr-board completion protocol exactly."}))' "$BOARD_ID")" \
  | jq -er '.result.id')"

TASK="Create exactly the file $RESULT_FILE in the disposable workspace. Its complete bytes must be exactly one UTF-8 line: $MARKER followed by one newline. Do not modify any other file. Verify the bytes locally. Then run exactly one board comment whose body contains both marker $MARKER and path $RESULT_FILE, and finish with board done --outcome ok. If any check fails, comment with the failure and use board done --outcome fail."
CARD_JSON="$("$BOARD_BIN" card new --title "Real Codex smoke" \
  --description "$TASK" --column "$EXEC_ID" --harness codex \
  --effort low --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jq -er '.id')"
printf '%s\n' "$CARD_JSON" >"$EVIDENCE/card-created.json"
write_state
printf 'real-codex-smoke: card=%s session=%s workspace=%s evidence=%s\n' \
  "$CARD_ID" "$SESSION" "$WS_ID" "$EVIDENCE"

# One bounded polling window; there is deliberately no retry or fallback path.
OUTCOME=""
: >"$EVIDENCE/status-samples.jsonl"
for poll in $(seq 1 600); do
  SHOW="$("$BOARD_BIN" card show "$CARD_ID" --json 2>/dev/null || true)"
  if printf '%s' "$SHOW" | jq -e '.card.id != null' >/dev/null 2>&1; then
    printf '%s' "$SHOW" | jq -c --argjson poll "$poll" \
      '{poll:$poll,status:.card.status,run_count:(.runs|length),outcome:(.runs[-1].outcome // null)}' \
      >>"$EVIDENCE/status-samples.jsonl"
    OUTCOME="$(printf '%s' "$SHOW" | jq -r '.runs[-1].outcome // empty')"
  fi
  SNAP="$(HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" api snapshot 2>/dev/null || true)"
  if printf '%s' "$SNAP" | jq -e '.result.snapshot' >/dev/null 2>&1; then
    printf '%s\n' "$SNAP" >"$EVIDENCE/herdr-snapshot.json"
  fi
  [ -z "$OUTCOME" ] || break
  sleep 0.5
done
[ "$OUTCOME" = "ok" ] || fail "single Codex run ended with outcome '${OUTCOME:-timeout}'"

"$BOARD_BIN" card show "$CARD_ID" --json >"$EVIDENCE/card-final.json"
HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" api snapshot >"$EVIDENCE/herdr-snapshot.json"
python3 - "$DB" "$EVIDENCE/counts.json" <<'PY'
import json, sqlite3, sys
con = sqlite3.connect(sys.argv[1])
out = {
    "boards": con.execute("select count(*) from boards").fetchone()[0],
    "cards": con.execute("select count(*) from cards").fetchone()[0],
    "runs": con.execute("select count(*) from runs").fetchone()[0],
}
with open(sys.argv[2], "w", encoding="utf-8") as stream:
    json.dump(out, stream, sort_keys=True)
    stream.write("\n")
assert out["cards"] == 1 and out["runs"] == 1
PY

python3 - "$EVIDENCE/card-final.json" "$EVIDENCE/herdr-snapshot.json" \
  "$RESULT_FILE" "$MARKER" "$TASK" "$WS_ID" <<'PY'
import json, pathlib, sys
show_path, snapshot_path, result_path, marker, task, workspace_id = sys.argv[1:]
show = json.load(open(show_path, encoding="utf-8"))
snapshot = json.load(open(snapshot_path, encoding="utf-8"))
card = show["card"]
runs = show["runs"]
comments = show["comments"]
assert card["harness"] == "codex"
assert card["effort"] == "low"
assert card["space_kind"] == "workspace" and card["space_ref"] == workspace_id
assert len(runs) == 1
run = runs[0]
assert run["harness"] == "codex" and run["outcome"] == "ok"
assert run["herdr_workspace_id"] == workspace_id and run["herdr_pane_id"]
# The thread id is the integration-reported self-minted conversation id.
assert run["session_id"] and run["session_id"] != "null"
assert run["prompt_snapshot"].startswith(task + "\n\n")
assert "board done --outcome ok" in run["prompt_snapshot"]
argv = json.loads(run["argv_json"])
assert argv[0] == "codex"
assert "-c" in argv and argv[argv.index("-c") + 1] == "model_reasoning_effort=low"
assert not any(arg.startswith("instructions=") for arg in argv)
assert all(task not in arg and marker not in arg and result_path not in arg for arg in argv)
agent_comments = [c for c in comments if c["author"] == f"agent:{run['id']}"]
assert agent_comments
assert any(marker in c["body"] and result_path in c["body"] for c in agent_comments)
path = pathlib.Path(result_path)
assert path.is_file() and not path.is_symlink()
assert path.read_bytes() == marker.encode("utf-8") + b"\n"
panes = snapshot["result"]["snapshot"].get("panes", [])
matched = [p for p in panes if p.get("pane_id") == run["herdr_pane_id"]]
assert len(matched) == 1
pane = matched[0]
assert pane.get("agent") == "codex"
session = pane.get("agent_session")
assert session and session.get("source") == "herdr:codex" and session.get("agent") == "codex"
assert session.get("kind") == "id" and session.get("value") == run["session_id"]
print("validated one Codex run, exact startup flags, reported thread id, comment, and file bytes")
PY

# Recheck the disposable path directly (the Python assertion above deliberately
# receives workspace id separately because Herdr ids are not filesystem paths).
python3 - "$RESULT_FILE" "$WORKSPACE_DIR" <<'PY'
import pathlib, sys
result = pathlib.Path(sys.argv[1])
workspace = pathlib.Path(sys.argv[2]).resolve()
assert result.parent.resolve() == workspace
PY

capture_runtime_evidence
LAST_ERROR="none"
RUN_SUCCEEDED=1
exit 0
