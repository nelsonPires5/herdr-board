#!/usr/bin/env bash
# Opt-in, fail-closed REAL Claude Haiku smoke. Intentionally not in run-all.sh.
set -euo pipefail
umask 077

if [ "${E2E_REAL_CLAUDE_HAIKU:-}" != "1" ]; then
  echo "real-claude-haiku-smoke: refusing real provider call; set E2E_REAL_CLAUDE_HAIKU=1 exactly" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd -P)"
RUN_ID="$$"
SESSION="hb-claude-$RUN_ID"
EVIDENCE="/tmp/herdr-board-real-claude-haiku-evidence-$RUN_ID"
STATE="/tmp/hb-claude-$RUN_ID.env"
TMP=""
WORKSPACE_DIR=""
STAGED_CLAUDE_DIR=""
DB=""
SOCKET=""
CONFIG=""
TARGET=""
BOARD_BIN=""
CLAUDE_BIN=""
HERDR_BIN="${HERDR_BIN_PATH:-herdr}"
CARGO_BIN=""
REAL_CLAUDE_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
REAL_CREDENTIALS=""
REAL_SETTINGS=""
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
MARKER="HERDR_BOARD_REAL_CLAUDE_HAIKU_OK"
TASK=""
RUN_SUCCEEDED=0
LAST_ERROR="preflight did not complete"

mkdir -m 700 "$EVIDENCE"
printf 'RUNNING\n' >"$EVIDENCE/result.txt"

# Keep cleanup bound to the exact Linux process we spawned, not a reusable PID.
# The token records /proc start time, executable, expected session/name argv,
# and complete argv; capture refuses a command line without both expected values.
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
  python3 - "$REAL_CREDENTIALS" "$REAL_SETTINGS" "$REAL_HOOK" <<'PY'
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
  printf 'real-claude-haiku-smoke: %s\n' "$*" >&2
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

  # Verify once before every Herdr mutation or process signal. A reused server
  # PID must leave the session/workspace untouched. The board daemon is a
  # separate exact child, so its cleanup must not be hidden behind that Herdr
  # server identity gate.
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

  # Verify the daemon's own /proc token immediately before signaling it. This
  # remains safe and independent when the Herdr server has already exited or
  # no longer matches, without ever falling back to PID-only cleanup.
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
  rm -f -- "$STATE"

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
  [ ! -e "$STATE" ] || cleanup_error+="state_remains;"

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
  if [ -n "$REAL_HASHES_BEFORE" ] && [ -n "$REAL_CREDENTIALS" ] \
      && [ -f "$REAL_CREDENTIALS" ] && [ -f "$REAL_SETTINGS" ] && [ -f "$REAL_HOOK" ]; then
    hashes_after="$(hash_real_files 2>/dev/null)"
    printf '%s\n' "$hashes_after" | jq . >"$EVIDENCE/real-claude-hashes-after.json" 2>/dev/null
    if [ "$hashes_after" != "$REAL_HASHES_BEFORE" ]; then cleanup_error+="real_claude_files_changed;"; fi
  else
    cleanup_error+="real_claude_hash_recheck_failed;"
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
      printf 'model=haiku\n'
      printf 'effort=low\n'
      printf 'permission=bypassPermissions\n'
      printf 'marker_file=%s\n' "$RESULT_FILE"
      printf 'exactly_one_run=yes\n'
      printf 'repo_status_unchanged=yes\n'
      printf 'real_claude_files_unchanged=yes\n'
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

# Tool and executable checks happen before any Herdr mutation.
command -v jq >/dev/null 2>&1 || fail "jq is required"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
CARGO_BIN="$(command -v cargo || true)"
[ -n "$CARGO_BIN" ] || fail "cargo is required"
command -v "$HERDR_BIN" >/dev/null 2>&1 || fail "herdr is required ($HERDR_BIN)"
CLAUDE_BIN="$(type -P claude || true)"
[ -n "$CLAUDE_BIN" ] || fail "real claude executable not found"
CLAUDE_BIN="$(readlink -f -- "$CLAUDE_BIN")"
case "$CLAUDE_BIN" in
  "$ROOT/e2e/fake-bin/claude"|*/e2e/fake-bin/claude)
    fail "refusing checked-in e2e fake claude: $CLAUDE_BIN"
    ;;
