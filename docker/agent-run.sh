#!/usr/bin/env bash
# Real-provider agent runner, executed INSIDE the sandbox agent container
# against the container-local board daemon and Herdr server.
#
# Usage: agent-run.sh <one-shot|seed> [provider] [model] [effort]
#   one-shot <provider> [model] [effort] — one bounded real dispatch for the
#     harness; the card lands on an auto "Running" column and must finish
#     with board done --outcome ok (column timeout 15 min; 20 min watchdog).
#   seed — create one card per supported harness (pi / codex / antigravity)
#     in the manual "Todo" column, ready to drag into "Running".
#
# Evidence (never credentials, never raw env) is written under /artifacts.
set -euo pipefail

export PATH="/home/board/.npm-global/bin:/home/board/.local/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"
cd /repo

mode="${1:?usage: agent-run.sh <one-shot|seed> [provider] [model] [effort]}"; shift

BOARD_BIN="$(command -v board)" || { echo "agent-run: board CLI missing (run scripts/sandbox.sh prepare first)" >&2; exit 2; }
HERDR_BIN="$(command -v herdr)"
HERDR_SOCK="/home/board/.config/herdr/herdr.sock"
WORKLOAD_DIR="/home/board/work"     # writable cwd for new-workspace spaces
RUNNING_COL="Running"
TODO_COL="Todo"

fail() { echo "agent-run: FAIL: $*" >&2; exit 2; }

board_id() { "$BOARD_BIN" board list --json 2>/dev/null | jq -r '.[0].id // empty'; }

ensure_board() { # a board must exist (fresh agent-state volume bootstrap); prints its id
  local bid
  bid="$(board_id)"
  if [ -z "$bid" ]; then
    bid="$("$BOARD_BIN" board create Sandbox --json 2>/dev/null | jq -r '.id // empty')"
  fi
  [ -n "$bid" ] || fail "no board available (board create failed)"
  printf '%s' "$bid"
}

prune_stale_runs() { # end any open run left by a previous invocation; a teardown
  # mid-run can strand an active/awaiting run that otherwise parks the queue.
  # Each agent container is dedicated and serialized, so there is never a live
  # run this must NOT touch: prune BEFORE the new card is created.
  local bid cids cid
  bid="$(board_id)" || true
  [ -n "$bid" ] || return 0
  cids="$("$BOARD_BIN" card list --board "$bid" --json 2>/dev/null | jq -r '.[] | select(.status=="queued" or .status=="running" or .status=="awaiting") | .id')"
  [ -n "$cids" ] || return 0
  while read -r cid; do
    [ -n "$cid" ] || continue
    "$BOARD_BIN" cancel "$cid" >/dev/null 2>&1 || true
    echo "agent-run: pruned stale run on card $cid"
  done <<< "$cids"
}

ensure_column() { # ensure_column <name> <trigger> -> prints column id; fails closed
  # if a same-named column already exists with a DIFFERENT trigger, so a stale
  # volume can never silently auto-dispatch seeded cards into an auto 'Todo'
  # (paid agents) or park one-shots in a manual 'Running'.
  local name="$1" trigger="$2" bid row cid actual
  bid="$(board_id)" || true
  row="$("$BOARD_BIN" column list --board "$bid" --json 2>/dev/null | jq -c --arg n "$name" '.[] | select(.name==$n)' | head -1)"
  if [ -n "$row" ]; then
    if [ "$(printf '%s' "$row" | jq -r '.trigger')" != "$trigger" ]; then
      actual="$(printf '%s' "$row" | jq -r '.trigger')"
      fail "column '$name' already exists with trigger '$actual' (need '$trigger'); refusing to use a mismatched column"
    fi
    printf '%s' "$(printf '%s' "$row" | jq -r '.id')"
    return 0
  fi
  cid="$("$BOARD_BIN" column create --board "$bid" --name "$name" --trigger "$trigger" --timeout 15 --json 2>/dev/null | jq -r '.id')"
  [ -n "$cid" ] || fail "could not resolve/create column '$name'"
  printf '%s' "$cid"
}

wait_daemon() {
  local i
  for i in $(seq 1 60); do
    "$BOARD_BIN" daemon status >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  fail "board daemon did not become ready"
}

TASK_ONE_SHOT='CODING AGENT TASK: USE YOUR TERMINAL/BASH TOOL to EXECUTE (do not describe) the shell command:
  board done --outcome ok
Run it with a tool that executes shell, then report the tool output. Give no text-only reply before running it. Do not create or modify any files.'
TASK_SEED='CODING AGENT TASK: USE YOUR TERMINAL/BASH TOOL to EXECUTE (do not describe) the shell command:
  board done --outcome ok
Run it with a tool that executes shell, then report the tool output. Do not create or modify any files.'

default_model() {
  case "$1" in
    pi) echo "opencode-go/deepseek-v4-flash" ;;
    codex) echo "gpt-5.6-luna" ;;
    antigravity) echo "gemini-3.7-flash" ;;
    *) fail "unsupported provider '$1' (pi|codex|antigravity)" ;;
  esac
}

