#!/usr/bin/env bash
# herdr-board Docker sandbox — one entry point for isolated edit-test cycles.
#
# Runs the repository's deterministic gate set (including every provider-free
# live Herdr e2e scenario) inside a disposable, network-disabled, non-root
# container; provides a persistent container-local Herdr + board daemon for
# shell/CLI/TUI use; and gates real-provider smoke tests behind an explicit
# network + credential opt-in.
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
  smoke --provider <p> --allow-network
                             Real-provider smoke test (claude|codex|opencode);
                             explicit network + credential opt-in
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
  scripts/sandbox.sh smoke --provider codex --allow-network
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

INNER_PATH="/home/board/.local/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"
SMOKE_PATH="/home/board/.npm-global/bin:/home/board/.local/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"

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
    --cargo) run docker rm -f "$ENV_CONTAINER" >/dev/null 2>&1 || true; run docker volume rm "$VOL_CARGO" ;;
    --target) run docker volume rm "$VOL_TARGET" ;;
    --state) run docker rm -f "$ENV_CONTAINER" >/dev/null 2>&1 || true; run docker volume rm "$VOL_STATE" ;;
    --artifacts) run docker volume rm "$VOL_ARTIFACTS" ;;
    --image)
      local hash
      hash="$(image_hash)"
      resolve_platform
      run docker rmi "hb-sandbox:$hash-$PLATFORM" || true
      ;;
    --all)
      run docker rm -f "$ENV_CONTAINER" >/dev/null 2>&1 || true
      run docker volume rm "$VOL_CARGO" "$VOL_TARGET" "$VOL_STATE" "$VOL_ARTIFACTS" "$VOL_TMP"
      local hash
      hash="$(image_hash)"
      resolve_platform
      run docker rmi "hb-sandbox:$hash-$PLATFORM" || true
      ;;
    *) die "unknown reset scope '$scope'" ;;
  esac
  info "reset $scope: done"
}

cmd_smoke() {
  local provider="" allow_network=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --provider) [ $# -ge 2 ] || die "--provider needs a value"; provider="$2"; shift 2 ;;
      --allow-network) allow_network=1; shift ;;
      *) die "unknown smoke flag '$1'" ;;
    esac
  done
  [ -n "$provider" ] || die "smoke needs --provider <claude|codex|opencode>"
  case "$provider" in
    pi) die "refusing: the real Pi smoke requires a WezTerm GUI on the host (wezterm cli); run it on the host instead: E2E_REAL_PI=1 bash e2e/real-pi-smoke.sh" ;;
    antigravity) die "refusing: there is no real-provider Antigravity smoke in this repository" ;;
    claude|codex|opencode) ;;
    *) die "unknown provider '$provider' (supported: claude, codex, opencode)" ;;
  esac
  [ "$allow_network" -eq 1 ] || die "smoke requires the explicit --allow-network opt-in (real provider calls cost money)"

  # Host-side credential pre-checks: fail BEFORE any container launch.
  case "$provider" in
    claude)
      local d="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
      [ -d "$d" ] || die "missing Claude config dir: $d"
      for f in .credentials.json settings.json remote-settings.json; do
        [ -f "$d/$f" ] || die "missing Claude credential/config file: $d/$f"
      done
      ;;
    codex)
      local d="${CODEX_HOME:-$HOME/.codex}"
      [ -d "$d" ] || die "missing Codex config dir: $d"
      for f in auth.json config.toml herdr-agent-state.sh; do
        [ -f "$d/$f" ] || die "missing Codex file: $d/$f (herdr-agent-state.sh comes from: herdr integration install codex)"
      done
      ;;
    opencode)
      local cfg="${XDG_CONFIG_HOME:-$HOME/.config}/opencode"
      local data="${XDG_DATA_HOME:-$HOME/.local/share}/opencode"
      [ -f "$cfg/opencode.json" ] || die "missing OpenCode config: $cfg/opencode.json"
      [ -d "$data" ] || die "missing OpenCode data dir: $data"
      ;;
  esac

  docker_no_daemon_hint
  resolve_platform
  ensure_image
  ensure_volumes
  init_volumes
  info "smoke: network enabled + only the '$provider' credential dir mounted read-only"
  local flags secrets_env=()
  flags="$(base_flags | grep -v '^--network none$' | grep -v '^--tmpfs /tmp:.*$')"
  case "$provider" in
    claude)
      flags="$flags
-v ${CLAUDE_CONFIG_DIR:-$HOME/.claude}:/secrets/claude:ro"
      secrets_env=(-e CLAUDE_CONFIG_DIR=/secrets/claude -e E2E_REAL_CLAUDE_HAIKU=1)
      ;;
    codex)
      flags="$flags
-v ${CODEX_HOME:-$HOME/.codex}:/secrets/codex:ro"
      secrets_env=(-e CODEX_HOME=/secrets/codex -e E2E_REAL_CODEX=1)
      ;;
    opencode)
      flags="$flags
-v ${XDG_CONFIG_HOME:-$HOME/.config}/opencode:/secrets/opencode/opencode:ro
-v ${XDG_DATA_HOME:-$HOME/.local/share}/opencode:/secrets/opencode/data/opencode:ro"
      secrets_env=(-e XDG_CONFIG_HOME=/secrets/opencode -e XDG_DATA_HOME=/secrets/opencode/data -e E2E_REAL_OPENCODE=1)
      ;;
  esac
  flags="$flags
-v $VOL_TMP:/tmp
-e PATH=$SMOKE_PATH"
  # shellcheck disable=SC2086
  run docker run --rm $flags "${secrets_env[@]}" \
    "$IMAGE_TAG" bash /repo/docker/smoke.sh "$provider"
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
  smoke) cmd_smoke "$@" ;;
  artifacts) cmd_artifacts "$@" ;;
  lock) cmd_lock "$@" ;;
  down) cmd_down "$@" ;;
  reset) cmd_reset "$@" ;;
  doctor) cmd_doctor "$@" ;;
  *) usage >&2; die "unknown subcommand '$SUBCOMMAND'" ;;
esac