esac
if declare -F claude >/dev/null 2>&1; then
  fail "refusing shell function named claude; a physical real Claude executable is required"
fi

HERDR_VERSION="$($HERDR_BIN --version 2>&1)"
[ "$HERDR_VERSION" = "herdr 0.7.5" ] \
  || fail "requires exactly Herdr 0.7.5 (got: $HERDR_VERSION)"
HERDR_SCHEMA="$($HERDR_BIN api schema --json)"
printf '%s' "$HERDR_SCHEMA" | jq -e '.protocol == 17' >/dev/null \
  || fail "requires Herdr schema protocol 17"
CLAUDE_VERSION="$($CLAUDE_BIN --version 2>&1)"
INTEGRATION_LINE="$($HERDR_BIN integration status | awk '$1 == "claude:" {print; exit}')"
printf '%s\n' "$INTEGRATION_LINE" \
  | grep -Eq '^claude:[[:space:]]+current[[:space:]]+\(v7\)([[:space:]]+\(.+\))?$' \
  || fail "Claude Herdr integration must be exactly current v7 (got: ${INTEGRATION_LINE:-missing})"

if "$HERDR_BIN" session list --json | jq -e --arg session "$SESSION" \
    '.sessions[]? | select(.name == $session)' >/dev/null; then
  fail "generated session already exists: $SESSION"
fi

TMP="$(mktemp -d /tmp/hb-claude.XXXXXX)"
WORKSPACE_DIR="$TMP/workspace"
STAGED_CLAUDE_DIR="$TMP/claude"
DB="$TMP/board.db"
SOCKET="$TMP/board.sock"
CONFIG="$TMP/config.toml"
TARGET="$TMP/target"
RESULT_FILE="$WORKSPACE_DIR/result.txt"
mkdir -m 700 "$WORKSPACE_DIR" "$STAGED_CLAUDE_DIR" "$TARGET" "$TMP/zdot"
WORKSPACE_DIR="$(cd "$WORKSPACE_DIR" && pwd -P)"
write_state

REAL_CREDENTIALS="$REAL_CLAUDE_DIR/.credentials.json"
REAL_SETTINGS="$REAL_CLAUDE_DIR/settings.json"
[ -f "$REAL_CREDENTIALS" ] || fail "missing real Claude credentials: $REAL_CREDENTIALS"
[ -f "$REAL_SETTINGS" ] || fail "missing real Claude settings: $REAL_SETTINGS"

# Retain only the installed Herdr SessionStart command and the user's explicit
# dangerous-mode acknowledgement. The hook command is preserved byte-for-byte;
# its script token must already be an absolute path.
python3 - "$REAL_SETTINGS" "$STAGED_CLAUDE_DIR/settings.json" "$TMP/hook-path" <<'PY'
import json, os, pathlib, shlex, sys
src_path, out_path, hook_path_file = sys.argv[1:]
with open(src_path, encoding="utf-8") as stream:
    src = json.load(stream)
if src.get("skipDangerousModePermissionPrompt") is not True:
    raise SystemExit("real settings must acknowledge dangerous permission mode")
session_start = src.get("hooks", {}).get("SessionStart")
if not isinstance(session_start, list):
    raise SystemExit("real settings have no SessionStart hook list")
kept_groups = []
script_paths = []
for group in session_start:
    if not isinstance(group, dict) or not isinstance(group.get("hooks"), list):
        continue
    kept_hooks = []
    for hook in group["hooks"]:
        if not isinstance(hook, dict) or hook.get("type") != "command":
            continue
        command = hook.get("command")
        if not isinstance(command, str):
            continue
        try:
            words = shlex.split(command)
        except ValueError:
            continue
        matches = [word for word in words if word.endswith("/herdr-agent-state.sh")]
        if not matches:
            continue
        if len(matches) != 1 or not os.path.isabs(matches[0]):
            raise SystemExit("Herdr SessionStart hook path must be exactly one absolute script path")
        kept_hooks.append(hook)
        script_paths.append(matches[0])
    if kept_hooks:
        clean_group = {key: value for key, value in group.items() if key != "hooks"}
        clean_group["hooks"] = kept_hooks
        kept_groups.append(clean_group)
