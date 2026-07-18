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

# Stop a running boardd before rebuilding so the reinstall replaces a stopped
# process rather than overwriting a binary the old daemon still has mapped in
# memory (which would leave a stale daemon serving the previous version). This
# is best-effort: a very old `board` without `--stop` errors here and we just
# continue — the README documents the manual stop for that one-time case.
if command -v board >/dev/null 2>&1; then
  board daemon --stop >/dev/null 2>&1 || true
fi

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
