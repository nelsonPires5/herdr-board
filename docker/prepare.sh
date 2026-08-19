#!/usr/bin/env bash
# Dependency preparation for the herdr-board sandbox. Runs in a container WITH
# network access (the only deterministic-mode exception: dependency fetch).
# Everything it writes goes to named volumes: the cargo registry cache, the
# build output, and a board binary inside the state volume.
set -euo pipefail
cd /repo

echo "prepare: cargo fetch --locked"
if ! cargo fetch --locked; then
  echo "prepare: cargo fetch --locked failed." >&2
  echo "  If Cargo.toml changed, the lockfile is stale. Regenerate it with:" >&2
  echo "    scripts/sandbox.sh lock" >&2
  exit 1
fi

echo "prepare: cargo build --release -p board-cli"
cargo build --release -p board-cli

mkdir -p /home/board/.local/bin
cp -f "$CARGO_TARGET_DIR/release/board" /home/board/.local/bin/board
echo "prepare: board installed at /home/board/.local/bin/board"
echo "prepare: ok (future gates runs are fully offline)"