if len(script_paths) != 1:
    raise SystemExit(f"expected exactly one Herdr SessionStart command, found {len(script_paths)}")
script = pathlib.Path(script_paths[0])
if not script.is_file():
    raise SystemExit(f"Herdr hook is not a file: {script}")
out = {
    "hooks": {"SessionStart": kept_groups},
    "skipDangerousModePermissionPrompt": True,
}
with open(out_path, "w", encoding="utf-8") as stream:
    json.dump(out, stream, indent=2)
    stream.write("\n")
os.chmod(out_path, 0o600)
with open(hook_path_file, "w", encoding="utf-8") as stream:
    stream.write(str(script))
    stream.write("\n")
os.chmod(hook_path_file, 0o600)
PY
REAL_HOOK="$(<"$TMP/hook-path")"
[ -f "$REAL_HOOK" ] || fail "configured Herdr hook is missing: $REAL_HOOK"

BASE_STATUS="$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all)"
printf '%s\n' "$BASE_STATUS" >"$EVIDENCE/git-status-before.txt"
REAL_HASHES_BEFORE="$(hash_real_files)"
printf '%s\n' "$REAL_HASHES_BEFORE" | jq . >"$EVIDENCE/real-claude-hashes-before.json"

cp -- "$REAL_CREDENTIALS" "$STAGED_CLAUDE_DIR/.credentials.json"
chmod 600 "$STAGED_CLAUDE_DIR/.credentials.json" "$STAGED_CLAUDE_DIR/settings.json"
python3 - "$STAGED_CLAUDE_DIR" <<'PY'
import os, pathlib, stat, sys
root = pathlib.Path(sys.argv[1])
for name in (".credentials.json", "settings.json"):
    path = root / name
    assert path.is_file(), path
    assert stat.S_IMODE(path.stat().st_mode) == 0o600, (path, oct(stat.S_IMODE(path.stat().st_mode)))
PY
jq -e '
  (keys | sort) == ["hooks", "skipDangerousModePermissionPrompt"] and
  .skipDangerousModePermissionPrompt == true and
  (.hooks | keys) == ["SessionStart"] and
  ([.hooks.SessionStart[].hooks[]] | length) == 1
' "$STAGED_CLAUDE_DIR/settings.json" >/dev/null \
  || fail "staged Claude settings are not minimal"

if ! CLAUDE_CONFIG_DIR="$STAGED_CLAUDE_DIR" \
    "$CLAUDE_BIN" auth status --json >"$TMP/claude-auth-status.json" 2>"$TMP/claude-auth-status.err"; then
  fail "staged Claude auth status command failed"
fi
jq -e '.loggedIn == true' "$TMP/claude-auth-status.json" >/dev/null \
  || fail "real Claude auth is not logged in through staged credentials"
printf 'logged_in=yes\nconfig=staged\n' >"$EVIDENCE/auth-preflight.txt"

{
  printf 'herdr_version=%s\n' "$HERDR_VERSION"
  printf 'herdr_schema_protocol=17\n'
  printf 'claude_version=%s\n' "$CLAUDE_VERSION"
  printf 'claude_binary=%s\n' "$CLAUDE_BIN"
  printf 'claude_integration=%s\n' "$INTEGRATION_LINE"
  printf 'claude_hook=%s\n' "$REAL_HOOK"
  printf 'requested_model=haiku\nrequested_effort=low\nrequested_permission=bypassPermissions\n'
} >"$EVIDENCE/preflight.txt"
printf '%s\n' "$INTEGRATION_LINE" >"$EVIDENCE/integration.txt"

# Build and test this checkout's candidate in an isolated target. --locked
# prevents dependency resolution from changing the checkout.
CARGO_TARGET_DIR="$TARGET" "$CARGO_BIN" test --locked \
  --manifest-path "$ROOT/Cargo.toml" -p board-core --test protocol17_spawn \
  >"$EVIDENCE/protocol17-test.log" 2>&1
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

CANDIDATE_PATH="$(dirname "$BOARD_BIN"):$(dirname "$CLAUDE_BIN"):$PATH"
printf 'export PATH=%q\nexport CLAUDE_CONFIG_DIR=%q\n' \
  "$CANDIDATE_PATH" "$STAGED_CLAUDE_DIR" >"$TMP/zdot/.zshrc"
