#!/usr/bin/env bash
# Entrypoint of the sandbox AGENT container (real-provider dispatches).
#
# Runs with the network enabled and exactly the opted-in host credential
# directories mounted READ-ONLY under /secrets/<provider>. It wires each
# mounted provider's credential/configuration files into the writable
# container HOME (the agent-state volume) via read-only symlinks — never
# copied, never made writable — so each provider CLI can write its own
# session/cache state in the volume while reading its credentials from the
# ro /secrets source. Then it preflights `herdr integration status` and
# (for antigravity) the live model catalog, failing closed on any missing or
# outdated hook, and finally starts the container-local Herdr server.
#
# Providers are FAIL-CLOSED: a provider whose /secrets source is present must
# have every required credential file, a current Herdr integration, and (agy)
# a catalog containing gemini-3.7-flash, or the container refuses to start.
set -euo pipefail

fatal() { echo "agent-entrypoint: FAIL: $*" >&2; exit 1; }

wire_codex() { # ~/.codex writable + ro symlinked credential + hook
  local src=/secrets/codex
  [ -d "$src" ] || return 0   # not opted into this run
  local base=/home/board/.codex
  mkdir -p "$base"
  # Surgical wiring: only the real credential (auth.json) and the Herdr
  # integration hook (public code) come from the ro /secrets source. The
  # config.toml is NOT symlinked: codex needs a writable config.toml (it
  # persists the directory-trust answer into it), and the checked-in minimal
  # one was already installed writable into CODEX_HOME by agent-prepare.sh.
  for f in auth.json herdr-agent-state.sh; do
    [ -f "$src/$f" ] || fatal "codex file missing: $src/$f (host prerequisite: herdr integration install codex)"
    ln -sfn "$src/$f" "$base/$f"
  done
  echo "agent-entrypoint: codex wired (ro auth + herdr hook; writable minimal config.toml)"
}

wire_pi() { # ~/.pi/agent writable + ro symlinks for ONLY the harness-required files
  local src=/secrets/pi/agent
  [ -d "$src" ] || return 0
  local base=/home/board/.pi/agent
  mkdir -p "$base" "$base/extensions"
  # Surgical wiring: only credentials, config, and the Herdr integration
  # extension are linked — nothing else. No optional/user extensions ever,
  # so nothing host-specific (e.g. the host's macOS voice hook, which spawns
  # a Homebrew binary path that does not exist in the Linux container) can
  # reach the container and crash pi.
  # Older runs left ~/.pi/agent/extensions as a whole-directory symlink into
  # the ro /secrets mount; drop it (a symlink is removed without dereferencing)
  # so the real writable dir with only the integration extension is recreated.
  rm -rf "$base/extensions"
  mkdir -p "$base/extensions"
  for f in auth.json settings.json; do
    [ -f "$src/$f" ] || fatal "pi credential missing: $src/$f (host prerequisite: herdr integration install pi)"
    ln -sfn "$src/$f" "$base/$f"
  done
  [ -f "$src/extensions/herdr-agent-state.ts" ] \
    || fatal "pi herdr integration extension missing: $src/extensions/herdr-agent-state.ts (host: herdr integration install pi)"
  ln -sfn "$src/extensions/herdr-agent-state.ts" "$base/extensions/herdr-agent-state.ts"
  echo "agent-entrypoint: pi wired (ro auth/settings + herdr extension only)"
}

