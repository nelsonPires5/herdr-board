#!/usr/bin/env bash
# Install the pinned real-provider CLIs into the sandbox AGENT state volume
# (the `${PREFIX}-agent-state` volume mounted at /home/board by the wrapper).
#
# pi  + codex : installed from npm at EXACT pinned versions (npm verifies the
#               registry integrity of the pinned release).
# agy         : downloaded from the pinned tarball in docker/agy-pin.txt with
#               the SHA-512 re-verified against the downloaded bytes; the
#               floating install.sh is never executed.
#
# Runs in a one-shot container WITH network (the explicit agent opt-in) but
# WITHOUT any credentials mounted: this step only fetches public artifacts
# and never sees host secrets. All state lands in the agent-state volume;
# nothing is baked into the image; evidence (pinned versions/checksums) is
# recorded under /artifacts.
set -euo pipefail
cd /repo

PINS=/repo/docker/agy-pin.txt
NPM_PREFIX=/home/board/.npm-global
LOCAL_BIN=/home/board/.local/bin
export PATH="$NPM_PREFIX/bin:$LOCAL_BIN:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"

# Detect the runtime architecture (matches the container platform).
case "$(uname -m)" in
  x86_64|amd64) binarch=amd64 ;;
  aarch64|arm64) binarch=arm64 ;;
  *) echo "agent-prepare: unsupported architecture: $(uname -m)" >&2; exit 2 ;;
esac

fail() { echo "agent-prepare: FAIL: $*" >&2; exit 1; }

# --- pi + codex (pinned npm releases) ---------------------------------------
echo "agent-prepare: installing pi@0.84.2 and codex@0.147.0 into $NPM_PREFIX"
mkdir -p "$NPM_PREFIX"
npm install -g --prefix "$NPM_PREFIX" \
  "@earendil-works/pi-coding-agent@0.84.2" \
  "@openai/codex@0.147.0" >/dev/null
command -v pi    >/dev/null 2>&1 || fail "pi CLI unavailable after npm install"
command -v codex >/dev/null 2>&1 || fail "codex CLI unavailable after npm install"
pi_ver="$(pi --version 2>/dev/null || true)"
codex_ver="$(codex --version 2>/dev/null || true)"
# pi prints the bare version; codex prints "codex-cli 0.147.0".
[ "$pi_ver" = "0.84.2" ] || fail "pi version mismatch: got '$pi_ver', want '0.84.2'"
case "$codex_ver" in *0.147.0*) ;; *) fail "codex version mismatch: got '$codex_ver', want '0.147.0'" ;; esac
echo "agent-prepare: pi $pi_ver, codex $codex_ver ok"

# --- codex writable config (checked-in minimal, trusted dirs, no host leak) --
# The container's CODEX_HOME needs a WRITABLE config.toml (codex persists the
# "trust this directory?" answer and hook hashes into it). The host's
# config.toml is macOS-specific (absolute host application paths, plugins,
# desktop/notify, mcp_servers) and must not be mirrored. This
# checked-in minimal file pre-trusts the container workspace dirs so a
# headless run never blocks; auth stays in the read-only credential mount, never here.
CODEX_HOME="${CODEX_HOME:-/home/board/.codex}"
mkdir -p "$CODEX_HOME"
cp -f /repo/docker/agent-codex-config.toml "$CODEX_HOME/config.toml"
chmod 600 "$CODEX_HOME/config.toml"
echo "agent-prepare: codex writable config.toml installed (trusted dirs, no host paths)"

# --- agy (pinned tarball + verified sha512) ---------------------------------
[ -f "$PINS" ] || fail "missing agy pin file: $PINS"
pin="$(grep -E "^${binarch}[[:space:]]" "$PINS" || true)"
[ -n "$pin" ] || fail "no agy pin for architecture '$binarch' in $PINS"
agy_version="$(printf '%s\n' "$pin" | awk -F'version=' '{print $2}' | awk '{print $1}')"
agy_url="$(printf '%s\n' "$pin" | awk -F'url=' '{print $2}' | awk '{print $1}')"
agy_sha512="$(printf '%s\n' "$pin" | awk -F'sha512=' '{print $2}' | awk '{print $1}')"
[ -n "$agy_url" ] && [ -n "$agy_sha512" ] && [ -n "$agy_version" ] \
  || fail "malformed agy pin line: $pin"

if [ ! -x "$LOCAL_BIN/agy" ] || ! agy --version 2>/dev/null | grep -q "$agy_version"; then
  echo "agent-prepare: downloading pinned agy $agy_version ($binarch)"
  curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 600 --retry 3 --retry-all-errors \
    --output /tmp/agy.tar.gz "$agy_url"
  printf '%s  %s\n' "$agy_sha512" /tmp/agy.tar.gz | sha512sum --check --status \
    || fail "agy tarball sha512 does not match the pin (got $(sha512sum /tmp/agy.tar.gz | awk '{print $1}'))"
  rm -rf /tmp/agy-extract && mkdir -p /tmp/agy-extract
  tar xzf /tmp/agy.tar.gz -C /tmp/agy-extract
  [ -f /tmp/agy-extract/antigravity ] || fail "pinned agy tarball has no 'antigravity' binary"
  mkdir -p "$LOCAL_BIN"
  cp -f /tmp/agy-extract/antigravity "$LOCAL_BIN/agy"
  chmod 755 "$LOCAL_BIN/agy"
  rm -rf /tmp/agy-extract /tmp/agy.tar.gz
fi
agy_ver="$(agy --version 2>/dev/null || true)"
case "$agy_ver" in *"$agy_version"*) ;; *) fail "agy version mismatch: got '$agy_ver', want '$agy_version'" ;; esac
echo "agent-prepare: agy $agy_ver ($binarch, sha512 pinned+verified)"

# --- evidence (pinned versions/checksums; never credentials) ----------------
evidence="/artifacts/agent-prepare-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$evidence"
{
  printf 'pi_version=%s\n' "$pi_ver"
  printf 'codex_version=%s\n' "$codex_ver"
  printf 'agy_arch=%s\n' "$binarch"
  printf 'agy_version=%s\n' "$agy_ver"
  printf 'agy_url=%s\n' "$agy_url"
  printf 'agy_sha512=%s\n' "$agy_sha512"
} > "$evidence/providers.txt"
echo "agent-prepare: evidence at $evidence (host: scripts/sandbox.sh artifacts)"
echo "agent-prepare: ok"
