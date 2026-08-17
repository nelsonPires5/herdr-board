#!/usr/bin/env bash
# Cargo.lock regeneration for the sandbox. The repo is mounted read-only, so
# the workspace is copied to a scratch dir, cargo updates the lockfile with
# network access, and only Cargo.lock is written back through a single-file
# read-write bind mount provided by the wrapper.
set -euo pipefail

work=/tmp/lockwork
rm -rf "$work"
mkdir -p "$work"
tar -C /repo --exclude=./.git --exclude=./target -cf - . | tar -C "$work" -xf -
cd "$work"

echo "lock: cargo fetch (updates Cargo.lock as needed)"
cargo fetch

cp /tmp/lockwork/Cargo.lock /out/Cargo.lock
echo "lock: Cargo.lock written back to the worktree"
