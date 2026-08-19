#!/usr/bin/env bash
# herdr-board Docker sandbox — one entry point for isolated edit-test cycles.
#
# Runs the repository's deterministic gate set (including every provider-free
# live Herdr e2e scenario) inside a disposable, network-disabled, non-root
# container; provides a persistent container-local Herdr + board daemon for
# shell/CLI/TUI use; and gates real-provider agent dispatches (pi/codex/
# antigravity) behind an explicit network + read-only credential opt-in.
#
# Supported hosts: Docker Engine on Linux, Docker via Colima on macOS
# (amd64 and arm64). See docs/sandbox.md for the full guide.
#
# Usage: scripts/sandbox.sh [--dry-run] [--platform linux/amd64|linux/arm64] \
#           <subcommand> [args...]
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$script_dir/.." && pwd)"
DOCKER_DIR="$REPO_ROOT/docker"

usage() {
  cat <<'EOF'
herdr-board sandbox — isolated Docker environment for gates, e2e, and validation

Usage: scripts/sandbox.sh [global flags] <subcommand> [args...]

Global flags:
  --dry-run                  Print the docker commands instead of running them
  --platform <linux/amd64|linux/arm64>
                             Target platform (default: the docker server arch;
                             a non-native choice is transparent emulation)
  -h, --help                 Show this help

Subcommands:
  gates [filter...]          Full deterministic suite offline: safety self-check,
                             fmt, clippy, workspace tests, python tests, static
                             harness gate, and all provider-free live Herdr e2e
                             scenarios (--require-all). Filters pass through to
                             e2e/run-all.sh (substring match).
  prepare                    Build the image if stale, create volumes, fetch
                             dependencies (network used only here), build board
  selfcheck                  Run the in-container isolation proof standalone
  shell                      Interactive bash against container-local Herdr
  board <args...>            Run a board CLI command in the sandbox environment
  tui                        Open the interactive TUI in the sandbox environment
  agent --provider <p> --allow-network [--model M] [--effort E]
                             One-shot end-to-end real-provider dispatch in the
                             sandbox (pi|codex|antigravity): a card runs in a
                             dedicated agent container and must finish ok
  agent --allow-network --tui [--seed]
                             Start the persistent agent sandbox (network +
                             read-only credentials) and open the interactive
                             TUI; --seed adds one card per harness into Todo so
                             you can drag them into Running. The agent
                             container is torn down when the TUI quits
  (CURRENCY NOTE: every agent run — one-shot or a dragged seeded card — makes
   real paid provider API calls; --allow-network is the explicit opt-in)
  artifacts [DEST]           Copy sandbox artifacts to the host
                             (default: ~/.cache/herdr-board/sandbox-artifacts/<ts>)
  lock                       Regenerate Cargo.lock in a network container and
                             write it back to the worktree
  down                       Stop the sandbox environment container
  reset [--cargo|--target|--state|--artifacts|--image|--all]
                             Remove sandbox caches / image (never touches
                             anything not created by this tool)
  doctor                     Diagnose the docker setup and sandbox resources

Examples:
  scripts/sandbox.sh prepare
  scripts/sandbox.sh gates
  scripts/sandbox.sh gates 03-sessions        # e2e iteration on one scenario
  scripts/sandbox.sh board card list --json
  scripts/sandbox.sh agent --provider pi --allow-network
  scripts/sandbox.sh agent --provider codex --allow-network --model gpt-5.6-luna --effort low
  scripts/sandbox.sh agent --allow-network --tui   # all three harnesses in the TUI
EOF
}

die() { echo "sandbox: $*" >&2; exit 2; }
info() { echo "sandbox: $*"; }

# ---------------------------------------------------------------------------
# Global state
# ---------------------------------------------------------------------------
DRY_RUN=0
PLATFORM=""
SUBCOMMAND=""

