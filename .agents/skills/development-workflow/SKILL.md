---
name: development-workflow
description: Default sandbox-first developer workflow for herdr-board. Run every test gate and the live Herdr E2E suite through scripts/sandbox.sh in an isolated, network-disabled, non-root container (worktree read-only); never run cargo tests, the live E2E suite, the TUI, or provider smokes directly against the host Herdr/board. Covers the one-command edit-test loop, interactive shell/board CLI/TUI, the explicit opt-in real-provider smokes, artifact/cache housekeeping, and the visual-validation stage (references/visual-validation.md) for TUI work through the sandbox.
---

# Development workflow (sandbox-first)

Use this skill for any development work in a herdr-board worktree: editing code,
running tests, exercising the CLI/TUI, or shepherding a change to a PR. Running
tests on the host conflicts with the user's active Herdr, board daemon, sockets,
sessions, and workspaces — so this repository's default is the **Docker sandbox**
(`scripts/sandbox.sh`), which runs the complete deterministic gate set in a
disposable, network-disabled, non-root container with the worktree mounted
read-only. Supported hosts: Docker Engine on Linux, Colima on macOS, amd64 and
arm64.

The full reference is [`docs/sandbox.md`](../../docs/sandbox.md). This skill is
the thin policy + routing layer on top of it; it does not duplicate the guide.

## 1. Hard rules (never negotiate)

1. Every local test run goes through the sandbox:
   `./scripts/sandbox.sh gates`. Do NOT run `cargo test`, `e2e/run-all.sh`,
   `e2e/ci.sh`, the TUI, or provider smokes directly against the host.
2. The host is never a test or run target for this repository's own state:
   no host `BOARD_*`, `HERDR_*`, or provider variables are forwarded, no host
   Herdr/board sockets, sessions, workspaces, databases, the Docker socket, or
   user data directories are ever mounted (the wrapper is an explicit env
   allowlist with `--network none` in deterministic modes).
3. The worktree is mounted **read-only** at `/repo`; Cargo caches, build
   output, databases, sockets, logs, and artifacts live in per-worktree named
   volumes outside the repository. Build output goes to the `/repo/target`
   volume so the herdr plugin contract `./target/release/board` keeps working.
4. Real-provider smokes run **only** through the explicit opt-in mode
   (`smoke --provider X --allow-network`). Without both, the command fails
   before any container launches with a clear message.
5. Visual (TUI) work routes through the sandbox when Docker/Colima is
   available — see the **visual-validation stage** below. Host-isolated
   fallback exists only when no Docker/Colima is present.

If a task would violate a rule, stop and do the sandbox form instead; the
sandbox path is strictly safer and strictly equivalent in coverage.

## 2. One-command edit-test loop

```bash
./scripts/sandbox.sh prepare   # once per worktree: image, volumes, dependency fetch (only network step)
./scripts/sandbox.sh gates     # the full deterministic suite, offline
```

`gates` runs, in order, and **stops at the first failing gate, naming it**
(non-zero exit): safety self-check → `cargo fmt --all --check` → clippy
(`--workspace --all-targets --all-features -- -D warnings`) → workspace tests →
Python tests (`scripts/tests`) → static harness gate (`e2e/test-harness.sh`) →
all provider-free live Herdr E2E scenarios (`e2e/run-all.sh --require-all`,
which identifies any failing scenario and runs its own identity/cleanup
guards). The e2e run is teed to the artifacts volume and the suite's evidence
root is exported for `artifacts`.

Iterate on a subset of scenarios with filters (substring match, forwarded to
`run-all.sh`):

```bash
./scripts/sandbox.sh gates 03-sessions
./scripts/sandbox.sh gates 16- 17-
```

Edits on the host are visible on the next `gates` run **without any image
rebuild** (the worktree is a read-only bind mount). Repeated runs reuse the
volumes; the repository stays clean. If `Cargo.toml` changed and the lockfile
is stale, `prepare` refuses with a hint — run `./scripts/sandbox.sh lock` to
regenerate it through a single-file write-back mount.

## 3. Interactive shell, board CLI, TUI

```bash
./scripts/sandbox.sh shell                       # bash in the env container
./scripts/sandbox.sh board board list --json
./scripts/sandbox.sh board card list --json
./scripts/sandbox.sh board board create my-board
./scripts/sandbox.sh tui                         # real TUI, attached terminal
```