cp "$TMP/zdot/.zshrc" "$TMP/zdot/.zprofile"
chmod 600 "$TMP/zdot/.zshrc" "$TMP/zdot/.zprofile"

printf 'HERDR MUTATION: boot exact disposable real-Claude session %s\n' "$SESSION"
env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID \
  -u HERDR_SOCKET_PATH -u 'BASH_FUNC_claude%%' \
  BOARD_DB="$DB" BOARD_SOCKET="$SOCKET" HERDR_BOARD_CONFIG="$CONFIG" \
  CLAUDE_CONFIG_DIR="$STAGED_CLAUDE_DIR" ZDOTDIR="$TMP/zdot" \
  PATH="$CANDIDATE_PATH" \
  "$HERDR_BIN" --session "$SESSION" server >"$TMP/herdr-server.log" 2>&1 &
SERVER_PID=$!
SERVER_IDENTITY=""
for _ in $(seq 1 25); do
  SERVER_IDENTITY="$(e2e_process_identity_capture "$SERVER_PID" "$SESSION" "$SESSION" "$HERDR_BIN")" && break
  sleep 0.02
done
if [ -z "$SERVER_IDENTITY" ]; then
  # The freshly spawned PID is still this shell's direct child. Stop and reap
  # only that child; do not issue a name-based Herdr mutation without a token.
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
# Do not publish/adopt the socket unless the exact server still owns its token.
e2e_process_identity_verify "$SERVER_PID" "$SERVER_IDENTITY" \
  || fail "disposable Herdr server failed identity check before socket adoption"
write_state
PING="$(HERDR_SOCKET_PATH="$SOCK" python3 "$ROOT/e2e/hrpc.py" ping '{}')"
printf '%s' "$PING" | jq -e '.version == "0.7.5" and .protocol == 17' >/dev/null \
  || fail "disposable session ping is not Herdr 0.7.5 protocol 17"
printf '%s\n' "$PING" >"$EVIDENCE/herdr-ping.json"

printf 'HERDR MUTATION: create one disposable workspace in %s\n' "$SESSION"
WS_JSON="$(HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" workspace create \
  --cwd "$WORKSPACE_DIR" --label "real-claude-haiku-$RUN_ID" --no-focus \
  --env "BOARD_BIN=$BOARD_BIN" --env "BOARD_DB=$DB" --env "BOARD_SOCKET=$SOCKET" \
  --env "HERDR_BOARD_CONFIG=$CONFIG" --env "BOARD_SCOPE_PATH=$WORKSPACE_DIR" \
  --env "CLAUDE_CONFIG_DIR=$STAGED_CLAUDE_DIR")"
WS_ID="$(printf '%s' "$WS_JSON" | jq -er '.result.workspace.workspace_id')"
printf '%s\n' "$WS_JSON" >"$EVIDENCE/workspace-created.json"
write_state

export BOARD_DB="$DB" BOARD_SOCKET="$SOCKET" HERDR_BOARD_CONFIG="$CONFIG"
export HERDR_SOCKET_PATH="$SOCK" BOARD_SPAWNER=herdr BOARD_SCOPE_PATH="$WORKSPACE_DIR"
env CLAUDE_CONFIG_DIR="$STAGED_CLAUDE_DIR" PATH="$CANDIDATE_PATH" \
  "$BOARD_BIN" daemon --foreground >"$TMP/daemon.log" 2>&1 &
DAEMON_PID=$!
DAEMON_IDENTITY=""
# The daemon has no Herdr session/name argv, so bind the same token primitive to
# its executable argv[0] and the literal daemon subcommand instead.
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
  "$BOARD_BIN" status >/dev/null 2>&1 && break
  sleep 0.2
done
"$BOARD_BIN" status >/dev/null 2>&1 || fail "candidate daemon did not become ready"

BOARD_ID="$(python3 "$ROOT/scripts/board-rpc.py" board.open \
  "$(python3 -c 'import json,sys; print(json.dumps({"scope_path":sys.argv[1]}))' "$WORKSPACE_DIR")" \
  | jq -er '.result.board.id')"