# ---------------------------------------------------------------------------
# Resource naming (per worktree; everything this tool creates is prefixed)
# ---------------------------------------------------------------------------
slug_raw="$(basename "$REPO_ROOT")"
SLUG="$(printf '%s' "$slug_raw" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9-' '-' | sed 's/^-*//;s/-*$//' | cut -c1-40)"
[ -n "$SLUG" ] || die "cannot derive a sandbox slug from $REPO_ROOT"
PREFIX="hb-sb-$SLUG"
VOL_CARGO="$PREFIX-cargo"
VOL_TARGET="$PREFIX-target"
VOL_STATE="$PREFIX-state"
VOL_ARTIFACTS="$PREFIX-artifacts"
VOL_TMP="$PREFIX-tmp"
ENV_CONTAINER="$PREFIX-env"
VOL_AGENT_STATE="$PREFIX-agent-state"
AGENT_CONTAINER="$PREFIX-agent"

INNER_PATH="/home/board/.local/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"
AGENT_PATH="/home/board/.npm-global/bin:/home/board/.local/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"
AGENT_BIN="/home/board/.local/bin/agy"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
run() { # run <cmd...> — echo then execute (or just echo under --dry-run)
  # Printed unescaped on purpose: --dry-run output is documentation that
  # users copy-paste; execution below always uses the exact argv array.
  printf 'sandbox: + %s\n' "$*"
  [ "$DRY_RUN" -eq 1 ] && return 0
  "$@"
}

docker_no_daemon_hint() {
  # --dry-run must compose and print the planned commands without a daemon
  # (that is what the daemon-free contract tests exercise).
  [ "$DRY_RUN" -eq 1 ] && return 0
  command -v docker >/dev/null 2>&1 || die "docker CLI not found; install Docker Engine or Colima (docs/sandbox.md)"
  if ! docker info >/dev/null 2>&1; then
    if [ "$(uname -s)" = Darwin ] && command -v colima >/dev/null 2>&1; then
      die "docker daemon unreachable — try: colima start"
    fi
    die "docker daemon unreachable — start Docker Engine first (docs/sandbox.md)"
  fi
}

uname_to_docker_arch() {
  case "$1" in
    x86_64) echo amd64 ;;
    aarch64|arm64) echo arm64 ;;
    *) return 1 ;;
  esac
}

resolve_platform() { # sets PLATFORM (amd64|arm64)
  local p="${PLATFORM#linux/}"
  if [ -n "$p" ]; then
    case "$p" in amd64|arm64) PLATFORM="$p" ;; *) die "unsupported platform '$p' (use linux/amd64 or linux/arm64)" ;; esac
    return
  fi
  local server
  server="$(docker version -f '{{.Server.Arch}}' 2>/dev/null || true)"
  if [ -z "$server" ]; then
    server="$(uname_to_docker_arch "$(uname -m)" || true)"
  fi
  case "$server" in
    amd64|arm64) PLATFORM="$server" ;;
    *) die "cannot determine docker server architecture (got '${server:-none}'); pass --platform explicitly" ;;
  esac
}

image_hash() {
  {
    cd "$DOCKER_DIR" && find . -type f | LC_ALL=C sort | xargs shasum -a 256
  } 2>/dev/null | shasum -a 256 | awk '{print substr($1, 1, 12)}'
}

IMAGE_TAG="" # set after resolve_platform

ensure_image() {
  local hash
  hash="$(image_hash)"
  IMAGE_TAG="hb-sandbox:$hash-$PLATFORM"
  info "image: $IMAGE_TAG"
  if [ "$DRY_RUN" -eq 0 ] && ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
    info "building image (pinned base, rust 1.97.0, herdr 0.8.0 verified per arch)"
    run docker build --platform "linux/$PLATFORM" -t "$IMAGE_TAG" "$DOCKER_DIR"
  fi
}

ensure_volumes() {
  local v
  for v in "$VOL_CARGO" "$VOL_TARGET" "$VOL_STATE" "$VOL_ARTIFACTS" "$VOL_TMP"; do
    if [ "$DRY_RUN" -eq 1 ]; then
      run docker volume create "$v" >/dev/null
    elif ! docker volume inspect "$v" >/dev/null 2>&1; then
      run docker volume create "$v" >/dev/null
    fi
  done
}

