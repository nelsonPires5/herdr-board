# Implementation plan & conventions — CONTRACT for all build agents

## Crate layout (cargo workspace, Rust edition 2021, stable toolchain)

```
Cargo.toml                  # workspace; [workspace.dependencies] pins shared deps
crates/
  board-core/    # OWNED BY PHASE A. models, protocol types, db(rusqlite)+migrations,
                 # column engine (pure), prompt assembly, harness adapters, config,
                 # blocking NDJSON client (used by CLI + TUI)
  board-herdr/   # herdr socket client: envelope, typed workspace/tab/agent/pane/
                 # notification/session calls, events stream (no worktree API)
  board-tui/     # OWNED BY PHASE C. ratatui app (lib with run() entry)
  board-daemon/  # OWNED BY PHASE D. boardd server (lib with run() entry)
  board-cli/     # OWNED BY PHASE D. the single `board` binary: clap subcommands
                 # tui/daemon/board/template/card/column/comment/run + legacy aliases
```

Ownership is strict: an agent only edits its crate(s) + may append to `[workspace.dependencies]`
in root Cargo.toml. Never edit another crate. Phase A creates all five crates compiling
(stubs for B/C/D).

## Contract versions and source ownership

The final compatibility matrix is: board protocol **v1**, SQLite schema **v15**, and exactly
Herdr **0.8.0 / socket protocol 19**. The versioned source of truth is `schema.sql` for fresh
SQLite databases and `board-core::db` migrations for upgrades; `board-core::protocol` owns the
board wire DTOs; `board-herdr` owns only the verified Herdr socket surface; and `docs/design.md`
and `docs/protocol.md` explain behavior rather than defining duplicate serde shapes. Schema v15 adds
projects (canonical-path identity, Global as the special project), per-project named boards,
persistent selection, and capped recency on top of v13's soft-deleted comments and immutable
comment-history snapshots; the CLI exposes the project/board subcommands while boardd remains the
sole SQLite writer. The complete live use-case catalog
is [`../e2e/README.md`](../e2e/README.md), scenarios **01–38** (through `e2e/38-board-project-archive.sh`); the safe
static harness is `e2e/test-harness.sh`, while `e2e/run-all.sh` is the opt-in live gate.

The current Herdr boundary is deliberately narrower than the upstream schema. The 0.8.0/protocol-19
fixture adds `workspace.move_block` and the `workspace.reordered` event/subscription for
atomic workspace/worktree-group reordering, plus the `antigravity_cli` and `grok` integration
targets. Those are additive and are not board dependencies;
the existing workspace, tab, pane, agent, notification, session, and event calls retain their
checked request/result shapes. Re-verify any future change with `herdr api schema --json` before
changing `board-herdr`.

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
directories (paths), tracing + tracing-subscriber (daemon logs), libc or nix for platform primitives.

## Key boundaries

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

// board-core::launch — durable, runtime-neutral enqueue materialization.
pub struct ExecutionSpec { /* exact argv, env, and managed prompt inputs */ }
pub struct RunLaunchSpec { /* independently versioned durable ExecutionSpec */ }

// board-daemon::spawner — runtime launch ownership.
pub struct HerdrLaunchPlan { /* placement, socket, cwd, and execution inputs */ }
pub struct RuntimeHandle { /* pane/workspace ids or local pid */ }
pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &HerdrLaunchPlan) -> Result<RuntimeHandle>;
    fn kill(&self, h: &RuntimeHandle) -> Result<()>;
    fn is_alive(&self, h: &RuntimeHandle) -> Result<bool>;
}
// board-daemon implements HerdrSpawner (via board-herdr) and LocalSpawner (plain child process,
// used by integration tests with the fake harness — no herdr needed). Runtime placement,
// process handles, liveness, and cleanup never belong to board-core.

