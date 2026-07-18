#!/usr/bin/env bash
# Opt-in, isolated REAL Pi smoke. This is intentionally outside run-all.sh.
set -euo pipefail

[ "${E2E_REAL_PI:-0}" = "1" ] || {
  echo "real-pi-smoke: refusing real provider call; set E2E_REAL_PI=1 explicitly" >&2
  exit 2
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
HERDR_BIN="${HERDR_BIN_PATH:-herdr}"
PI_BIN="$(command -v pi || true)"
[ -n "$PI_BIN" ] || { echo "real-pi-smoke: pi not found" >&2; exit 2; }
case "$PI_BIN" in *'/e2e/fake-bin/pi') echo "real-pi-smoke: fake Pi is on PATH" >&2; exit 2;; esac
command -v python3 >/dev/null || { echo "real-pi-smoke: python3 required" >&2; exit 2; }
command -v jq >/dev/null || { echo "real-pi-smoke: jq required" >&2; exit 2; }
env -u WEZTERM_UNIX_SOCKET wezterm cli list --format json >/dev/null \
  || { echo "real-pi-smoke: WezTerm CLI unavailable" >&2; exit 2; }

SETTINGS_DIR="${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}"
SETTINGS="$SETTINGS_DIR/settings.json"
[ -f "$SETTINGS" ] || { echo "real-pi-smoke: missing $SETTINGS" >&2; exit 2; }
PROVIDER="$(jq -er '.defaultProvider' "$SETTINGS")"
MODEL="$(jq -er '.defaultModel' "$SETTINGS")"
DEFAULT_MODEL="$PROVIDER/$MODEL"
DEFAULT_THINKING="$(jq -er '.defaultThinkingLevel' "$SETTINGS")"
PI_VERSION="$(pi --version)"
MODEL_ROW="$(pi --list-models "$DEFAULT_MODEL" | awk -v p="$PROVIDER" -v m="$MODEL" '$1==p && $2==m {print; found=1} END{if(!found) exit 1}')" \
  || { echo "real-pi-smoke: default model $DEFAULT_MODEL not in pi --list-models" >&2; exit 2; }
INTEGRATION="$("$HERDR_BIN" integration status | awk '$1=="pi:" {print}')"
printf '%s\n' "$INTEGRATION" | grep -q 'current' \
  || { echo "real-pi-smoke: Pi Herdr integration is not current: $INTEGRATION" >&2; exit 2; }

RUN_ID="$$"
SESSION="hb-pi-$RUN_ID"
TMP="$(mktemp -d /tmp/hb-pi-smoke.XXXXXX)"
TARGET="${E2E_REAL_PI_TARGET:-$TMP/target}"
EVIDENCE="${E2E_REAL_PI_EVIDENCE:-/tmp/herdr-board-real-pi-evidence-$RUN_ID}"
STATE="${E2E_REAL_PI_STATE:-/tmp/hb-pi-$RUN_ID.env}"
WORKSPACE_DIR="$TMP/workspace"
POEM="$TMP/poema.txt"
DB="$TMP/board.db"
SOCKET="$TMP/board.sock"
CONFIG="$TMP/config.toml"
PI_SESSIONS="$TMP/pi-sessions"
mkdir -p "$WORKSPACE_DIR" "$PI_SESSIONS" "$EVIDENCE"

SERVER_PID=""
DAEMON_PID=""
WS_ID=""
SOCK=""
TARGET_LINK_CREATED=0
cleanup() {
  local rc=$?
  if [ "${E2E_REAL_PI_KEEP:-0}" = "1" ]; then
    echo "real-pi-smoke: KEEP enabled; state=$STATE session=$SESSION tmp=$TMP evidence=$EVIDENCE"
    return "$rc"
  fi
  if [ -n "$WS_ID" ]; then
    printf 'HERDR MUTATION: close disposable workspace %s\n' "$WS_ID"
    HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" workspace close "$WS_ID" >/dev/null 2>&1 || true
  fi
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  printf 'HERDR MUTATION: stop/delete disposable session %s\n' "$SESSION"
  "$HERDR_BIN" session stop "$SESSION" >/dev/null 2>&1 || true
  "$HERDR_BIN" session delete "$SESSION" >/dev/null 2>&1 || true
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [ "$TARGET_LINK_CREATED" = 1 ] && [ -L "$ROOT/target" ]; then
    rm "$ROOT/target"
  fi
  rm -rf "$TMP"
  rm -f "$STATE"
  if "$HERDR_BIN" session list --json | jq -e --arg s "$SESSION" '.sessions[] | select(.name==$s)' >/dev/null; then
    echo "real-pi-smoke: cleanup failed; session remains: $SESSION" >&2
    return 1
  fi
  echo "real-pi-smoke: cleanup verified ($SESSION and $TMP absent)"
  return "$rc"
}
trap cleanup EXIT

