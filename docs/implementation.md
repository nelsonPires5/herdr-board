# Implementation plan & conventions — CONTRACT for all build agents

## Crate layout (cargo workspace, Rust edition 2021, stable toolchain)

```
Cargo.toml                  # workspace; [workspace.dependencies] pins shared deps
crates/
  board-core/    # OWNED BY PHASE A. models, protocol types, db(rusqlite)+migrations,
                 # column engine (pure), prompt assembly, harness adapters, config,
                 # blocking NDJSON client (used by CLI + TUI)
  board-herdr/   # OWNED BY PHASE B. herdr socket client: envelope, typed calls
                 # (workspace/tab/agent/pane/worktree/notification), events stream
  board-tui/     # OWNED BY PHASE C. ratatui app (lib with run() entry)
  board-daemon/  # OWNED BY PHASE D. boardd server (lib with run() entry)
  board-cli/     # OWNED BY PHASE D. the single `board` binary: clap subcommands
                 # tui/daemon/card/column/comment/done/move/cancel/retry/status
```

Ownership is strict: an agent only edits its crate(s) + may append to `[workspace.dependencies]`
in root Cargo.toml. Never edit another crate. Phase A creates all five crates compiling
(stubs for B/C/D).

## Shared dependencies (workspace-pinned by Phase A)

serde, serde_json, rusqlite (bundled), uuid (v4), clap (derive), anyhow, thiserror,
tokio (daemon only: rt-multi-thread, net, sync, time, process, signal),
ratatui + crossterm (tui), tui-textarea (tui), insta (dev, tui), tempfile (dev),
directories (paths), tracing + tracing-subscriber (daemon logs), libc (daemonize) or nix.

## Key traits (defined in board-core by Phase A — B/C/D implement/consume)

```rust
// board-core::client — blocking NDJSON client over UnixStream (TUI + CLI use it).
pub trait BoardClient { fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value>; }
// + typed convenience wrappers mirroring docs/protocol.md, and subscribe() -> impl Iterator<Item=Event>.
// Provide `FakeBoardClient` (in-memory board state) behind #[cfg(feature="fake-client")] for TUI tests.

// board-core::spawn — how the daemon launches agent processes.
pub struct SpawnReq { pub name: String, pub cwd: Option<PathBuf>, pub workspace_ref: Option<String>,
                      pub env: Vec<(String,String)>, pub argv: Vec<String> }
pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &SpawnReq) -> Result<SpawnHandle>;   // handle: pane/workspace ids or pid
    fn kill(&self, h: &SpawnHandle) -> Result<()>;
    fn is_alive(&self, h: &SpawnHandle) -> Result<bool>;
}
// Phase D implements HerdrSpawner (via board-herdr) AND LocalSpawner (plain child process,
// used by integration tests with the fake harness — no herdr needed).
```

## Semantics source of truth

`docs/protocol.md` + `docs/design.md` §5–§8. `schema.sql` at repo root is the current fresh schema
(embedded and versioned with `PRAGMA user_version`). Schema v8 is current: it adds the partial
unique index `idx_runs_one_open_per_card` and transactional enqueue/promotion/finalization units of
work. Upgrade retains a single open run unchanged and rejects ambiguous duplicates with every card
and run ID; no duplicate is normalized or selected as a winner. It retains v5's preserved board id=1
as `Global` (`scope_path=NULL`) and scoped-board rows, v6's `awaiting`/`done` status invariants, and
v7's nullable `runs.system_prompt_snapshot`. New v7 queued runs store the exact
resolved, trailer-inclusive system prompt; pre-v7 rows remain `NULL` with no backfill. That legacy
`NULL` is intentional: built-ins keep their persisted all-in-one argv, while configured rows keep
their historical spawn-time reconstruction. The internal snapshot is omitted from boardd wire
responses. Every canonical-path board independently seeds one manual `Todo` column.

The completion race is harness-specific: `RunDoneParams.run_id` is optional so manual/TUI
completion remains compatible, while the CLI forwards `BOARD_RUN_ID` when present. An immediate
configured-harness `board done` may finalize only its exact queued run before runner registration;
a queued built-in Pi/Claude run is rejected until its managed pane is registered. A supplied
mismatched id is rejected, preventing stale children from completing replacement runs. The
Herdr-neutral eligibility and finalizer policy is centralized in
`board_core::engine::{LifecycleDecision, FinalizePlan}`; boardd remains responsible for gathering
facts and applying DB/process/events effects.

## Conventions

- `cargo fmt` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` green
  before an agent reports done. No `unwrap()` outside tests; anyhow at edges, thiserror in core.
- No `Date.now`-style flakiness in tests: inject clocks where needed (engine takes `now: i64`).
- Paths via `directories::BaseDirs` + env overrides (`BOARD_DB`, `BOARD_SOCKET`).
- Log with tracing; daemon writes `~/.local/share/herdr-board/daemon.log`.
- Commit nothing; leave the tree for review.

## Phase order

A (core+scaffold) → B (herdr client) ∥ C (TUI) → D (daemon+CLI+integration tests) → E (packaging/skill/e2e).

## Testing per phase

- A: unit tests: engine transitions (ok/fail/no-target/manual entry), lifecycle decisions
  (run identity, queued harness eligibility, cancel/timeout/pane-exit plans, auto-hop guard,
  resumability evidence), prompt assembly (with/without comments, truncation to last 20), adapter
  argv building (session mint/resume/fork; bypass refusal; overrides), migrations idempotent,
  config parsing.
- B: unit: envelope encode/decode. Integration (ignored-by-default `#[ignore]` + run when HERDR_SOCK exists): read-only calls `session.snapshot`, `workspace.list` against live herdr.
- C: insta snapshots via `ratatui::backend::TestBackend` + synthetic key events + FakeBoardClient: empty board (Todo only + hints), board with example pipeline & cards (status glyphs), new-card modal, column form, card detail w/ comments+runs, `?` help, delete-column prompt, move flow.
- D: integration test (no herdr): start daemon on temp socket + temp DB with LocalSpawner + fake harness script → create card → move to auto column → fake agent comments + done → assert auto-transition, comments, run rows, statuses; timeout path; cancel path; queue serialization (two cards same space key run serially).
- E: `scripts/e2e.sh` (real herdr, fake harness): disposable workspace, drive TUI via `herdr pane send-keys`, assert via `pane read` + sqlite3.
