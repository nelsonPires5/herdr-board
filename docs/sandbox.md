# Docker sandbox for isolated gates and E2E

`scripts/sandbox.sh` is a local developer tool that runs the repository's test
gates — including the full provider-free live Herdr E2E suite — inside a
disposable Docker container, so an agent or a human can iterate on a worktree
without touching the host's active Herdr sessions, board daemon, sockets,
databases, or workspaces.

Supported hosts: **Docker Engine on Linux** and **Docker through Colima on
macOS**; both **amd64** and **arm64** (Apple Silicon included). CI integration
is intentionally out of scope — this is a local tool.

## Setup

- Linux: install Docker Engine.
- macOS: install Colima (`brew install colima docker`) and start it
  (`colima start`). The wrapper talks to whatever docker CLI/context is active.

Check readiness any time:

```bash
scripts/sandbox.sh doctor
```

The image is built on first use from the pinned `docker/Dockerfile`: a
digest-pinned `rust:1.97.0-slim-bookworm` base, the Herdr 0.8.0 release asset
for your architecture verified by SHA-256, exact version string, and socket
protocol 19 (both `amd64` and `arm64` assets are pinned). Nothing floats; the
image is rebuilt automatically when the `docker/` directory changes.

## The one-command edit-test loop

```bash
scripts/sandbox.sh prepare   # once per worktree: image, volumes, dependency fetch
scripts/sandbox.sh gates     # the full deterministic suite, offline
```

`gates` runs, inside one isolated container:

1. the safety self-check (see below),
2. `cargo fmt --all --check`,
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
4. `cargo test --workspace --all-features`,
5. the Python test tier (`scripts/tests`),
6. the static harness gate (`e2e/test-harness.sh`),
7. all provider-free live Herdr scenarios via `e2e/run-all.sh --require-all`.

The first failing gate is named and the exit code is non-zero; a failing E2E
scenario is identified by the suite's own summary. `prepare` is the only step
that uses the network (dependency fetch + image build); `gates` runs with the
network disabled entirely and has no LLM cost.

Edits on the host are visible immediately: the worktree is bind-mounted
read-only at `/repo`, so a re-run after an edit needs **no image rebuild**.
Build output, Cargo caches, databases, sockets, logs, and artifacts live in
per-worktree Docker volumes outside the repository — repeated runs reuse the
caches and the repository stays clean.

Iterate on a subset of scenarios by passing filters (substring match, forwarded
to `run-all.sh`):

```bash
scripts/sandbox.sh gates 03-sessions
scripts/sandbox.sh gates 16- 17-          # several at once
```

If `Cargo.toml` changed and the lockfile is stale, `prepare` refuses with a
hint; regenerate it in a network container without ever mounting the repo
read-write:

```bash
scripts/sandbox.sh lock
```

## Isolation and safety

Every container the wrapper starts gets the same hard profile:

- non-root (`uid 1000`), `--cap-drop ALL`, `no-new-privileges`, no host PID
  namespace, no privileged mode, no host network; the container rootfs is
  ephemeral (`--rm`) and never committed back to the image;
- the worktree mounted **read-only** at `/repo`, with the build-output volume
  at `/repo/target` as the single deliberate writable spot under it (this is
  what keeps the herdr plugin contract `./target/release/board` working, and
  the volume lives outside the repository on the host);
- no host Herdr/board sockets, sessions, workspaces, databases, Docker socket,
  or user data directories are ever mounted;
- inherited `BOARD_*` / `HERDR_*` / provider variables are never forwarded —
  the container environment is an explicit allowlist;
- deterministic modes run with `--network none`.

`sandbox.sh selfcheck` (also gate 0 of every `gates` run) proves the profile
from inside the container: the source mount is read-only, the process is
non-root, `/proc/self/mountinfo` contains only allowlisted mounts (no docker
socket anywhere), and network egress is impossible (DNS and TCP probes must
fail).

The live suite keeps all of its own guards unchanged — ephemeral
`hb-e2e-<slug>-<pid>-<random64>` Herdr sessions, HMAC identity tokens,
mutation logging, and the post-run resource audit run exactly as on the host
or in CI. The sandbox adds isolation around them; it does not replace them.

## Shell and board CLI use

`sandbox.sh shell` starts (or reuses) a persistent environment container that
runs a container-local Herdr server; the board daemon auto-starts on first
use, exactly like on a host:

```bash
scripts/sandbox.sh shell                 # interactive bash
scripts/sandbox.sh board card list --json
scripts/sandbox.sh board board create my-board
scripts/sandbox.sh board version
```

Everything in there is disposable: Herdr sessions, workspaces, board
databases, and sockets live in the container and its state volume. Stop it
with `scripts/sandbox.sh down`.

## Interactive TUI

```bash
scripts/sandbox.sh tui
```

