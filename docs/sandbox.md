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
digest-pinned `rust:1.97.0-slim-bookworm` base, the Herdr 0.8.2 release asset
for your architecture verified by SHA-256, exact version string, and socket
protocol 20 (both `amd64` and `arm64` assets are pinned). Nothing floats; the
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

## Real-provider agent mode (explicit opt-in)

The sandbox can run a **real provider end to end** (pi, codex, or antigravity)
in a dedicated agent container that has network access and the provider's
credentials mounted read-only. This replaces the earlier host-side single-call
smoke: a card actually runs against the real provider and must finish
`board done --outcome ok`.

```bash
scripts/sandbox.sh agent --provider codex --allow-network
scripts/sandbox.sh agent --provider pi       --allow-network --model opencode-go/deepseek-v4-flash --effort low
scripts/sandbox.sh agent --provider antigravity --allow-network   # model gemini-3.7-flash, effort low
scripts/sandbox.sh agent --allow-network --tui --seed        # all three harnesses in the interactive TUI
```

This is the only mode with network access (besides `prepare`/`lock`). It
requires **both** a provider choice (if not `--tui`) and `--allow-network`;
missing credentials or a missing Herdr integration hook fail **before** any
container launches, with a clear message naming the host prerequisite.

> **Cost:** one-shot dispatch and every seeded card you drag into "Running"
> start a real agent that makes **paid provider API calls**. The cheap
defaults above keep the cost low, but nothing in this mode is free and
nothing is rate-limited.

**Host prerequisites (one-time, reversible):** the host must already be
logged in to each provider and have the matching Herdr integration hook
installed — `herdr integration install pi`, `herdr integration install
codex`, `herdr integration install antigravity-cli` (confirm with
`herdr integration status`; `pi` is usually already present). Locally:

- pi:        `~/.pi/agent` (auth.json, settings.json, `extensions/herdr-agent-state.ts`)
- codex:     `~/.codex` (auth.json, config.toml, `herdr-agent-state.sh`)
- antigravity: `~/.gemini` (oauth creds + `jetski_state.pbtxt` install identity + `config/hooks/herdr-agent-state.sh`)

The agent container runs on its **own state volume** (`...-agent-state`), so
it never collides with the offline environment container. The pinned provider
CLIs (pi 0.84.2, codex 0.147.0, and antigravity from a pinned tarball) are
installed there by `docker/agent-prepare.sh`; the antigravity tarball is
verified against the SHA-512 pin in `docker/agy-pin.txt`, never fetched via
the floating `install.sh`. Credentials are never copied into the image, the
logs, or other modes: they are mounted read-only at `/secrets/<provider>` and
wired into the writable container HOME through read-only symlinks (the agent's
own session/cache state stays in the agent state volume).

The agent entrypoint preflights `herdr integration status` inside the
container (fails closed on a missing/outdated hook) and, for antigravity,
requires the live `agy --output-format json models` catalog to offer
`gemini-3.7-flash` before the daemon starts. Default models/efforts (all
cheap):

| provider | model | effort |
|---|---|---|
| pi | `opencode-go/deepseek-v4-flash` | low |
| codex | `gpt-5.6-luna` | low |
| antigravity | `gemini-3.7-flash` | low |

One-shot (`agent --provider <p> ...`) dispatches a single card onto an auto
"Running" column (column timeout 15 min), polls up to a 20-minute watchdog,
and requires `board done --outcome ok`; the agent container is torn down on
exit. With `--tui` the agent container stays up: `--seed` creates one card per
harness in the manual "Todo" column so you can drag cards into "Running" and
watch each real harness run in the sandbox TUI (quitting the TUI tears the
agent container down). Evidence (sanitized: versions/checksums, card/run JSON,
herdr snapshots — never credentials or raw env) is written under `/artifacts`
and copied out with `scripts/sandbox.sh artifacts`.

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
| `docker/Dockerfile` | Pinned image: digest-pinned Rust base, per-arch SHA-verified Herdr 0.8.2, non-root user. |
| `docker/selfcheck.sh`, `docker/lib.sh` | The in-container isolation proof and shared mount audit. |
| `docker/gates.sh` | Gate sequence with named first failure and e2e evidence export. |
| `docker/prepare.sh` | Dependency fetch + `board` build (network-enabled step). |
| `docker/env-entrypoint.sh` | Persistent environment container (Herdr server). |
| `docker/agent-prepare.sh`, `docker/agent-entrypoint.sh`, `docker/agent-run.sh`, `docker/agy-pin.txt` | Real-provider agent mode: pinned CLI install (SHA-verified agy tarball), credential symlinking + fail-closed integration preflight, and the in-container one-shot/seed runner. |
| `docker/lock.sh` | Lockfile regeneration through a single-file write-back mount. |
| `scripts/tests/test_sandbox.py` | Daemon-free contract tests: arguments, isolation defaults, pinning, cleanup, refusals. |
