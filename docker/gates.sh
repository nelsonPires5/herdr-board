#!/usr/bin/env bash
# The herdr-board sandbox gate runner. Executes INSIDE an isolated container
# (network disabled, non-root, read-only repo mount) and runs every maintained
# gate: the safety self-check, fmt, clippy, workspace tests, python tests, the
# static e2e harness gate, and all provider-free live Herdr e2e scenarios with
# --require-all. The first failing gate is named on stderr and the exit code
# is non-zero.
set -euo pipefail
cd /repo

export CARGO_NET_OFFLINE=true

gate() { # gate <name> <cmd...>
  local name="$1"; shift
  echo
  echo "==================================================================="
  echo "sandbox gates: [$name]"
  echo "==================================================================="
  if "$@"; then
    echo "sandbox gates: [$name] PASS"
  else
    local rc=$?
    echo "sandbox gates: [$name] FAIL (exit $rc)" >&2
    echo "sandbox gates: stopping at the first failing gate: $name" >&2
    exit "$rc"
  fi
}

# Gate 0: prove the isolation profile before anything else runs.
export HB_SELFCHECK_NETWORK=off
gate "selfcheck (isolation proof)" bash /repo/docker/selfcheck.sh

gate "cargo fmt --all --check" cargo fmt --all --check
gate "cargo clippy --workspace --all-targets --all-features -- -D warnings" \
  cargo clippy --workspace --all-targets --all-features -- -D warnings
gate "cargo test --workspace --all-features" cargo test --workspace --all-features
gate "python tests (scripts/tests)" \
  python3 -m unittest discover -s scripts/tests -p 'test_*.py'
gate "e2e static harness gate" bash /repo/e2e/test-harness.sh

# Live e2e: all provider-free scenarios against the pinned in-image Herdr.
# run-all.sh rebuilds board into the target volume (E2E_FORCE_BUILD=1) and
# enforces its own identity/cleanup guards exactly as on the host or in CI.
mkdir -p /artifacts
export E2E_FORCE_BUILD=1
export HERDR_BIN_PATH=/usr/local/bin/herdr
export BOARD_BIN="$CARGO_TARGET_DIR/release/board"
e2e_rc=0
bash /repo/e2e/run-all.sh --require-all "$@" 2>&1 | tee /artifacts/run-all.log \
  || e2e_rc=${PIPESTATUS[0]}

# Export the suite's artifact root (validated like e2e/ci.sh does) so the
# wrapper's `artifacts` subcommand can retrieve it later. The awk match is
# deliberately loose (mawk has no interval expressions); the python check
# below enforces the exact hb-e2e-run.<6 alnum> shape and ownership marker.
mapfile -t artifact_roots < <(
  awk '/^  artifacts: \/tmp\/hb-e2e-run\./ { print $2 }' \
    /artifacts/run-all.log || true
)
if [ "${#artifact_roots[@]}" -eq 1 ]; then
  root="${artifact_roots[0]}"
  if python3 - "$root" <<'PY'
import stat
import sys
from pathlib import Path

root = Path(sys.argv[1])
st = root.lstat()
valid = (
    stat.S_ISDIR(st.st_mode)
    and not root.is_symlink()
    and stat.S_IMODE(st.st_mode) == 0o700
    and root.parent.resolve() == Path("/tmp")
    and root.name.startswith("hb-e2e-run.")
    and len(root.name.removeprefix("hb-e2e-run.")) == 6
)
marker = root / ".owned-artifacts"
valid = valid and marker.is_file() and not marker.is_symlink()
valid = valid and marker.read_text(encoding="utf-8").splitlines()[0] == "herdr-board-e2e-artifacts"
raise SystemExit(0 if valid else 1)
PY
  then
    dest="/artifacts/e2e-$(date -u +%Y%m%dT%H%M%SZ)"
    mkdir -p "$dest"
    cp -a "$root"/. "$dest/"
    echo "sandbox gates: e2e artifacts exported to $dest"
  else
    echo "sandbox gates: refusing invalid suite artifact root: $root" >&2
  fi
else
  echo "sandbox gates: no valid suite artifact root found in run-all.log" >&2
fi

if [ "$e2e_rc" -ne 0 ]; then
  echo "sandbox gates: [live-e2e run-all --require-all] FAIL (exit $e2e_rc)" >&2
  exit "$e2e_rc"
fi
echo "sandbox gates: [live-e2e run-all --require-all] PASS"

echo
echo "sandbox gates: ALL GATES PASSED"
