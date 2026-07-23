# AGENTS.md

Cross-agent contributor guide for herdr-board. Read this before touching the repo. herdr-board is a
kanban board that dispatches AI coding agents into visible herdr panes; the single `board` binary is
TUI + daemon + CLI. Rust, cargo workspace, edition 2021, all crates share the workspace version.

## Workspace layout & crate ownership

| Crate | Owns | Never leaks into |
|---|---|---|
| `board-core` | models, `board-core::protocol` types, SQLite db + migrations, the pure column engine, prompt assembly, harness adapters, config, the blocking boardd client | herdr/tokio/ratatui specifics |
| `board-herdr` | the Herdr unix-socket client (envelope, typed workspace/tab/agent/pane/notification/session calls, event stream) | board state; no worktree API |
| `board-tui` | the ratatui app (`run()` entry), forms, snapshot tests | daemon logic |
| `board-daemon` | boardd server: run queue, dispatch, per-session herdr clients, watchers, spawner | — |
| `board-cli` | the `board` binary: clap subcommands wiring the above | business logic |

Ownership is strict: edit your crate(s) + append to root `[workspace.dependencies]`. Semantics
source of truth: `docs/protocol.md` + `docs/design.md`. Docs live in `docs/` (index: `docs/README.md`);
`schema.sql` is the fresh-schema source of truth and `board-core::db` owns upgrades. Final compatibility
is board protocol v1, SQLite schema v11, and exactly Herdr 0.7.5 / socket protocol 17. The complete
live catalog is `e2e/README.md` (scenarios 01–21); `e2e/test-harness.sh` is the provider-free static
safety gate.

## Build / test gates (keep green)

```bash
cargo test --workspace --all-features       # unit + integration; no live herdr needed
cargo clippy --all-targets -- -D warnings    # zero warnings
cargo fmt --all --check                      # formatted
```

- `#[ignore]`'d tests hit a live herdr (run only when `HERDR_SOCK`/`HERDR_SOCKET_PATH` exists).
- End-to-end: `e2e/run-all.sh` (compat: `scripts/e2e.sh`) drives a REAL Herdr with a scenario
  suite, but checked-in fake Pi/Claude executables keep the standard suite provider-free and
  zero-cost. Every scenario boots its own collision-resistant **ephemeral** Herdr session
  (`hb-e2e-<scenario>-<pid>-<random64>`, bounded slug) and never touches or adopts
  your real sessions; each uses a marker-gated mode-0700 short HOME with explicit AF_UNIX margin,
  an isolated temp DB + socket, and **disposable**
  marked workspaces, prefixes every Herdr mutation `HERDR MUTATION:`, and tears everything down on exit
  (`--keep` leaves sessions/workspaces for review). The forced-build standard suite passes 01–21
  provider-free under a mode-0700 root with controlled HOME/ZDOT/rc/PATH, never sourcing user rc;
  Herdr and Bash >=4 are resolved absolutely before PATH narrowing. The standard suite supports Linux and macOS: process identity uses Linux `/proc` or Darwin `libproc` + `KERN_PROCARGS2`, and every token is HMAC-bound to a non-exported per-invocation key delivered to the verifier over an inherited file descriptor. Linux additionally rechecks the owner token in `/proc/environ`; Darwin requires a signed direct-child capability before the exact PID/start/executable/complete-argv transition can be adopted. Session mutation, board-daemon signals, workspace close, and session stop/delete are never authorized by PID liveness alone; scenario mutation wrappers freshly verify boardd and each primary/secondary target. Stop and post-stop delete have separate fail-closed
  authorization, with delete requiring the exact private ownership marker. The opt-in real-Claude smoke retains its independent Linux-only identity implementation and is not part of the portable provider-free gate. Cleanup is limited to invocation-emitted exact names/roots/PIDs; marker and script-content digests are reverified immediately before destructive cleanup, and inherited roots always fail closed (reuse is process-local with exact path/mode/header/token/owner validation). A post-spawn server is provisionally ledgered by PID/start/exe/argv/owner token before full capture and is signalled only after a fresh match,
  and failures propagate so a passing scenario cannot hide failed cleanup. Standard children use an
  environment allowlist that excludes inherited provider configuration. See [`docs/testing.md`](docs/testing.md)
  for the layers and how to add one.

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
  handing a change off. The configured runner's residual orphan-script limitation remains
  documented; it is not silently treated as a cleanup guarantee.

