# Contributing

Thanks for your interest in herdr-board! Bug reports, feature requests, and pull requests are
welcome. For the full cross-agent contributor guide (crate ownership, herdr gotchas), see
[`AGENTS.md`](AGENTS.md); the user-facing docs live in [`docs/`](docs/README.md).

## Development setup

Requirements: a **Rust toolchain** (stable, edition 2021) and **herdr 0.7.3+** on `PATH` for the
end-to-end path (unit and integration tests need neither herdr nor an agent harness).

```bash
git clone https://github.com/nelsonPires5/herdr-board
cd herdr-board
cargo build            # or ./scripts/build.sh for the release binary herdr's [[build]] step runs
cargo run -p board-cli -- tui   # run the board locally
```

## Gates that must pass

Keep this tier green before opening a PR:

```bash
cargo test --workspace --all-features        # unit + integration (LocalSpawner + fake harness; no herdr)
cargo clippy --all-targets -- -D warnings     # zero warnings
cargo fmt --all --check                       # formatted
```

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
- The gates above pass. Reference the design in `docs/design.md` / the contract in `docs/protocol.md`
  when a change touches behavior or the wire.
