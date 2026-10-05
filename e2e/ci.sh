#!/usr/bin/env bash
# Provider-free CI entrypoint: install the pinned Herdr and export live-suite evidence.
set -euo pipefail
umask 077

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
EXPORT_DIR="$REPO_ROOT/e2e-artifacts"
rm -rf "$EXPORT_DIR"
mkdir -m 700 "$EXPORT_DIR"
exec > >(tee "$EXPORT_DIR/runner.log") 2>&1

HERDR_VERSION=0.9.0
HERDR_PROTOCOL=22
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    # Git Bash on Windows: the release ships a zip; its SHA pins the archive,
    # a marker written after a verified extract pins the extracted tree
    # (conpty/ ships beside herdr.exe), and herdr.exe is re-hashed every run.
    HERDR_PLATFORM=windows-x86_64
    HERDR_URL=https://github.com/herdrdev/herdr/releases/download/v0.9.0/herdr-windows-x86_64.zip
    HERDR_SHA256=b4508c445de1c1a68c760a01735da2aba2fa214b2aafd4b07f732e49b2a64b11
    HERDR_EXE_SHA256=9b3bf49f94c2d09b1d62e11171b132865768dafc36b1327fb946c0cef9ca0d00
    HERDR_EXE=herdr.exe
    # The Windows live suite runs this subset for now; the rest of the
    # catalog is a documented follow-up (docs/testing.md).
    SUITE_SCENARIOS=(01-core 04-fail-on-fail 06-silent-exit 17-configured-p17-runner 19-daemon-before-herdr)
    # Windows Python installs ship `python`; `python3` may be absent or a stub.
    python3 -c '' >/dev/null 2>&1 || python3() { python "$@"; }
    ;;
  *)
    HERDR_PLATFORM=linux-x86_64
    HERDR_URL=https://github.com/herdrdev/herdr/releases/download/v0.9.0/herdr-linux-x86_64
    HERDR_SHA256=4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f
    HERDR_EXE_SHA256=$HERDR_SHA256
    HERDR_EXE=herdr
    SUITE_SCENARIOS=()
    ;;
esac
CACHE_DIR="${HERDR_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/herdr-board/herdr-$HERDR_VERSION-$HERDR_PLATFORM}"
HERDR_BIN="$CACHE_DIR/$HERDR_EXE"
HERDR_ZIP_MARKER="$CACHE_DIR/.verified-zip.sha256"
mkdir -p "$CACHE_DIR"
chmod 700 "$CACHE_DIR"

sha_matches() {
  [ -f "$1" ] && [ ! -L "$1" ] || return 1
  if [ "$HERDR_EXE" = herdr.exe ]; then
    [ -f "$HERDR_ZIP_MARKER" ] && [ "$(cat "$HERDR_ZIP_MARKER")" = "$HERDR_SHA256" ] || return 1
  fi
  printf '%s  %s\n' "$HERDR_EXE_SHA256" "$1" | sha256sum --check --status
}

if [ ! -x "$HERDR_BIN" ] || ! sha_matches "$HERDR_BIN"; then
  tmp="$(mktemp "$CACHE_DIR/.herdr.XXXXXX")"
  trap 'rm -f "${tmp:-}"' EXIT
  echo "Downloading Herdr $HERDR_VERSION from pinned release asset"
  curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 120 --retry 3 --retry-all-errors \
    --output "$tmp" "$HERDR_URL"
  printf '%s  %s\n' "$HERDR_SHA256" "$tmp" | sha256sum --check
  if [ "$HERDR_EXE" = herdr.exe ]; then
    rm -f "$HERDR_ZIP_MARKER" "$HERDR_BIN"
    python3 -m zipfile -e "$(cygpath -w "$tmp")" "$(cygpath -w "$CACHE_DIR")"
    [ -f "$HERDR_BIN" ] || { echo "herdr.exe missing from the pinned zip" >&2; exit 1; }
    printf '%s' "$HERDR_SHA256" >"$HERDR_ZIP_MARKER"
    rm -f "$tmp"
  else
    chmod 755 "$tmp"
    mv -f "$tmp" "$HERDR_BIN"
  fi
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
  'import json,sys; p=json.load(sys.stdin).get("protocol"); expected=int(sys.argv[1]); print(f"Herdr socket protocol: {p}"); raise SystemExit(p != expected)' "$HERDR_PROTOCOL"
echo "Pinned Herdr SHA-256: $HERDR_SHA256"

export HERDR_BIN_PATH="$HERDR_BIN"
set +e
E2E_FORCE_BUILD=1 "$REPO_ROOT/e2e/run-all.sh" --require-all "${SUITE_SCENARIOS[@]}" 2>&1 | tee "$EXPORT_DIR/suite.log"
suite_status=${PIPESTATUS[0]}
set -e
printf '%s\n' "$suite_status" >"$EXPORT_DIR/suite.status"

mapfile -t artifact_roots < <(
  awk '/^  artifacts: (\/tmp|[A-Za-z]:\/[^ ]*)\/hb-e2e-run\.[[:alnum:]]{6}$/ { print $2 }' \
    "$EXPORT_DIR/suite.log"
)
export_status=0
if [ "${#artifact_roots[@]}" -eq 1 ]; then
  artifact_root="${artifact_roots[0]}"
  # Native Windows Python sees Git Bash's /tmp only through its Windows path.
  native_root="$artifact_root"
  native_tmp=/tmp
  if [ "$HERDR_EXE" = herdr.exe ]; then
    native_root="$(cygpath -w "$artifact_root")"
    native_tmp="$(cygpath -w /tmp)"
  fi
  if python3 - "$native_root" "$native_tmp" <<'PY'
import os
import stat
import sys
from pathlib import Path

root = Path(sys.argv[1])
st = root.lstat()
valid = (
    stat.S_ISDIR(st.st_mode)
    and not root.is_symlink()
    # NTFS has no POSIX mode bits; the runner profile ACL is owner-only.
    and (os.name == "nt" or stat.S_IMODE(st.st_mode) == 0o700)
    and root.parent.resolve() == Path(sys.argv[2]).resolve()
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