# The plugin manifest expects ./target; create the isolated-target symlink before
# recording git status so this harness artifact is present in both comparisons.
if [ ! -e "$ROOT/target" ]; then
  ln -s "$TARGET" "$ROOT/target"
  TARGET_LINK_CREATED=1
fi
BASE_STATUS="$(git -C "$ROOT" status --short)"
SETTINGS_HASH_BEFORE="$(shasum -a 256 "$SETTINGS" | awk '{print $1}')"
printf '%s\n' "$BASE_STATUS" >"$EVIDENCE/git-status-before.txt"
printf '%s\n' "$PI_VERSION" >"$EVIDENCE/pi-version.txt"
printf '%s\n' "$DEFAULT_MODEL" >"$EVIDENCE/detected-model.txt"
printf '%s\n' "$DEFAULT_THINKING" >"$EVIDENCE/persisted-thinking.txt"
printf '%s\n' "$MODEL_ROW" >"$EVIDENCE/model-row.txt"
printf '%s\n' "$INTEGRATION" >"$EVIDENCE/integration.txt"

printf 'real-pi-smoke: pi=%s model=%s persisted-thinking=%s invocation-thinking=low\n' \
  "$PI_VERSION" "$DEFAULT_MODEL" "$DEFAULT_THINKING"

CARGO_TARGET_DIR="$TARGET" "$HOME/.cargo/bin/cargo" build \
  --manifest-path "$ROOT/Cargo.toml" --release -p board-cli
BOARD_BIN="$TARGET/release/board"
[ -x "$BOARD_BIN" ] || { echo "real-pi-smoke: candidate board missing" >&2; exit 1; }
cat >"$CONFIG" <<'EOF'
[daemon]
spawner = "herdr"
tick_ms = 200
EOF

cat >"$STATE" <<EOF
SESSION=$(printf %q "$SESSION")
TMP=$(printf %q "$TMP")
TARGET=$(printf %q "$TARGET")
EVIDENCE=$(printf %q "$EVIDENCE")
STATE=$(printf %q "$STATE")
WORKSPACE_DIR=$(printf %q "$WORKSPACE_DIR")
POEM=$(printf %q "$POEM")
BOARD_BIN=$(printf %q "$BOARD_BIN")
BOARD_DB=$(printf %q "$DB")
BOARD_SOCKET=$(printf %q "$SOCKET")
HERDR_BOARD_CONFIG=$(printf %q "$CONFIG")
DEFAULT_MODEL=$(printf %q "$DEFAULT_MODEL")
EOF

printf 'HERDR MUTATION: boot disposable real-Pi session %s\n' "$SESSION"
env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID \
  -u HERDR_SOCKET_PATH \
  BOARD_DB="$DB" BOARD_SOCKET="$SOCKET" HERDR_BOARD_CONFIG="$CONFIG" \
  PI_CODING_AGENT_SESSION_DIR="$PI_SESSIONS" \
  PATH="$(dirname "$BOARD_BIN"):$PATH" \
  "$HERDR_BIN" --session "$SESSION" server >"$TMP/herdr-server.log" 2>&1 &
SERVER_PID=$!
printf 'SERVER_PID=%q\n' "$SERVER_PID" >>"$STATE"
for _ in $(seq 1 75); do
  SOCK="$("$HERDR_BIN" session list --json 2>/dev/null | jq -r --arg s "$SESSION" '.sessions[] | select(.name==$s and .running==true) | .socket_path' | head -1)"
  [ -n "$SOCK" ] && [ -S "$SOCK" ] && break
  sleep .2
done
[ -S "$SOCK" ] || { echo "real-pi-smoke: session failed to boot" >&2; exit 1; }
printf 'HERDR_SOCKET_PATH=%q\n' "$SOCK" >>"$STATE"

printf 'HERDR MUTATION: link candidate plugin only in %s\n' "$SESSION"
"$HERDR_BIN" --session "$SESSION" plugin link "$ROOT" >/dev/null
printf 'HERDR MUTATION: create empty disposable workspace\n'
ws_json="$(HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" workspace create \
  --cwd "$WORKSPACE_DIR" --label real-pi-smoke --no-focus \
  --env "BOARD_DB=$DB" --env "BOARD_SOCKET=$SOCKET" \
  --env "HERDR_BOARD_CONFIG=$CONFIG")"
WS_ID="$(printf '%s' "$ws_json" | jq -er '.result.workspace.workspace_id')"
printf 'WS_ID=%q\n' "$WS_ID" >>"$STATE"

export BOARD_DB="$DB" BOARD_SOCKET="$SOCKET" HERDR_BOARD_CONFIG="$CONFIG"
export HERDR_SOCKET_PATH="$SOCK" BOARD_SPAWNER=herdr
"$BOARD_BIN" daemon --foreground >"$TMP/daemon.log" 2>&1 &
DAEMON_PID=$!
printf 'DAEMON_PID=%q\n' "$DAEMON_PID" >>"$STATE"
for _ in $(seq 1 50); do "$BOARD_BIN" status >/dev/null 2>&1 && break; sleep .2; done
"$BOARD_BIN" status >/dev/null