one_shot() {
  local provider="${1:?provider}" model="${2:-$(default_model "$1")}" effort="${3:-low}"
  local bid running_id card_json card_id evidence outcome show poll perm
  # Approval preset per harness so a headless run never blocks on a prompt:
  # codex defaults to interactive approval (null -> blocked until the column
  # timeout); antigravity's safe preset is "sandbox". pi needs none.
  case "$provider" in
    codex) perm="approve-for-me" ;;
    antigravity) perm="sandbox" ;;
    *) perm="" ;;
  esac
  wait_daemon
  prune_stale_runs
  bid="$(ensure_board)"
  mkdir -p "$WORKLOAD_DIR"
  running_id="$(ensure_column "$RUNNING_COL" auto)"
  evidence="/artifacts/agent-${provider}-$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "$evidence"

  echo "agent-run: dispatching one real '$provider' run (model '$model', effort '$effort') -> $RUNNING_COL"
  card_json="$("$BOARD_BIN" card create --board "$bid" --title "sandbox $provider agent check" \
    --description "$TASK_ONE_SHOT" --column "$running_id" \
    --harness "$provider" ${model:+--model "$model"} ${effort:+--effort "$effort"} ${perm:+--permission "$perm"} \
    --space-kind new-workspace --space-ref "sb-${provider}" --space-cwd "$WORKLOAD_DIR" --json 2>/dev/null || true)"
  card_id="$(printf '%s' "$card_json" | jq -r '.id // empty')"
  [ -n "$card_id" ] || fail "card create failed: $(printf '%s' "$card_json")"
  printf '%s\n' "$card_json" > "$evidence/card-created.json"

  echo "agent-run: card=$card_id; polling (column timeout 15m, watchdog 20m)"
  outcome=""
  : > "$evidence/status-samples.jsonl"
  for poll in $(seq 1 2400); do
    show="$("$BOARD_BIN" card show "$card_id" --json 2>/dev/null || true)"
    if printf '%s' "$show" | jq -e '.card.id != null' >/dev/null 2>&1; then
      printf '%s' "$show" | jq -c --argjson poll "$poll" \
        '{poll:$poll,status:.card.status,run_count:(.runs|length),outcome:(.runs[-1].outcome // null)}' \
        >> "$evidence/status-samples.jsonl" 2>/dev/null || true
      outcome="$(printf '%s' "$show" | jq -r '.runs[-1].outcome // empty' 2>/dev/null || true)"
    fi
    HERDR_SOCKET_PATH="$HERDR_SOCK" "$HERDR_BIN" api snapshot > "$evidence/herdr-snapshot.json" 2>/dev/null || true
    [ -z "$outcome" ] || break
    sleep 0.5
  done

  "$BOARD_BIN" card show "$card_id" --json > "$evidence/card-final.json" 2>/dev/null || true
  HERDR_SOCKET_PATH="$HERDR_SOCK" "$HERDR_BIN" api snapshot > "$evidence/herdr-snapshot.json" 2>/dev/null || true
  { printf 'provider=%s\n' "$provider"
    printf 'model=%s\n' "$model"
    printf 'effort=%s\n' "$effort"
    printf 'outcome=%s\n' "${outcome:-timeout}"
  } > "$evidence/summary.txt"
  echo "agent-run: provider=$provider outcome=${outcome:-timeout}; evidence=$evidence"
  [ "$outcome" = "ok" ] && return 0
  if [ -z "$outcome" ]; then
    fail "no outcome within the watchdog (20 min); check $evidence"
  else
    fail "agent finished with outcome '$outcome'; check $evidence"
  fi
}

seed() {
  local bid running_id todo_id label provider model effort perm card
  wait_daemon
  prune_stale_runs
  bid="$(ensure_board)"
  mkdir -p "$WORKLOAD_DIR"
  running_id="$(ensure_column "$RUNNING_COL" auto)"
  todo_id="$(ensure_column "$TODO_COL" manual)"
  echo "agent-run: seeding cards into '$TODO_COL' (drag them to '$RUNNING_COL' to dispatch)"
  while IFS=: read -r label provider model effort; do
    printf '%s\n' "$label" | grep -qE '^(pi|codex|antigravity)$' || continue
    case "$label" in
      codex) perm="approve-for-me" ;;
      antigravity) perm="sandbox" ;;
      *) perm="" ;;
    esac
    card="$("$BOARD_BIN" card create --board "$bid" --title "sandbox check ($label)" \
      --description "$TASK_SEED" --column "$todo_id" \
      --harness "$provider" --model "$model" --effort "$effort" ${perm:+--permission "$perm"} \
      --space-kind new-workspace --space-ref "sb-${label}" --space-cwd "$WORKLOAD_DIR" --json 2>/dev/null || true)"
    cid="$(printf '%s' "$card" | jq -r '.id // empty')"
    [ -n "$cid" ] || { echo "agent-run: seed failed for $label: $(printf '%s' "$card")" >&2; continue; }
    echo "agent-run: seeded $label -> card $cid (harness=$provider model=$model effort=$effort) in '$TODO_COL'"
  done <<'SPECS'
pi:pi:opencode-go/deepseek-v4-flash:low
codex:codex:gpt-5.6-luna:low
antigravity:antigravity:gemini-3.7-flash:low
SPECS
}

case "$mode" in
  one-shot) one_shot "$@" ;;
  seed) seed ;;
  *) fail "unknown mode '$mode' (one-shot|seed)" ;;
esac