## Conventions

- `anyhow` at edges, `thiserror` in core. No `unwrap()` outside tests.
- Inject clocks/paths — the engine takes `now: i64`; paths via `directories` + env overrides
  (`BOARD_DB`, `BOARD_SOCKET`). No wall-clock flakiness in tests.
- Commit style: **Conventional Commits** grouped by crate/intent, as in the git log —
  `feat(core): …`, `feat(daemon,cli): …`, `docs: …`.
- The daemon opens a **fresh Herdr connection per operation** (`HerdrClient::connect` in
  `dispatch.rs`/`ops.rs`/`spawner.rs`); one `HerdrClient` = one request/response connection, event
  streaming lives on its own connection. Runtime launch ownership is daemon-only: `board-core`
  persists the neutral schema-v11 launch spec, while `board-daemon` owns placement, pane/process
  handles, liveness, cleanup, and the Herdr supervisor.
- `RootConfig` is parsed once at daemon startup; typed `[daemon]` settings are resolved before
  environment overrides, and malformed existing config is fatal. CLI and TUI use typed
  `board-core::client::BoardClient` wrappers rather than raw method/result handling.
- Auto-start creates one child process-group leader (no double-fork/`setsid`); stop is an exact
  socket/identity-gated operation. The active-run summary drives TUI timers, and the always-on
  per-session supervisor reconnects and reconciles conservatively.
- Definition of done for a user-facing change: update the docs and `CHANGELOG.md` in the same change.
- Release/version changes follow [`docs/releasing.md`](docs/releasing.md). Agents must never create,
  push, move, or delete release tags manually: a maintainer starts **Prepare Release**, merges its PR,
  and the **Release** workflow creates the tag only after `main` CI is green. No tag ruleset currently
  enforces this; it is repository policy.

## herdr gotchas (field-tested)

**Learning/verifying herdr is its own page.** herdr has no man page; the authoritative
sources are the installed binary itself — `herdr api schema --json` (methods/types/events +
protocol number), `herdr <cmd> --help`, `herdr api snapshot`. Never assume a herdr command,
flag, or JSON shape from memory, and pin the argv you verified in a test comment. Repo herdr
facts are pinned to exactly **Herdr 0.7.5 / protocol 17**. herdr-board intentionally rejects every
other Herdr version and protocol; re-verify against `api schema` before changing that gate or any
wire behavior. **See [`docs/herdr.md`](docs/herdr.md).**

- **Never run destructive herdr commands against a user's workspaces/sessions.** Mutations only
  against disposable workspaces you created (see `e2e/`). Read-only probes otherwise.
- **Agent names are exclusive** while a pane is open. Names are `card-<id>-<column-slug>`; on an
  `agent_name_taken` collision the daemon retries with the `-r<run>` fallback.
- **Panes don't inherit the workspace's env/cwd.** Protocol-17 launch is pane-first:
  `tab.create`/`pane.split` establishes cwd + env, then `agent.start` targets that pane with
  `{name, kind, pane_id, args}`. Workspace cwd is read from the workspace's pane snapshot.
- **Tab labels are not unique** in herdr — resolve the `kanban` tab by find-or-create on id, not label.
- **Herdr events are a raw-socket stream** (`events.subscribe`, persistent connection); the CLI only
  has a blocking one-shot `events.wait`. Protocol-17 `pane_agent_status_changed` carries pane,
  workspace, agent, and status fields; `idle ≠ finished`, and a trailing `idle` may follow `done`
  (completion still needs the explicit `board done` channel). Watcher identity is `(session socket,
  pane id)`, not pane id alone.
- **AF_UNIX paths cap at 108 chars.** Test DBs/sockets must live under a short `/tmp` dir
  (`tempfile::tempdir()`), not a deep nested path, or `connect` fails with a cryptic error.