init_volumes() { # one-time chown so the non-root user owns its volumes
  if [ "$DRY_RUN" -eq 0 ]; then
    docker run --rm --user 0:0 --network none \
      -v "$VOL_CARGO":/opt/cargo -v "$VOL_TARGET":/repo/target \
      -v "$VOL_STATE":/home/board -v "$VOL_ARTIFACTS":/artifacts -v "$VOL_TMP":/tmp \
      --entrypoint bash "$IMAGE_TAG" \
      -c 'chown -R 1000:1000 /opt/cargo /repo/target /home/board /artifacts /tmp' >/dev/null
  else
    info "(dry-run) would chown volumes to 1000:1000 once"
  fi
}

sandbox_ready() { # shared ensure: image + volumes + ownership
  docker_no_daemon_hint
  resolve_platform
  ensure_image
  ensure_volumes
  init_volumes
  # The build-output volume mounts at /repo/target; the nested mountpoint
  # must exist in the bind source (gitignored, never committed).
  [ "$DRY_RUN" -eq 1 ] || mkdir -p "$REPO_ROOT/target"
}

# Base isolation flags for deterministic containers. Every mode gets the same
# hard profile: non-root, dropped caps, no host PID/net, read-only repo bind,
# and all writable state in volumes/tmpfs. (No --read-only rootfs: the nested
# /repo/target build-output volume cannot be created on a read-only rootfs,
# and the repo itself is protected by the :ro bind mount.)
base_flags() {
  cat <<EOF
--user 1000:1000
--cap-drop ALL
--security-opt no-new-privileges
--network none
--tmpfs /tmp:rw,exec,nosuid,nodev,size=8g,mode=1777
--tmpfs /run:rw,nosuid,nodev,size=16m,mode=755
-v $REPO_ROOT:/repo:ro
-v $VOL_CARGO:/opt/cargo
-v $VOL_TARGET:/repo/target
-v $VOL_STATE:/home/board
-v $VOL_ARTIFACTS:/artifacts
-e HOME=/home/board
-e PATH=$INNER_PATH
-e CARGO_HOME=/opt/cargo
-e CARGO_TARGET_DIR=/repo/target
EOF
}

run_isolated() { # run_isolated <script-inside-container> [args...] — bash /repo/docker/<script>
  local script="$1"; shift
  local flags
  flags="$(base_flags)"
  # shellcheck disable=SC2086
  run docker run --rm $flags "$IMAGE_TAG" bash "/repo/docker/$script" "$@"
}

# ---------------------------------------------------------------------------
# Environment container (persistent container-local Herdr + board daemon)
# ---------------------------------------------------------------------------
ensure_env_container() {
  local state
  state="$(docker container inspect -f '{{.State.Status}}' "$ENV_CONTAINER" 2>/dev/null || true)"
  if [ "$state" = running ]; then
    return
  fi
  if [ -n "$state" ]; then
    run docker rm -f "$ENV_CONTAINER" >/dev/null
  fi
  info "starting sandbox environment container ($ENV_CONTAINER)"
  local flags
  flags="$(base_flags)"
  # shellcheck disable=SC2086
  run docker run -d --name "$ENV_CONTAINER" --init $flags \
    "$IMAGE_TAG" bash /repo/docker/env-entrypoint.sh >/dev/null
  # Wait for the container-local Herdr socket.
  local i
  for i in $(seq 1 60); do
    if docker exec "$ENV_CONTAINER" test -S /home/board/.config/herdr/herdr.sock 2>/dev/null; then
      info "herdr server ready (container-local socket)"
      return
    fi
    sleep 0.5
  done
  die "herdr server did not become ready in $ENV_CONTAINER; try: scripts/sandbox.sh doctor"
}

require_board_binary() {
  if ! docker exec "$ENV_CONTAINER" test -x /home/board/.local/bin/board 2>/dev/null; then
    die "board is not built in the sandbox yet; run: scripts/sandbox.sh prepare"
  fi
}

# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------
cmd_gates() {
  sandbox_ready
  info "running the full deterministic gate set offline (network disabled)"
  run_isolated gates.sh "$@"
  info "gates: PASS — artifacts available via: scripts/sandbox.sh artifacts"
}

