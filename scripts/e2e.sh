#!/usr/bin/env bash
# e2e.sh — compat wrapper. The live end-to-end test is now a scenario suite under
# scripts/e2e/ (see docs/testing.md). This forwards to the runner so the historic
# entrypoint (referenced by README/CONTRIBUTING) stays stable.
#
#   scripts/e2e.sh                 -> run every scenario (build once, summary)
#   scripts/e2e/run-all.sh         -> same
#   bash scripts/e2e/01-core.sh    -> a single scenario
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
exec "$script_dir/e2e/run-all.sh" "$@"