wire_agy() { # ~/.gemini writable + ro symlinked credentials + copied hook
  local src=/secrets/gemini
  [ -d "$src" ] || return 0
  local base=/home/board/.gemini
  # cache/ must exist before the onboarding marker below is redirected into it
  mkdir -p "$base/antigravity-cli" "$base/antigravity-cli/cache" "$base/config/hooks"
  # Surgical wiring: only the credential/config files the agy CLI actually
  # needs to authenticate and the Herdr integration hook. No optional user
  # config is mirrored; anything not strictly required stays on the host.
  for f in antigravity-cli/antigravity-oauth-token config/config.json \
           oauth_creds.json google_accounts.json state.json installation_id; do
    [ -e "$src/$f" ] || fatal "antigravity credential missing: $src/$f (host prerequisite: antigravity login + herdr integration install antigravity-cli)"
    ln -sfn "$src/$f" "$base/$f"
  done
  # Herdr integration hook is public code (from `herdr integration install
  # antigravity-cli`), never a credential: copy it so `herdr integration
  # status` sees the exact shipped hook while everything else stays writable.
  [ -f "$src/config/hooks/herdr-agent-state.sh" ] \
    || fatal "antigravity herdr hook missing: $src/config/hooks/herdr-agent-state.sh (host: herdr integration install antigravity-cli)"
  cp -f "$src/config/hooks/herdr-agent-state.sh" "$base/config/hooks/herdr-agent-state.sh"
  # antigravity gates interactive agentic sessions on an account-eligibility
  # check ("Verifying your account"); a container that generates its own
  # install identity re-triggers it and blocks a headless dispatch. Carry the
  # host install identity (non-secret: onboarding flags + install uuid) so the
  # container presents the same install Google already verified.
  if [ -f "$src/antigravity-cli/jetski_state.pbtxt" ]; then
    cp -f "$src/antigravity-cli/jetski_state.pbtxt" "$base/antigravity-cli/jetski_state.pbtxt"
    chmod 600 "$base/antigravity-cli/jetski_state.pbtxt"
  else
    fatal "antigravity install identity missing at $src/antigravity-cli/jetski_state.pbtxt (host: run the antigravity CLI once so it records its install id)"
  fi
  # agy shows an interactive first-run onboarding wizard (color-scheme picker)
  # when these are absent, which blocks a headless dispatch. Seed a checked-in
  # minimal UI settings file and the onboarding-complete marker — only if
  # absent, so a persistent interactive session's own theme keeps on restart.
  if [ ! -f "$base/antigravity-cli/settings.json" ]; then
    cp -f /repo/docker/agent-agy-settings.json "$base/antigravity-cli/settings.json"
    chmod 600 "$base/antigravity-cli/settings.json"
  fi
  if [ ! -f "$base/antigravity-cli/cache/onboarding.json" ]; then
    printf '{"consumerOnboardingComplete":true,"enterpriseOnboardingComplete":false,"onboardingComplete":true}\n' \
      > "$base/antigravity-cli/cache/onboarding.json"
    chmod 600 "$base/antigravity-cli/cache/onboarding.json"
  fi
  echo "agent-entrypoint: antigravity wired (ro creds via symlink, hook + install identity copied, onboarding seeded)"
}

# --- wire credentials before anything reads them ----------------------------
wire_codex
wire_pi
wire_agy

# --- Herdr integration preflight (fail closed) ------------------------------
echo "agent-entrypoint: herdr integration status"
herdr integration status 2>&1 | tee /tmp/herdr-integration-status.txt || true
# Only require the providers that were actually opted in (had /secrets).
missing=""
grep -E '^codex: current'                /tmp/herdr-integration-status.txt >/dev/null 2>&1 \
  || [ ! -d /secrets/codex ]           || missing="$missing codex"
grep -E '^pi: current'                   /tmp/herdr-integration-status.txt >/dev/null 2>&1 \
  || [ ! -d /secrets/pi ]               || missing="$missing pi"
grep -E '^antigravity-cli: current'       /tmp/herdr-integration-status.txt >/dev/null 2>&1 \
  || [ ! -d /secrets/gemini ]           || missing="$missing antigravity-cli"
if [ -n "$missing" ]; then
  fatal "missing/outdated Herdr integration(s):$missing — host prerequisite: herdr integration install pi codex antigravity-cli"
fi

# --- antigravity live catalog preflight (fail closed) -----------------------
# Only when antigravity was opted in: the live `agy --output-format json
# models` (root flag before the subcommand; JSON on stdout, spinner on
# stderr) must offer a gemini-3.7-flash-* variant before the daemon starts.
if [ -d /secrets/gemini ]; then
  command -v agy >/dev/null 2>&1 || fatal "agy CLI not on PATH (run agents prepare first)"
  models="$(agy --output-format json models 2>/dev/null || true)"
  printf '%s' "$models" | grep -q '"id":"gemini-3.7-flash' \
    || fatal "agy live catalog has no gemini-3.7-flash variant (auth missing? run the antigravity CLI login on the host)"
  echo "agent-entrypoint: agy catalog has gemini-3.7-flash"
fi

# A previous agent container may have been torn down without unlinking its
# socket (the agent-state volume persists). Remove a stale socket so a freshly
# started herdr can bind this path; it is owned exclusively by the agent
# containers of this sandbox.
rm -f /home/board/.config/herdr/herdr.sock
echo "agent-entrypoint: starting herdr server (container-local default session)"
exec herdr server
