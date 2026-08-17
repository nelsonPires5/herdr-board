#!/usr/bin/env bash
# Real-provider smoke runner for the sandbox. Runs in a container WITH network
# (explicit opt-in from scripts/sandbox.sh smoke --provider X --allow-network)
# and with exactly one provider credential directory mounted read-only at
# /secrets. Provider CLIs are Linux binaries installed at smoke time into the
# state volume (never into the image, never into logs).
#
# Usage: smoke.sh <provider>   (provider: claude | codex | opencode)
set -euo pipefail
provider="${1:?usage: smoke.sh <claude|codex|opencode>}"
cd /repo

npm_prefix=/home/board/.npm-global
export PATH="$npm_prefix/bin:$PATH"

case "$provider" in
  claude) npm_pkg=@anthropic-ai/claude-code; bin=claude; script=real-claude-haiku-smoke.sh ;;
  codex) npm_pkg=@openai/codex; bin=codex; script=real-codex-smoke.sh ;;
  opencode) npm_pkg=opencode-ai; bin=opencode; script=real-opencode-smoke.sh ;;
  *) echo "smoke.sh: unknown provider '$provider'" >&2; exit 2 ;;
esac

if ! command -v "$bin" >/dev/null 2>&1; then
  echo "smoke: installing $bin (npm $npm_pkg) into the state volume"
  mkdir -p "$npm_prefix"
  npm install --prefix "$npm_prefix" "$npm_pkg"
fi
command -v "$bin" >/dev/null 2>&1 || {
  echo "smoke: $bin CLI unavailable after install" >&2
  exit 2
}
echo "smoke: provider CLI: $bin $("$(command -v "$bin")" --version 2>&1 || echo unknown-version)"

evidence="/artifacts/smoke-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$evidence"
set +e
bash "/repo/e2e/$script"
rc=$?
set -e

# Best-effort evidence preservation (the scripts stage everything under
# container-local /tmp roots; copy them out before they vanish).
shopt -s nullglob
for d in /tmp/hb-claude.* /tmp/hb-codex.* /tmp/hb-opencode.*; do
  cp -a "$d" "$evidence/" 2>/dev/null || true
done
shopt -u nullglob
echo "smoke: evidence copied to $evidence (host: scripts/sandbox.sh artifacts)"
exit "$rc"
