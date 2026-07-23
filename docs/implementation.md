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

## Configuration boundary

`board-core::config::RootConfig` owns the complete typed TOML document. Board
settings stay at the root for compatibility; daemon settings are the typed
`[daemon]` table (`SpawnerKind`, timeout-unit, and polling/tick defaults).
`RootConfig::load` is the only file parse at daemon startup. Missing files and
sections use defaults, but malformed existing files are fatal `Error::Config`
errors. `board-daemon` applies injected environment overrides after parsing,
with environment values taking precedence, and does not run a second
best-effort parser or substitute defaults on failure.

## Shared dependencies (workspace-pinned by Phase A)

serde, serde_json, rusqlite (bundled), uuid (v4), clap (derive), anyhow, thiserror,
tokio (daemon only: rt-multi-thread, net, sync, time, process, signal),
ratatui + crossterm (tui), tui-textarea (tui), insta (dev, tui), tempfile (dev),
directories (paths), tracing + tracing-subscriber (daemon logs), libc (daemonize) or nix.

## Key traits (defined in board-core by Phase A — B/C/D implement/consume)

```rust
// board-core::client — blocking NDJSON client over UnixStream (TUI + CLI use it).
pub trait BoardClient {
    // The only raw transport primitive; typed wrappers are default methods.
    fn call(&mut self, method: &str, params: serde_json::Value)
        -> anyhow::Result<serde_json::Value>;
    fn subscribe(&mut self)
        -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>>;
    // Typed wrappers mirror docs/protocol.md and decode every result DTO.
}
// Provide `FakeBoardClient` (in-memory board state) behind #[cfg(feature="fake-client")] for TUI tests.
// CLI/TUI clients use these wrappers and never perform DB I/O.

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
(embedded and versioned with `PRAGMA user_version`). Schema v11 is current. v8 adds the partial
unique index `idx_runs_one_open_per_card` and transactional enqueue/promotion/finalization units of
work. v9 adds nullable durable timeout deadline/pause timestamps. Promotion writes the deadline in
its transaction; awaiting pause/resume updates the card and timeout atomically and idempotently,
using saturating shifts. Upgrade derives legacy open-run values once from `runs.started_at`, the
column timeout, and (for awaiting) `cards.updated_at`; restart consumes the persisted budget. Upgrade retains a single open run unchanged and rejects ambiguous duplicates with every card
and run ID; no duplicate is normalized or selected as a winner. It retains v5's preserved board id=1
as `Global` (`scope_path=NULL`) and scoped-board rows, v6's `awaiting`/`done` status invariants, and
v7's nullable `runs.system_prompt_snapshot`. New v7 queued runs store the exact resolved,
trailer-inclusive system prompt; pre-v7 rows remain `NULL` with no backfill. v10 adds partial
FIFO-queued and active-open run indexes; daemon queue reads use direct SQL pairs instead of scanning
every card's run history. v11 adds nullable `runs.launch_spec_json`: v10 rows remain NULL, while new
runs persist a version-1 tagged materialization of exact argv, env, managed prompt channels, and the
run's Herdr session. Unsupported spec versions fail decoding. Dispatch consumes `runs.session` for
v11 placement; pre-v11 rows explicitly retain current-card session lookup. The launch spec and
system snapshot are both private DB state omitted from board wire DTOs. The typed `SpaceKey`
preserves session/kind/ref null identity. A
per-daemon async pass lock prevents competing passes from duplicating claims; each pass claims
per-space/global slots before concurrently launching independent spaces. That legacy
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
facts and applying one `finalize_run_uow`. It prepares transition and auto-hop inputs before that
transaction; committed DTOs are the only source for post-commit bookkeeping and effects. The fixed
post-commit order is scheduler bookkeeping, watch refresh, kill, notification scheduling, terminal
events, then dispatch wake. The scheduler→store critical section provides transient mutual exclusion;
there is no durable or in-memory `finalizing_cards` source of truth. No socket, process, notification,
or other external I/O occurs inside the SQLite transaction.

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
- Restart recovery (`board-daemon::supervisor`) is a conservative one-pass classifier. Session resolution and snapshot I/O are injectable and happen before mutation. `Alive` adopts scheduler/watch intent and replays terminal status, `Gone` uses the existing pane-exit finalizer, and `Unknown` does nothing. The apply phase re-reads the open run/card, making duplicate passes idempotent and rejecting stale observations. Startup constructs/runs this pass for the Herdr spawner regardless of whether its initial best-effort client connected. The always-on supervisor then maintains independent per-socket streams and backoff, subscribes before taking a fresh bounded snapshot, and periodically reconciles missed events without resetting healthy sockets.
- D: integration test (no herdr): start daemon on temp socket + temp DB with LocalSpawner + fake harness script → create card → move to auto column → fake agent comments + done → assert auto-transition, comments, run rows, statuses; timeout path; cancel path; queue serialization (two cards same space key run serially).
- E: `scripts/e2e.sh` (real herdr, fake harness): disposable workspace, drive TUI via `herdr pane send-keys`, assert via `pane read` + sqlite3.
