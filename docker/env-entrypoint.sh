#!/usr/bin/env bash
# Entrypoint of the persistent sandbox environment container. Starts a
# container-local Herdr server for the default session; the board daemon
# auto-starts on the first board CLI/TUI use (existing autostart behavior).
# All state lands in named volumes: HOME=/home/board (state volume).
set -euo pipefail

echo "sandbox env: starting herdr server (container-local default session)"
exec herdr server
