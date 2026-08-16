# Contributing

Thanks for your interest in herdr-board! Bug reports, feature requests, and pull requests are
welcome. For the full cross-agent contributor guide (crate ownership, herdr gotchas), see
[`AGENTS.md`](AGENTS.md); the user-facing docs live in [`docs/`](docs/README.md).

## Development setup

Requirements: a **Rust toolchain** (stable, edition 2021) and exactly **Herdr 0.8.0 with
socket protocol 19** on `PATH` for the end-to-end path (unit and integration tests need neither
herdr nor an agent harness).

```bash
git clone https://github.com/nelsonPires5/herdr-board
cd herdr-board
cargo build            # or ./scripts/build.sh for the release binary herdr's [[build]] step runs
cargo run -p board-cli -- tui   # run the board locally
```

## Architecture

One `board` binary, five workspace crates: `board-core` (models, protocol, database, engine,
prompts, config, harness adapters), `board-daemon` (orchestration and dispatch), `board-herdr`
(Herdr socket client), `board-tui` (Ratatui application), and `board-cli` (the `board` binary).

| Role | Responsibility |
|---|---|
| `board daemon` | Owns SQLite state, run queue, orchestration, workspace resolution, pane spawning, and status watching. |
| `board tui` | Ratatui board opened inside a Herdr overlay/tab; talks to and auto-starts the daemon. |
| `board <verb>` | CLI used by humans and dispatched agents (`comment`, `done`, `move`, and others). |

The CLI and TUI share the typed `board_core::client::BoardClient`; only boardd touches SQLite.
Design and protocol: [`docs/design.md`](docs/design.md) and [`docs/protocol.md`](docs/protocol.md);
index: [`docs/README.md`](docs/README.md). `schema.sql` is the SQLite migration source of truth;
`scripts/` holds build/install helpers; `e2e/` holds scenarios 01–36 against disposable Herdr
sessions and workspaces.

## Gates that must pass

Keep this tier green before opening a PR. The gate list has one maintained copy:
**[`docs/README.md` → Test gates](docs/README.md#test-gates-single-source)** — the same commands
CI runs, kept in sync by `scripts/tests/test_docs.py`.

- No `unwrap()` outside tests; `anyhow` at edges, `thiserror` in core.
- Tests must be hermetic and deterministic — inject clocks/paths, no wall-clock timing.

## Commit style

**[Conventional Commits](https://www.conventionalcommits.org/)**, grouped by crate/intent as in the
git log: `feat(core): …`, `feat(daemon,cli): …`, `feat(tui): …`, `docs: …`, `fix: …`, `test: …`.

## Testing

See [`docs/testing.md`](docs/testing.md) for the full pyramid (unit → daemon/CLI integration → TUI
snapshots → live e2e) and how to add a test. **Policy:** write the failing unit test first for a
behavior change, and add a live e2e scenario for any new herdr-touching flow (trivial doc/typo
changes are exempt); keep the gates and `e2e/run-all.sh` green.

- **Unit + integration:** `cargo test --workspace --all-features`. The daemon integration tests use
  `LocalSpawner` + a fake harness script, so they run without a live herdr.
- **End-to-end:** `e2e/run-all.sh` (compat wrapper: `scripts/e2e.sh`) drives a REAL herdr
  with a scenario suite on **disposable** workspaces and an isolated temp DB + socket, tearing down
  on exit. Not part of CI. Read `docs/testing.md` first; never aim it at a workspace you care about.

## Adding a harness adapter

Harnesses are pluggable behind a `HarnessAdapter`. To add one:

- Model its capabilities in `crates/board-core/src/capability.rs` (models, efforts, permission
  modes — what `board harness models|efforts|permissions` surfaces).
- Implement the argv/prompt/session behavior in `crates/board-core/src/harness.rs` (session
  mint/resume/fork, model/effort/permission flags, prompt delivery). A config-defined harness
  (`[harness.NAME]` in `config.toml`, prompt via `$BOARD_PROMPT`) is the zero-code path.
- Add unit tests for argv building (mint/resume/fork, override precedence, permission handling).

## PR expectations

- One focused change per PR. Update the docs and `CHANGELOG.md` (`[Unreleased]`) in the same PR as a
  user-facing change — a change isn't done until the docs match it.
- Branch targeting: feature PRs open against **`dev`** (the long-lived integration branch); only
  hotfixes open against **`main`** (production) — the `dev -> main` promotion is opened and merged
  by the Promote workflow.
- Release policy: [`docs/releasing.md`](docs/releasing.md) for the Prepare Release → promote → CI-green
  tag flow.
- The gates above pass. Reference the design in `docs/design.md` / the contract in `docs/protocol.md`
  when a change touches behavior or the wire.
