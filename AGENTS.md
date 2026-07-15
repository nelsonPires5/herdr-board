# AGENTS.md

Cross-agent contributor guide for herdr-board. Read this before touching the repo. herdr-board is a
kanban board that dispatches AI coding agents into visible herdr panes; the single `board` binary is
TUI + daemon + CLI. Rust, cargo workspace, edition 2021, all crates `0.1.0`.

## Workspace layout & crate ownership

| Crate | Owns | Never leaks into |
|---|---|---|
| `board-core` | models, `board-core::protocol` types, SQLite db + migrations, the pure column engine, prompt assembly, harness adapters, config, the blocking boardd client | herdr/tokio/ratatui specifics |
| `board-herdr` | the herdr unix-socket client (envelope, typed calls, event stream) | board state |
| `board-tui` | the ratatui app (`run()` entry), forms, snapshot tests | daemon logic |
| `board-daemon` | boardd server: run queue, dispatch, per-session herdr clients, watchers, spawner | — |
| `board-cli` | the `board` binary: clap subcommands wiring the above | business logic |

Ownership is strict: edit your crate(s) + append to root `[workspace.dependencies]`. Semantics
source of truth: `docs/protocol.md` + `docs/design.md`. Docs live in `docs/` (index: `docs/README.md`);
`schema.sql` at the root is the migration source of truth.

## Build / test gates (keep green)

```bash
cargo test --workspace --all-features       # unit + integration; no live herdr needed
cargo clippy --all-targets -- -D warnings    # zero warnings
cargo fmt --all --check                      # formatted
```

- `#[ignore]`'d tests hit a live herdr (run only when `HERDR_SOCK`/`HERDR_SOCKET_PATH` exists).
- End-to-end: `e2e/run-all.sh` (compat: `scripts/e2e.sh`) drives a REAL herdr with a scenario
  suite. It boots its own **ephemeral** herdr session (`hb-e2e-<pid>`) per run and never touches your
  real sessions; each scenario uses an isolated temp DB + socket, creates **disposable** workspaces,
  prefixes every herdr mutation `HERDR MUTATION:`, and tears everything down on exit (`--keep` leaves
  sessions/workspaces for review). See [`docs/testing.md`](docs/testing.md) for the layers and how to add one.

## Testing policy (pragmatic)

Full layering, harness details, and how to add tests live in [`docs/testing.md`](docs/testing.md).

- **Test-first for behavior.** For any behavior change, write the failing unit test first
  (red→green) in the owning crate's existing test style (`crates/<crate>/tests/`).
- **New herdr-touching flow ⇒ e2e.** Any new user-visible flow that reaches herdr isn't done until
  it has a use case documented and a live scenario under `e2e/` (per `docs/testing.md` and
  `e2e/README.md`).
- **Trivial changes are exempt** — docs, comments, typos, pure renames need no new test.
- **Green before handoff.** The gates above **and** `e2e/run-all.sh` must pass (all scenarios
  PASS — the suite boots its own ephemeral session(s), so 03-sessions no longer skips) before
  handing a change off.

## Conventions

- `anyhow` at edges, `thiserror` in core. No `unwrap()` outside tests.
- Inject clocks/paths — the engine takes `now: i64`; paths via `directories` + env overrides
  (`BOARD_DB`, `BOARD_SOCKET`). No wall-clock flakiness in tests.
- Commit style: **Conventional Commits** grouped by crate/intent, as in the git log —
  `feat(core): …`, `feat(daemon,cli): …`, `docs: …`.
- The daemon opens a **fresh herdr connection per operation** (`HerdrClient::connect` in
  `dispatch.rs`/`ops.rs`/`spawner.rs`); one `HerdrClient` = one request/response connection, event
  streaming lives on its own connection.
- Definition of done for a user-facing change: update the docs and `CHANGELOG.md` in the same change.

## herdr gotchas (field-tested)

**Learning/verifying herdr is its own page.** herdr has no man page; the authoritative
sources are the installed binary itself — `herdr api schema --json` (methods/types/events +
protocol number), `herdr <cmd> --help`, `herdr api snapshot`. Never assume a herdr command,
flag, or JSON shape from memory, and pin the argv you verified in a test comment. Repo herdr
facts are pinned to **herdr 0.7.3 / protocol 16**; on a newer herdr, re-verify against
`api schema` **before** patching code. **See [`docs/herdr.md`](docs/herdr.md).**

- **Never run destructive herdr commands against a user's workspaces/sessions.** Mutations only
  against disposable workspaces you created (see `e2e/`). Read-only probes otherwise.
- **Agent names are exclusive** while a pane is open. Names are `card-<id>-<column-slug>`; on an
  `agent_name_taken` collision the daemon retries with the `-r<run>` fallback.
- **Panes don't inherit the workspace's env/cwd.** `agent.start` needs cwd + env passed explicitly;
  workspace cwd is read from the workspace's pane snapshot.
- **Tab labels are not unique** in herdr — resolve the `kanban` tab by find-or-create on id, not label.
- **herdr events are a raw-socket stream** (`events.subscribe`, persistent connection); the CLI only
  has a blocking one-shot `events.wait`. Event fields: `pane_agent_status_changed` carries
  `{pane, workspace, agent, status}`; `idle ≠ finished` (done needs the explicit `board done` channel).
- **AF_UNIX paths cap at 108 chars.** Test DBs/sockets must live under a short `/tmp` dir
  (`tempfile::tempdir()`), not a deep nested path, or `connect` fails with a cryptic error.