opens the real TUI in the environment container through an attached terminal;
resizing follows your terminal and quitting shuts the TUI down cleanly. The
fake-client TUI path (snapshot tests in `board-tui`) remains part of `gates`
for deterministic visual checks — the Docker path adds the real thing on top,
it does not replace it.

## Real-provider smoke tests (explicit opt-in)

```bash
scripts/sandbox.sh smoke --provider claude   --allow-network
scripts/sandbox.sh smoke --provider codex    --allow-network
scripts/sandbox.sh smoke --provider opencode --allow-network
```

This is the only mode with network access. It requires **both** the provider
choice and `--allow-network`; missing credentials or a missing opt-in fail
**before** any container launches, with a clear message. Only the chosen
provider's credential directory is mounted, read-only, at `/secrets`:
`~/.claude`, `~/.codex`, or `~/.config/opencode` + `~/.local/share/opencode`
(respecting `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `XDG_*` overrides). The host
must have the matching Herdr integration installed once
(`herdr integration install <provider>`) — the smoke scripts stage configs
from those directories themselves. Provider CLIs are Linux binaries installed
at smoke time into the state volume (never into the image, never logged).
Credentials are never copied into the image, the logs, or other modes.

`pi` is refused with a pointer to the host-side command (the real Pi smoke
requires a WezTerm GUI), and `antigravity` is refused (no real-provider
Antigravity smoke exists in this repository). These refusals are deliberate:
nothing silently skips.

Each smoke makes at most one real provider call — the same one-attempt cost
guard as on the host.

## Artifacts, cache reset, and cleanup

```bash
scripts/sandbox.sh artifacts            # copy run/e2e evidence out of the sandbox
scripts/sandbox.sh artifacts ~/e2e-out  # or to an explicit destination
scripts/sandbox.sh reset --target       # drop build output only
scripts/sandbox.sh reset --all          # volumes + image for this worktree
```

`artifacts` refuses to write inside the repository; the default destination is
`~/.cache/herdr-board/sandbox-artifacts/<timestamp>`. `reset` only ever
touches resources with this worktree's `hb-sb-<slug>-…` prefix (plus the
shared sandbox image tag).

## Architecture behavior

The default platform is the docker server's architecture; Herdr artifacts are
pinned per architecture (`herdr-linux-x86_64` / `herdr-linux-aarch64`), and
the same workflows run on both. To run the other architecture explicitly:

```bash
scripts/sandbox.sh --platform linux/amd64 prepare
scripts/sandbox.sh --platform linux/amd64 gates
```

A non-native platform is **transparent emulation** (QEMU via binfmt): on
Colima/Apple Silicon an amd64 run works but is much slower, and building the
amd64 image needs the emulator registered in the VM (Docker Desktop ships it;
on Colima run `docker run --privileged tonistiigi/binfmt --install amd64`
once). No test silently skips by architecture — if an architecture cannot
run, the gate fails loudly.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `docker daemon unreachable` | Start Docker Engine; on macOS: `colima start`. Then `scripts/sandbox.sh doctor`. |
| First `prepare` is slow | Image build + dependency fetch happen once; later runs reuse caches. |
| `prepare` fails on `cargo fetch --locked` | The lockfile is stale after a `Cargo.toml` change: `scripts/sandbox.sh lock`, then retry. |
| `gates` fails in cargo test on a read-only repo | Expected only when a snapshot test legitimately fails; the failure is real, fix it and re-run (insta cannot write `.snap.new` on the read-only mount). |
| Cannot run two sandbox workflows at once from one worktree | The gates container is one-at-a-time by design (shared target volume); use separate worktrees, which get separate volumes. |
| Emulated (`--platform`) run is very slow or OOMs | Give Colima more resources (`colima stop && colima start --cpu 8 --memory 8`) or run natively. |
| `board` not built in the sandbox | Run `scripts/sandbox.sh prepare` first. |

## What is where

| Path | Purpose |
|---|---|
| `scripts/sandbox.sh` | The one entry command (argument handling, isolation profile, volumes, modes). |
| `docker/Dockerfile` | Pinned image: digest-pinned Rust base, per-arch SHA-verified Herdr 0.8.0, non-root user. |
| `docker/selfcheck.sh`, `docker/lib.sh` | The in-container isolation proof and shared mount audit. |
| `docker/gates.sh` | Gate sequence with named first failure and e2e evidence export. |
| `docker/prepare.sh` | Dependency fetch + `board` build (network-enabled step). |
| `docker/env-entrypoint.sh` | Persistent environment container (Herdr server). |
| `docker/smoke.sh` | Real-provider smoke runner (opt-in network + credentials). |
| `docker/lock.sh` | Lockfile regeneration through a single-file write-back mount. |
| `scripts/tests/test_sandbox.py` | Daemon-free contract tests: arguments, isolation defaults, pinning, cleanup, refusals. |
