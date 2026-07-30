#!/usr/bin/env bash
# Provider-free CI entrypoint: install the pinned Herdr and export live-suite evidence.
set -euo pipefail
umask 077

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
EXPORT_DIR="$REPO_ROOT/e2e-artifacts"
rm -rf "$EXPORT_DIR"
mkdir -m 700 "$EXPORT_DIR"
exec > >(tee "$EXPORT_DIR/runner.log") 2>&1

HERDR_VERSION=0.7.5
HERDR_PROTOCOL=17
HERDR_URL=https://github.com/herdrdev/herdr/releases/download/v0.7.5/herdr-linux-x86_64
HERDR_SHA256=3dc83288073e4c2d3c679a30e7be97bcca9141c6fd17dbbb9219142e95c59253
CACHE_DIR="${HERDR_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/herdr-board/herdr-$HERDR_VERSION-linux-x86_64}"
HERDR_BIN="$CACHE_DIR/herdr"
mkdir -p "$CACHE_DIR"
chmod 700 "$CACHE_DIR"

sha_matches() {
  [ -f "$1" ] && [ ! -L "$1" ] &&
    printf '%s  %s\n' "$HERDR_SHA256" "$1" | sha256sum --check --status
}

if [ ! -x "$HERDR_BIN" ] || ! sha_matches "$HERDR_BIN"; then
  tmp="$(mktemp "$CACHE_DIR/.herdr.XXXXXX")"
  trap 'rm -f "${tmp:-}"' EXIT
  echo "Downloading Herdr $HERDR_VERSION from pinned release asset"
  curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 120 --retry 3 --retry-all-errors \
    --output "$tmp" "$HERDR_URL"
  printf '%s  %s\n' "$HERDR_SHA256" "$tmp" | sha256sum --check
  chmod 755 "$tmp"
  mv -f "$tmp" "$HERDR_BIN"
  trap - EXIT
else
  echo "Using SHA-verified cached Herdr $HERDR_VERSION"
fi

sha_matches "$HERDR_BIN" || {
  echo "Pinned Herdr checksum verification failed" >&2
  exit 1
}
actual_version="$("$HERDR_BIN" --version)"
[ "$actual_version" = "herdr $HERDR_VERSION" ] || {
  echo "Expected 'herdr $HERDR_VERSION', got '$actual_version'" >&2
  exit 1
}
"$HERDR_BIN" api schema --json | python3 -c \
  'import json,sys; p=json.load(sys.stdin).get("protocol"); print(f"Herdr socket protocol: {p}"); raise SystemExit(p != 17)'
echo "Pinned Herdr SHA-256: $HERDR_SHA256"

export HERDR_BIN_PATH="$HERDR_BIN"
set +e
"$REPO_ROOT/e2e/run-all.sh" --require-all 2>&1 | tee "$EXPORT_DIR/suite.log"
suite_status=${PIPESTATUS[0]}
set -e
printf '%s\n' "$suite_status" >"$EXPORT_DIR/suite.status"

mapfile -t artifact_roots < <(
  awk '/^  artifacts: \/tmp\/hb-e2e-run\.[[:alnum:]]{6}$/ { print $2 }' \
    "$EXPORT_DIR/suite.log"
)
export_status=0
if [ "${#artifact_roots[@]}" -eq 1 ]; then
  artifact_root="${artifact_roots[0]}"
  if python3 - "$artifact_root" <<'PY'
import os
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
    mkdir -m 700 "$EXPORT_DIR/suite"
    cp -R "$artifact_root"/. "$EXPORT_DIR/suite/"
    printf '%s\n' "$artifact_root" >"$EXPORT_DIR/private-artifact-root.txt"
    echo "Exported exact invocation artifact root to $EXPORT_DIR/suite"
  else
    echo "Refusing invalid suite artifact root: $artifact_root" >&2
    export_status=1
  fi
elif [ "${#artifact_roots[@]}" -gt 1 ]; then
  echo "Refusing ambiguous suite artifact roots" >&2
  export_status=1
else
  echo "Suite did not emit an artifact root; runner log remains available" >&2
fi

[ "$suite_status" -ne 0 ] && exit "$suite_status"
exit "$export_status"
