#!/usr/bin/env bash
# Build the `board` binary in release mode. Idempotent: cargo is a no-op when
# nothing changed. This is the plugin's [[build]] step (herdr runs it at install
# time) and is also called by scripts/install.sh.
#
# Output: target/release/board (relative to the repo root).
set -euo pipefail

# Resolve the repo root from this script's location so the build works no matter
# what cwd herdr / the caller invokes it from.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
cd "$repo_root"

# Prefer a user-local cargo if PATH doesn't already have one.
if ! command -v cargo >/dev/null 2>&1; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || {
  echo "build.sh: cargo not found (install the Rust toolchain: https://rustup.rs)" >&2
  exit 1
}

echo "build.sh: cargo build --release -p board-cli (repo: $repo_root)"
cargo build --release -p board-cli

bin="$repo_root/target/release/board"
if [ ! -x "$bin" ]; then
  echo "build.sh: expected binary not found at $bin" >&2
  exit 1
fi
echo "build.sh: ok -> $bin"