EXEC_ID="$(python3 "$ROOT/scripts/board-rpc.py" column.create \
  '{"name":"Execute","trigger":"auto","system_prompt":"Execute somente a tarefa do card no diretório descartável e cumpra o protocolo do board."}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["id"])')"
DESCRIPTION="Crie o arquivo $POEM com um poema original em português.
O arquivo deve ter exatamente quatro linhas não vazias e conter a palavra \"lua\"
(case-insensitive). Não altere nenhum arquivo do repositório. Depois valide você
mesmo que o arquivo existe e atende às regras. Comente no card o caminho e o
resultado da validação antes de finalizar a atividade."
card_json="$("$BOARD_BIN" card new --title "Poema temporário Pi" -d "$DESCRIPTION" \
  --column "$EXEC_ID" --harness pi --model "$DEFAULT_MODEL" --effort low \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jq -er '.id')"
printf 'CARD_ID=%q\n' "$CARD_ID" >>"$STATE"
printf '%s\n' "$card_json" >"$EVIDENCE/card-created.json"

echo "real-pi-smoke: card=$CARD_ID session=$SESSION workspace=$WS_ID state=$STATE"
OBSERVED_WORKING=0
outcome=""
for _ in $(seq 1 600); do
  show="$("$BOARD_BIN" card show "$CARD_ID" --json 2>/dev/null || true)"
  if [ -n "$show" ]; then
    outcome="$(printf '%s' "$show" | jq -r '.runs[-1].outcome // empty')"
    status="$(printf '%s' "$show" | jq -r '.card.status // empty')"
    printf '%s %s\n' "$status" "$outcome" >>"$EVIDENCE/status-samples.txt"
  fi
  snap="$(HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" api snapshot 2>/dev/null || true)"
  if printf '%s' "$snap" | jq -e '.result.snapshot.panes[]? | select(.agent=="pi" and .agent_status=="working")' >/dev/null 2>&1; then
    OBSERVED_WORKING=1
  fi
  [ -n "$outcome" ] && break
  sleep .5
done
[ "$outcome" = "ok" ] || { echo "real-pi-smoke: outcome=$outcome" >&2; exit 1; }
"$BOARD_BIN" card show "$CARD_ID" --json >"$EVIDENCE/card-final.json"
HERDR_SOCKET_PATH="$SOCK" "$HERDR_BIN" api snapshot >"$EVIDENCE/herdr-snapshot.json"
cp "$TMP/daemon.log" "$EVIDENCE/daemon.log"
cp "$TMP/herdr-server.log" "$EVIDENCE/herdr-server.log"

python3 - "$EVIDENCE/card-final.json" "$DEFAULT_MODEL" "$POEM" <<'PY'
import json, pathlib, re, sys
card_path, model, poem_path = sys.argv[1:]
x = json.load(open(card_path, encoding="utf-8"))
card, run, comments = x["card"], x["runs"][-1], x["comments"]
assert card["harness"] == "pi", card
assert card["model"] == model, card
assert card["effort"] == "low", card
assert run["harness"] == "pi", run
assert run["outcome"] == "ok", run
argv = json.loads(run["argv_json"])
assert argv[argv.index("--model") + 1] == model, argv
assert argv[argv.index("--thinking") + 1] == "low", argv
assert any(c["author"] == f"agent:{run['id']}" and poem_path in c["body"] for c in comments), comments
poem = pathlib.Path(poem_path)
assert poem.is_file(), poem
lines = poem.read_text(encoding="utf-8").splitlines()
assert len(lines) == 4, lines
assert all(line.strip() for line in lines), lines
assert re.search(r"lua", "\n".join(lines), re.I), lines
PY

FINAL_STATUS="$(git -C "$ROOT" status --short)"
[ "$FINAL_STATUS" = "$BASE_STATUS" ] || {
  diff -u "$EVIDENCE/git-status-before.txt" <(printf '%s\n' "$FINAL_STATUS") >&2 || true
  echo "real-pi-smoke: repository changed" >&2
  exit 1
}
SETTINGS_HASH_AFTER="$(shasum -a 256 "$SETTINGS" | awk '{print $1}')"
[ "$SETTINGS_HASH_AFTER" = "$SETTINGS_HASH_BEFORE" ] \
  || { echo "real-pi-smoke: Pi settings changed" >&2; exit 1; }
cat >"$EVIDENCE/result.txt" <<EOF
PASS
pi_version=$PI_VERSION
default_model=$DEFAULT_MODEL
persisted_default_thinking=$DEFAULT_THINKING
invocation_thinking=low
card_id=$CARD_ID
observed_working=$OBSERVED_WORKING
poem_validated=$POEM
repo_status_unchanged=yes
pi_settings_unchanged=yes
EOF
cat "$EVIDENCE/result.txt"