These commands start (or reuse) a persistent environment container that runs a
**container-local Herdr server**; the board daemon auto-starts on first use,
exactly like on a host. Everything in there is disposable: Herdr sessions,
workspaces, board database, and sockets live in the container and its state
volume. Stop it with `scripts/sandbox.sh down`.

## 4. Real-provider smoke tests (explicit opt-in only)

```bash
./scripts/sandbox.sh smoke --provider claude   --allow-network
./scripts/sandbox.sh smoke --provider codex    --allow-network
./scripts/sandbox.sh smoke --provider opencode --allow-network
```

This is the only mode with network access. It requires **both** `--provider`
and `--allow-network`; missing credentials or a missing opt-in fail **before**
any container launch, with a clear message. Only the chosen provider's
credential directory is mounted, read-only, at `/secrets` (respecting
`CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `XDG_*` overrides). Provider CLIs are
Linux binaries installed at smoke time into the state volume; credentials are
never copied into the image or logs. `pi` is refused pre-launch (the real Pi
smoke requires a WezTerm GUI on the host) and `antigravity` is refused (no
in-repo real smoke) — with the reasons printed; nothing silently skips.

## 5. Housekeeping

```bash
./scripts/sandbox.sh artifacts            # copy evidence out (default ~/.cache/herdr-board/sandbox-artifacts/<ts>)
./scripts/sandbox.sh artifacts ~/e2e-out  # or an explicit destination (never inside the repo)
./scripts/sandbox.sh selfcheck            # in-container isolation proof (also gate 0 of gates)
./scripts/sandbox.sh reset --target       # drop build output only
./scripts/sandbox.sh reset --all          # volumes + image for this worktree
./scripts/sandbox.sh lock                 # regenerate Cargo.lock (network, single-file write-back)
./scripts/sandbox.sh down                 # stop the environment container
./scripts/sandbox.sh doctor               # diagnose docker/sandbox resources
```

`reset` only ever touches resources prefixed `hb-sb-<worktree-slug>-…` (plus
the shared sandbox image tag). `artifacts` refuses to write inside the
repository. After any run, verify the host is untouched: `git status` clean
and no new sessions/sockets/db/artifacts under the host user's Herdr/board
directories (the sandbox never writes there).

## 6. Visual-validation stage

If the change affects the TUI's layout, styling, colors, forms, popups, card
states, keyboard/mouse behavior, or TUI snapshots, run the visual-validation
stage — **through the sandbox** when Docker/Colima is available:

**Read [`references/visual-validation.md`](references/visual-validation.md)** before any
TUI work. It gives two routes:

- **Route A (preferred) — through the sandbox:** seed fixtures with
  `scripts/sandbox.sh board …`, show the real TUI with `scripts/sandbox.sh tui`
  in a new WezTerm tab for the human to inspect (resize + clean exit), and run
  the deterministic fake-client snapshot suite inside `scripts/sandbox.sh
  gates`. Capture stays host-side (the pane is on the host), so
  `wezterm cli get-text --escapes` and `screencapture` keep working; a PTY +
  `pyte` capture (`references/pty-capture.py`) is the no-WezTerm-CLI fallback.
  Sandbox renders use a neutral 256-color palette: they prove geometry and
  emitted attributes, not the user's real palette or font — when the question
  is whether something *looks* right, hand the attached TUI to the human
  instead of claiming visual approval.
- **Route B (fallback) — host-isolated, only when Docker/Colima is
  unavailable:** the classic isolated flow (board DB/socket/daemon under a
  short `/tmp` dir + an ephemeral named Herdr session), described in detail in
  the reference. It still never touches the user's real DB, daemon, or
  sessions.

Either route must keep the skill's non-negotiables: isolated state only,
mutation-log every Herdr change (`HERDR MUTATION:`), clean up by exact
PID/socket owner (never broad `pkill`), and prove cleanup before handoff.

## 7. Handoff checklist

Before handing off a change that went through this workflow, confirm:

1. `scripts/sandbox.sh gates` is green (or the only failures are understood).
2. `git status --porcelain` is clean and the repo contains no build output,
   fixture databases, or artifact copies.
3. No host Herdr session/socket/workspace/board database was created, and
   `~/.config/herdr` / `~/.local/share/herdr-board` are unchanged.
4. Docs and `CHANGELOG.md` are updated in the same change (one `Unreleased`
   entry per PR, with the PR link, per repository rules).
5. Visual changes were shown to the human via the sandbox `tui` route (Route A)
   before approval.
6. Commits follow Conventional Commits (e.g. `feat(sandbox): …`,
   `docs(skills): …`) and the PR targets `dev`.