cmd_prepare() {
  sandbox_ready
  info "prepare: fetching dependencies and building board (network enabled for this step only)"
  local flags
  flags="$(base_flags | grep -v '^--network none$')"
  # shellcheck disable=SC2086
  run docker run --rm $flags "$IMAGE_TAG" bash /repo/docker/prepare.sh
  info "prepare: done"
}

cmd_selfcheck() {
  sandbox_ready
  run_isolated selfcheck.sh
}

cmd_shell() {
  docker_no_daemon_hint
  resolve_platform
  ensure_image
  ensure_volumes
  init_volumes
  ensure_env_container
  info "opening a shell in $ENV_CONTAINER (exit to leave; the container keeps running)"
  run docker exec -it "$ENV_CONTAINER" bash
}

cmd_board() {
  [ $# -gt 0 ] || die "board needs arguments, e.g.: scripts/sandbox.sh board card list"
  docker_no_daemon_hint
  resolve_platform
  ensure_image
  ensure_volumes
  init_volumes
  ensure_env_container
  require_board_binary
  run docker exec -e PATH="$INNER_PATH" "$ENV_CONTAINER" board "$@"
}

cmd_tui() {
  docker_no_daemon_hint
  resolve_platform
  ensure_image
  ensure_volumes
  init_volumes
  ensure_env_container
  require_board_binary
  info "opening the TUI in $ENV_CONTAINER (resize follows your terminal; q to quit)"
  run docker exec -it -e PATH="$INNER_PATH" -e TERM="${TERM:-xterm-256color}" "$ENV_CONTAINER" board tui
}

cmd_down() {
  docker_no_daemon_hint
  run docker rm -f "$ENV_CONTAINER" >/dev/null 2>&1 || info "environment container not present"
  info "environment container stopped"
}

cmd_artifacts() {
  local dest="${1:-}"
  if [ -z "$dest" ]; then
    dest="${XDG_CACHE_HOME:-$HOME/.cache}/herdr-board/sandbox-artifacts/$(date +%Y%m%dT%H%M%S)"
  fi
  case "$dest" in
    "$REPO_ROOT"|"$REPO_ROOT"/*) die "refusing to write artifacts inside the repository" ;;
  esac
  docker_no_daemon_hint
  resolve_platform
  ensure_image
  mkdir -p -m 700 "$dest"
  local host_uid host_gid
  host_uid="$(id -u)"
  host_gid="$(id -g)"
  run docker run --rm --user 0:0 --network none \
    -v "$VOL_ARTIFACTS":/from:ro -v "$dest":/out \
    --entrypoint bash "$IMAGE_TAG" \
    -c "cp -a /from/. /out/ && chown -R $host_uid:$host_gid /out"
  info "artifacts copied to: $dest"
}

cmd_lock() {
  docker_no_daemon_hint
  resolve_platform
  ensure_image
  ensure_volumes
  info "regenerating Cargo.lock in a network container (writes back only Cargo.lock)"
  run docker run --rm \
    --user 1000:1000 --cap-drop ALL --security-opt no-new-privileges \
    -v "$REPO_ROOT":/repo:ro \
    -v "$REPO_ROOT/Cargo.lock":/out/Cargo.lock \
    -v "$VOL_CARGO":/opt/cargo \
    -v "$VOL_TMP":/tmp \
    -e HOME=/home/board \
    -e PATH="$INNER_PATH" \
    -e CARGO_HOME=/opt/cargo \
    --entrypoint bash "$IMAGE_TAG" /repo/docker/lock.sh
  info "Cargo.lock updated"
}

cmd_reset() {
  docker_no_daemon_hint
  local scope="${1:-}"
  [ -n "$scope" ] || die "reset needs a scope: --cargo|--target|--state|--artifacts|--image|--all"
  case "$scope" in
    --cargo) run docker rm -f "$ENV_CONTAINER" "$AGENT_CONTAINER" >/dev/null 2>&1 || true; run docker volume rm "$VOL_CARGO" ;;
    --target) run docker volume rm "$VOL_TARGET" ;;
    --state) run docker rm -f "$ENV_CONTAINER" "$AGENT_CONTAINER" >/dev/null 2>&1 || true; run docker volume rm "$VOL_STATE" "$VOL_AGENT_STATE" ;;
    --artifacts) run docker volume rm "$VOL_ARTIFACTS" ;;
    --image)
      local hash
      hash="$(image_hash)"
      resolve_platform
      run docker rmi "hb-sandbox:$hash-$PLATFORM" || true
      ;;
    --all)
      run docker rm -f "$ENV_CONTAINER" "$AGENT_CONTAINER" >/dev/null 2>&1 || true
      run docker volume rm "$VOL_CARGO" "$VOL_TARGET" "$VOL_STATE" "$VOL_AGENT_STATE" "$VOL_ARTIFACTS" "$VOL_TMP"
      local hash
      hash="$(image_hash)"
      resolve_platform
      run docker rmi "hb-sandbox:$hash-$PLATFORM" || true
      ;;
    *) die "unknown reset scope '$scope'" ;;
  esac
  info "reset $scope: done"
}

# ---------------------------------------------------------------------------
# Agent mode (real-provider dispatches in a dedicated network container)
# ---------------------------------------------------------------------------
agent_base_flags() { # base isolation profile with the DEDICATED agent state vol
  local flags
  flags="$(base_flags | grep -v -- "-v $VOL_STATE:/home/board")"
  printf '%s\n-v %s:/home/board\n' "$flags" "$VOL_AGENT_STATE"
}

agent_ensure_state() { # agent-state volume + reuse the offline-built board binary
  if [ "$DRY_RUN" -eq 1 ]; then
    run docker volume create "$VOL_AGENT_STATE" >/dev/null
  elif ! docker volume inspect "$VOL_AGENT_STATE" >/dev/null 2>&1; then
    run docker volume create "$VOL_AGENT_STATE" >/dev/null
  fi
  if [ "$DRY_RUN" -eq 0 ]; then
    docker run --rm --user 0:0 --network none -v "$VOL_AGENT_STATE":/home/board \
      --entrypoint bash "$IMAGE_TAG" -c 'chown -R 1000:1000 /home/board' >/dev/null
    if ! docker run --rm --user 0:0 --network none \
      -v "$VOL_STATE":/src:ro -v "$VOL_AGENT_STATE":/dst \
      --entrypoint bash "$IMAGE_TAG" \
      -c 'mkdir -p /dst/.local/bin && cp -f /src/.local/bin/board /dst/.local/bin/board 2>/dev/null && chown 1000:1000 /dst/.local/bin/board' >/dev/null 2>&1; then
      die "board is not built in the sandbox yet; run: scripts/sandbox.sh prepare"
    fi
  fi
}

agent_provision_clis() { # pinned provider CLIs into the agent state volume
  info "agent: provisioning pinned provider CLIs ($AGENT_CONTAINER state volume)"
  local flags
  flags="$(agent_base_flags | grep -v '^--network none$')"
  # shellcheck disable=SC2086
  run docker run --rm $flags "$IMAGE_TAG" bash /repo/docker/agent-prepare.sh
}

cmd_agent() {
  local provider="" model="" effort="" allow_network=0 tui=0 seed=0
  local providers=() mounts=() env_args=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --provider) [ $# -ge 2 ] || die "--provider needs a value"; provider="$2"; shift 2 ;;
      --model) [ $# -ge 2 ] || die "--model needs a value"; model="$2"; shift 2 ;;
      --effort) [ $# -ge 2 ] || die "--effort needs a value"; effort="$2"; shift 2 ;;
      --allow-network) allow_network=1; shift ;;
      --tui) tui=1; shift ;;
      --seed) seed=1; shift ;;
      *) die "unknown agent flag '$1'" ;;
    esac
  done
  [ "$allow_network" -eq 1 ] || die "agent requires the explicit --allow-network opt-in (real provider calls cost money)"

  if [ -n "$provider" ]; then
    case "$provider" in
      pi|codex|antigravity) providers=("$provider") ;;
      *) die "unknown provider '$provider' (supported: pi, codex, antigravity)" ;;
    esac
  elif [ "$tui" -eq 1 ]; then
    providers=(pi codex antigravity)
  else
    die "agent needs --provider <pi|codex|antigravity> (or --tui for the interactive sandbox with all harnesses)"
  fi

  # --seed seeds one card per harness (all three); a lone --provider could
  # never scope that, so reject the combination instead of mounting one
  # provider while creating cards for three.
  if [ "$seed" -eq 1 ] && [ -n "$provider" ]; then
    die "--tui --seed seeds all three harnesses; drop --provider (one-shot needs --provider without --tui --seed)"
  fi

  # Host credential + Herdr integration pre-checks: fail BEFORE any launch.
  local p
  for p in "${providers[@]}"; do
    case "$p" in
      pi)
        local d="$HOME/.pi/agent"
        [ -d "$d" ] || die "missing Pi agent dir: $d"
        for f in auth.json settings.json extensions/herdr-agent-state.ts; do
          [ -f "$d/$f" ] || die "missing Pi file: $d/$f (herdr-agent-state.ts comes from: herdr integration install pi)"
        done
        ;;
      codex)
        local d="${CODEX_HOME:-$HOME/.codex}"
        [ -d "$d" ] || die "missing Codex config dir: $d"
        for f in auth.json config.toml herdr-agent-state.sh; do
          [ -f "$d/$f" ] || die "missing Codex file: $d/$f (herdr-agent-state.sh comes from: herdr integration install codex)"
        done
        ;;
      antigravity)
        local d="$HOME/.gemini"
        [ -d "$d" ] || die "missing Antigravity config dir: $d"
        for f in config/config.json antigravity-cli/antigravity-oauth-token \
                 oauth_creds.json google_accounts.json state.json installation_id \
                 antigravity-cli/jetski_state.pbtxt config/hooks/herdr-agent-state.sh; do
          [ -e "$d/$f" ] || die "missing Antigravity credential/hook: $d/$f (hook comes from: herdr integration install antigravity-cli)"
        done
        ;;
    esac
  done

  docker_no_daemon_hint
  resolve_platform
  ensure_image
  ensure_volumes
  init_volumes
  agent_ensure_state
  agent_provision_clis

  info "agent: starting real-provider container ($AGENT_CONTAINER)"
  local flags i
  flags="$(agent_base_flags | grep -v '^--network none$')"
  for p in "${providers[@]}"; do
    case "$p" in
      pi) mounts+=("-v" "$HOME/.pi/agent:/secrets/pi/agent:ro") ;;
      codex) mounts+=("-v" "${CODEX_HOME:-$HOME/.codex}:/secrets/codex:ro") ;;
      antigravity) mounts+=("-v" "$HOME/.gemini:/secrets/gemini:ro") ;;
    esac
  done
  env_args=(-e AGY_BIN="$AGENT_BIN" -e PATH="$AGENT_PATH")
  if [ "$DRY_RUN" -eq 1 ]; then
    # --dry-run documents every step; the real docker calls stay quiet below.
    run docker rm -f "$AGENT_CONTAINER"
    # shellcheck disable=SC2086
    run docker run -d --name "$AGENT_CONTAINER" --init $flags "${mounts[@]}" "${env_args[@]}" \
      "$IMAGE_TAG" bash /repo/docker/agent-entrypoint.sh
  else
    docker rm -f "$AGENT_CONTAINER" >/dev/null 2>&1 || true
    # shellcheck disable=SC2086
    docker run -d --name "$AGENT_CONTAINER" --init $flags "${mounts[@]}" "${env_args[@]}" \
      "$IMAGE_TAG" bash /repo/docker/agent-entrypoint.sh >/dev/null
  fi
  # Install the teardown trap immediately after launch, so an interruption
  # during the readiness wait also tears the (networked) agent container down.
  cleanup_agent() { run docker rm -f "$AGENT_CONTAINER" >/dev/null 2>&1 || true; }
  trap cleanup_agent EXIT INT TERM

  if [ "$DRY_RUN" -eq 1 ]; then
    info "(dry-run) agent herdr server ready (would wait for the socket)"
  else
    local i
    for i in $(seq 1 120); do
      # functional probe, not just a socket file: a stale socket left in the
      # persisted volume by a torn-down predecessor must not count as ready
      if docker exec "$AGENT_CONTAINER" herdr api snapshot >/dev/null 2>&1; then
        info "agent herdr server ready (agent state socket)"
        break
      fi
      sleep 0.5
      if [ "$i" -eq 120 ]; then
        run docker rm -f "$AGENT_CONTAINER" >/dev/null 2>&1 || true
        die "agent herdr did not become ready (see agent-entrypoint preflight); try: scripts/sandbox.sh doctor"
      fi
    done
  fi

  if [ "$tui" -eq 1 ]; then
    if [ "$seed" -eq 1 ]; then
      info "agent: seeding one card per harness into Todo (drag them to Running to dispatch)"
      # shellcheck disable=SC2086
      run docker exec -e PATH="$AGENT_PATH" -e AGY_BIN="$AGENT_BIN" "$AGENT_CONTAINER" bash /repo/docker/agent-run.sh seed
    fi
    info "agent: opening the TUI ($AGENT_CONTAINER; q quits and tears the container down)"
    run docker exec -it -e PATH="$AGENT_PATH" -e AGY_BIN="$AGENT_BIN" -e TERM="${TERM:-xterm-256color}" "$AGENT_CONTAINER" board tui
  else
    [ -n "$provider" ] || die "one-shot agent mode needs --provider"
    info "agent: dispatching one real '$provider' run (model '${model:-default}', effort '${effort:-low}')"
    # shellcheck disable=SC2086
    run docker exec -e PATH="$AGENT_PATH" -e AGY_BIN="$AGENT_BIN" "$AGENT_CONTAINER" bash \
      /repo/docker/agent-run.sh one-shot "$provider" "$model" "$effort"
  fi
}

cmd_doctor() {
  echo "== sandbox doctor =="
  echo "repo:      $REPO_ROOT (mounted read-only at /repo)"
  echo "resources: prefix $PREFIX"
  if ! command -v docker >/dev/null 2>&1; then
    echo "docker:    MISSING — install Docker Engine (Linux) or Colima (macOS)"
    exit 2
  fi
  if ! docker info >/dev/null 2>&1; then
    echo "docker:    daemon unreachable"
    [ "$(uname -s)" = Darwin ] && command -v colima >/dev/null 2>&1 && echo "hint:     colima start"
    exit 2
  fi
  docker info --format 'docker:    server {{.ServerVersion}} ({{.Architecture}}), {{.OSType}}'
  resolve_platform
  ensure_image
  echo "volumes:   $(docker volume ls -q | grep -c "^$PREFIX-" || true) sandbox volume(s) for this worktree"
  docker volume ls -q | grep "^$PREFIX-" | sed 's/^/           /' || true
  local state
  state="$(docker container inspect -f '{{.State.Status}}' "$ENV_CONTAINER" 2>/dev/null || echo absent)"
  echo "env:       $ENV_CONTAINER ($state)"
  state="$(docker container inspect -f '{{.State.Status}}' "$AGENT_CONTAINER" 2>/dev/null || echo absent)"
  echo "agent:     $AGENT_CONTAINER ($state; real-provider, network + ro secrets)"
  echo "disk:"
  docker system df 2>/dev/null | sed 's/^/           /' || true
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --platform) [ $# -ge 2 ] || die "--platform needs a value"; PLATFORM="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    -*) die "unknown flag '$1' (see --help)" ;;
    *) SUBCOMMAND="$1"; shift; break ;;
  esac
done
[ -n "$SUBCOMMAND" ] || { usage >&2; exit 2; }

case "$SUBCOMMAND" in
  gates) cmd_gates "$@" ;;
  prepare) cmd_prepare "$@" ;;
  selfcheck) cmd_selfcheck "$@" ;;
  shell) cmd_shell "$@" ;;
  board) cmd_board "$@" ;;
  tui) cmd_tui "$@" ;;
  agent) cmd_agent "$@" ;;
  artifacts) cmd_artifacts "$@" ;;
  lock) cmd_lock "$@" ;;
  down) cmd_down "$@" ;;
  reset) cmd_reset "$@" ;;
  doctor) cmd_doctor "$@" ;;
  *) usage >&2; die "unknown subcommand '$SUBCOMMAND'" ;;
esac