// board-daemon::spawner::rescue — placement + launch WITHOUT run promotion, for
// reopening a run whose pane is gone (`run.focus`). It shares the placement and
// launch helpers with `Spawner::spawn` instead of duplicating them, but calls no
// unit of work: a rescue performs zero DB writes, so its pane has no run row and
// is neither owned, watched, nor timed out. `board_core::harness::resume_invocation`
// re-threads the run's persisted ExecutionSpec onto SessionPlan::Resume (per-harness
// syntax owned by `session_argv`) and clears every prompt channel.
pub(crate) fn rescue_run_pane(plan: &RescuePlan<'_>) -> Result<RescueOutcome>;
```

## Semantics source of truth

`docs/protocol.md` + `docs/design.md` §5–§8. `schema.sql` at repo root is the current fresh schema
(embedded and versioned with `PRAGMA user_version`). Schema v14 is current. v8 adds the partial
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
run's Herdr session. Unsupported spec versions fail decoding. Current placement consumes
`runs.session`; pre-v11 rows explicitly retain current-card session lookup. Consecutive managed
resume hops whose durable conversation id, session, workspace, tab, pane, and agent kind still match
reuse that live child: placement protects it from reclaim, waits for protocol-19 `Idle` or derived
`Done`, skips `pane.split`/`agent.start`, and sends only the new run's exact `initial_prompt` through
`agent.prompt`. Mint/fork (including fresh columns and retries) retain new-pane startup, and a manual
landing leaves the last pane open. v12 adds durable anchor identity, while v13 adds
`comments.deleted_at`, `comment_history`, and its immutable insert-audit
trigger. v14 introduces `projects` (canonical-path identity, Global as project `id=1` with
`scope_path NULL`), rebuilds `boards` with `project_id` + per-project case-insensitive unique
names (every project's first board is `main`), and adds the `selection`, `board_selection`,
`project_recents`, and `board_recents` tables (recency capped at 3). The v13→v14 migration runs
outside the transaction with foreign keys disabled, moving every existing board into its project
as `main` so all ids/cards/columns/runs/history are preserved. The launch spec and system snapshot are both private DB state omitted from board wire DTOs;
comment history is exposed only through `comment.history`. The typed `SpaceKey` preserves
session/kind/ref null identity. A per-daemon async pass lock prevents competing passes from duplicating
claims; each pass claims
per-space/global slots before concurrently launching independent spaces. That legacy
`NULL` is intentional: built-ins keep their persisted all-in-one argv, while configured rows keep
their historical spawn-time reconstruction. The internal snapshot is omitted from boardd wire
responses. Every canonical-path board independently seeds one manual `Todo` column.

The completion race is harness-specific: `RunDoneParams.run_id` is optional so manual/TUI
completion remains compatible, while the CLI forwards `BOARD_RUN_ID` when present. An immediate
configured-harness `board done` may finalize only its exact queued run before runner registration;
a queued built-in (Pi/Claude/Codex/OpenCode/Antigravity) run is rejected until its managed pane is registered. A supplied
mismatched id is rejected unless `actor_pane_id` exactly identifies the managed pane shared with the
current open resume run; that pane credential resolves the process's immutable first-stage
`BOARD_RUN_ID` to the current stage. A different/missing pane never grants that remap. The
Herdr-neutral eligibility and finalizer policy is centralized in
`board_core::engine::{LifecycleDecision, FinalizePlan}`; boardd remains responsible for gathering
facts and applying one `finalize_run_uow`. It prepares transition and auto-hop inputs before that
transaction; committed DTOs are the only source for post-commit bookkeeping and effects. The fixed
post-commit order is scheduler bookkeeping, watch refresh, kill, notification scheduling, terminal
events, then dispatch wake. The scheduler→store critical section provides transient mutual exclusion;
there is no durable or in-memory `finalizing_cards` source of truth. No socket, process, notification,
or other external I/O occurs inside the SQLite transaction.

## Conventions

- `cargo fmt` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` green, and
  `python3 -m unittest discover -s scripts/tests -p 'test_*.py'` green (the docs/release contract
  tier CI also runs) before an agent reports done. No `unwrap()` outside tests; anyhow at edges,
  thiserror in core.
- No `Date.now`-style flakiness in tests: inject clocks where needed (engine takes `now: i64`).
- Paths via `directories::BaseDirs` + env overrides (`BOARD_DB`, `BOARD_SOCKET`, `BOARD_LOG_DIR`).
- `board-daemon::logging` owns private daily NDJSON, startup/periodic exact-prefix retention, and
  foreground mirroring. `board-herdr` emits metadata-only call/subscription completions into that
  subscriber; neither transport records payloads.
- Commit nothing; leave the tree for review.

## Phase order

A (core+scaffold) → B (herdr client) ∥ C (TUI) → D (daemon+CLI+integration tests) → E (packaging/skill/e2e).

## Testing per phase

- A: unit tests: engine transitions (ok/fail/no-target/manual entry), lifecycle decisions
  (run identity, queued harness eligibility, cancel/timeout/pane-exit plans, auto-hop guard,
  resumability evidence), prompt assembly (with/without comments, truncation to last 20), adapter
  argv building (session mint/resume/fork; bypass refusal; overrides), migrations idempotent,
  config parsing.
- B: unit: envelope encode/decode. Integration (ignored-by-default `#[ignore]` + run when HERDR_SOCK exists): read-only calls `session.snapshot`, `workspace.list` against a Herdr 0.8.0 / protocol-19 live socket.
- C: insta snapshots via `ratatui::backend::TestBackend` + synthetic key events + FakeBoardClient: empty board (Todo only + hints), board with example pipeline & cards (status glyphs), new-card modal, column form, card detail w/ comments+runs, `?` help, delete-column prompt, move flow.
- Restart recovery (`board-daemon::supervisor`) is a conservative one-pass classifier. Session resolution and snapshot I/O are injectable and happen before mutation. `Alive` adopts scheduler/watch intent and replays terminal status, `Gone` uses the existing pane-exit finalizer, and `Unknown` does nothing. The apply phase re-reads the open run/card, making duplicate passes idempotent and rejecting stale observations. Startup constructs/runs this pass for the Herdr spawner regardless of whether its initial best-effort client connected. The always-on supervisor then maintains independent per-socket streams and backoff, subscribes before taking a fresh bounded snapshot, and periodically reconciles missed events without resetting healthy sockets.
- D: integration test (no herdr): start daemon on temp socket + temp DB with LocalSpawner + fake harness script → create card → move to auto column → fake agent comments + done → assert auto-transition, comments, run rows, statuses; timeout path; cancel path; queue serialization (two cards same space key run serially). The daemon comment suite also checks actor ownership, system-comment immutability, soft deletion, audit history, and event routing.
- E: scenarios `e2e/01-core.sh` through `e2e/37-multi-project.sh` (real Herdr 0.8.0 / socket
  protocol 19, fake harnesses): disposable workspaces, pane-first placement, typed prompt delivery,
  bounded same-pane `agent_pane_busy` retry, supervisor recovery, timer refresh, and
  identity-gated cleanup. The managed fixtures use Pi integration v8 and Claude integration v7
  when exercising precise lifecycle/session signals; the configured fixture remains unmanaged.
  CLI comment creation/context and system transition comments are covered by the live suite;
  CRUD/audit parity is kept hermetic in the CLI contracts. Run `bash e2e/test-harness.sh` for the
  provider-free static safety checks; reserve `e2e/run-all.sh` for the separate live gate.