EXEC_ID="$(python3 "$ROOT/scripts/board-rpc.py" column.create \
  "$(python3 -c 'import json,sys; print(json.dumps({"board_id":int(sys.argv[1]),"name":"Execute","trigger":"auto","system_prompt":"Perform only the trusted static smoke task in the disposable workspace; follow the herdr-board completion protocol exactly."}))' "$BOARD_ID")" \
  | jq -er '.result.id')"

TASK="Create exactly the file $RESULT_FILE in the disposable workspace. Its complete bytes must be exactly one UTF-8 line: $MARKER followed by one newline. Do not modify any other file. Verify the bytes locally. Then run exactly one board comment whose body contains both marker $MARKER and path $RESULT_FILE, and finish with board done --outcome ok. If any check fails, comment with the failure and use board done --outcome fail."
CARD_JSON="$("$BOARD_BIN" card new --title "Real Claude Haiku smoke" \
  --description "$TASK" --column "$EXEC_ID" --harness claude --model haiku \
  --effort low --permission bypassPermissions --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jq -er '.id')"
printf '%s\n' "$CARD_JSON" >"$EVIDENCE/card-created.json"
write_state
printf 'real-claude-haiku-smoke: card=%s session=%s workspace=%s evidence=%s\n' \
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
[ "$OUTCOME" = "ok" ] || fail "single Claude run ended with outcome '${OUTCOME:-timeout}'"

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
assert out["cards"] == 1 and out["runs"] == 1, out
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
assert card["harness"] == "claude", card
assert card["model"] == "haiku", card
assert card["effort"] == "low", card
assert card["permission_mode"] == "bypassPermissions", card
assert card["space_kind"] == "workspace" and card["space_ref"] == workspace_id, card
assert len(runs) == 1, runs
run = runs[0]
assert run["harness"] == "claude" and run["outcome"] == "ok", run
assert run["herdr_workspace_id"] == workspace_id and run["herdr_pane_id"], run
assert run["prompt_snapshot"].startswith(task + "\n\n"), (run["prompt_snapshot"], task)
assert "board done --outcome ok" in run["prompt_snapshot"], run["prompt_snapshot"]
argv = json.loads(run["argv_json"])
expected_prefix = [
    "claude", "--model", "haiku", "--effort", "low",
    "--permission-mode", "bypassPermissions", "--allowedTools", "Bash(board:*)",
    "--session-id",
]
assert argv[:len(expected_prefix)] == expected_prefix, argv
assert len(argv) == len(expected_prefix) + 1 and argv[-1] == run["session_id"], (argv, run)
assert all(task not in arg and marker not in arg and result_path not in arg for arg in argv), argv
agent_comments = [c for c in comments if c["author"] == f"agent:{run['id']}"]
assert agent_comments, comments
assert any(marker in c["body"] and result_path in c["body"] for c in agent_comments), agent_comments
path = pathlib.Path(result_path)
assert path.is_file() and not path.is_symlink(), path
assert path.read_bytes() == marker.encode("utf-8") + b"\n", path.read_bytes()
panes = snapshot["result"]["snapshot"].get("panes", [])
matched = [p for p in panes if p.get("pane_id") == run["herdr_pane_id"]]
assert len(matched) == 1, (run["herdr_pane_id"], panes)
pane = matched[0]
assert pane.get("agent") == "claude", pane
session = pane.get("agent_session")
assert session and session.get("source") == "herdr:claude" and session.get("agent") == "claude", pane
assert session.get("kind") in ("id", "path") and session.get("value"), session
print("validated one Claude Haiku run, exact startup flags, SessionStart report, comment, and file bytes")
PY

# Recheck the disposable path directly (the Python assertion above deliberately
# receives workspace id separately because Herdr ids are not filesystem paths).
python3 - "$RESULT_FILE" "$WORKSPACE_DIR" <<'PY'
import pathlib, sys
result = pathlib.Path(sys.argv[1])
workspace = pathlib.Path(sys.argv[2]).resolve()
assert result.parent.resolve() == workspace, (result, workspace)
PY

capture_runtime_evidence
LAST_ERROR="none"
RUN_SUCCEEDED=1
exit 0
