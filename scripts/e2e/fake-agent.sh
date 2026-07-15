#!/usr/bin/env bash
# fake-agent.sh — the fake harness the live e2e scenarios dispatch instead of a
# real coding agent. It receives the board env the daemon injects at agent.start
# (BOARD_PROMPT / BOARD_CARD_ID / BOARD_RUN_ID / BOARD_SOCKET) and the built
# `board` binary via BOARD_BIN (passed through the config argv env-wrapper, since
# herdr panes do NOT inherit workspace-level env). It reports back through the
# real CLI so the request path is exercised too.
#
# Mirrors crates/board-cli/tests/fixtures/fake-agent.sh (the crate integration
# tests' fixture) and adds FAKE_AGENT_HOLD so a scenario can keep the pane open
# after the run finishes (a herdr pane closes when its process exits, so grid /
# layout assertions need the process to stay alive).
#
# Knobs (env):
#   FAKE_AGENT_SLEEP   seconds to sleep BEFORE reporting (default 1.5). A run's
#                      started_at commits just after agent.start; an instant
#                      `board done` races it and gets "no active run", so never
#                      drop this below ~1.5s.
#   FAKE_AGENT_OUTCOME ok|fail passed to `board done` (default ok).
#   FAKE_AGENT_COMMENT the comment body (default "fake: done work").
#   FAKE_AGENT_SILENT  1 = exit without ever commenting or calling `board done`
#                      (simulates a crashed agent).
#   FAKE_AGENT_HOLD    seconds to sleep AFTER `board done` (default 0). Set to a
#                      large value (e.g. 300) to keep the pane alive for layout
#                      assertions; scenario cleanup closes the workspace.
set -euo pipefail

: "${BOARD_PROMPT:=}"
: "${BOARD_CARD_ID:?BOARD_CARD_ID required}"
: "${BOARD_SOCKET:?BOARD_SOCKET required}"
: "${BOARD_BIN:?BOARD_BIN required}"

sleep "${FAKE_AGENT_SLEEP:-1.5}"

# Simulate an agent that crashes / exits without ever calling `board done`.
if [ "${FAKE_AGENT_SILENT:-0}" = "1" ]; then
  exit 0
fi

"$BOARD_BIN" comment "${FAKE_AGENT_COMMENT:-fake: done work}"
"$BOARD_BIN" done --outcome "${FAKE_AGENT_OUTCOME:-ok}"

# Keep the pane's process alive so scenarios can inspect the live layout. The run
# has already finished via the `board done` channel above.
hold="${FAKE_AGENT_HOLD:-0}"
if [ "$hold" != "0" ]; then
  sleep "$hold"
fi
