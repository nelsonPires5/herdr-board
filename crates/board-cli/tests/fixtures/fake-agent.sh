#!/usr/bin/env bash
# Fake harness used by the daemon integration tests. Receives the board env
# (BOARD_PROMPT / BOARD_CARD_ID / BOARD_RUN_ID / BOARD_SOCKET) and the built
# `board` binary via BOARD_BIN. Sleeps, then comments + closes the run through
# the real CLI (so the CLI request path is exercised too).
set -euo pipefail

: "${BOARD_PROMPT:=}"
: "${BOARD_CARD_ID:?BOARD_CARD_ID required}"
: "${BOARD_SOCKET:?BOARD_SOCKET required}"
: "${BOARD_BIN:?BOARD_BIN required}"

sleep "${FAKE_AGENT_SLEEP:-1}"

# Simulate an agent that crashes / exits without ever calling `board done`.
if [ "${FAKE_AGENT_SILENT:-0}" = "1" ]; then
  exit 0
fi

"$BOARD_BIN" comment "fake: done work"
"$BOARD_BIN" done --outcome "${FAKE_AGENT_OUTCOME:-ok}"
