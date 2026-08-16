# Testing

How herdr-board is tested, and how to add tests for a change. The final contract is board
protocol v1, SQLite schema v14, and Herdr 0.8.0 / protocol 19. Four layers, cheap and hermetic
first, expensive and live last:

```
unit / pure (per crate)                 no I/O, no daemon        — cargo test
  └─ daemon + CLI integration           real boardd socket,      — cargo test
        over a LocalSpawner                fake harness, no herdr
     └─ TUI fake-client + snapshots      ratatui TestBackend,     — cargo test
                                           in-memory client
        └─ live e2e scenarios           REAL herdr, disposable   — e2e/
                                           workspaces
```

CI is configured to run all four layers. The live job starts only after the cheaper gates pass,
installs the pinned Herdr 0.8.0 binary, and boots only suite-owned ephemeral servers — see
[Running](#running).

## The pyramid

### 1. Unit / pure tests (per crate)

Public API behavior belongs in the crate's `tests/` integration target, where the test is compiled
as an external client. Private invariants stay adjacent to the implementation under `src/` in a
`#[cfg(test)]` module; moving one of those tests must not require making an internal helper public.
This distinction is pragmatic rather than a requirement to force every test into one layer.

The stable ownership layout is responsibility-oriented, not a file manifest:

| Area | Boundary and representative coverage |
|---|---|
| `board-core` | Public engine, client, protocol, configuration, prompt, harness, and database behavior lives under `crates/board-core/tests/`. Database coverage is one binary, `tests/db.rs`, delegating to `tests/db/{migrations,crud,runs,atomic}.rs` over shared fixtures — `migrations.rs` owns every schema upgrade and `atomic.rs` owns rollback of the durable units of work. The production responsibilities are separated into `crates/board-core/src/engine/` (pure lifecycle, transitions, validation, signals, and column resolution) and `crates/board-core/src/client/` (traits, Unix transport, and fake client). |
| `board-daemon` | Queue lifecycle and launch behavior remain private daemon tests under `crates/board-daemon/src/dispatch/tests/` and `crates/board-daemon/src/spawner/tests/`; request operation tests live under `crates/board-daemon/src/ops/tests/`, and timeout/local/Herdr observation tests under `crates/board-daemon/src/watchers/tests/`. Their production owners are the corresponding responsibility directories. Each of those directories is split by *concern*, not by file size — `dispatch/tests/{atomicity,finalize,registration,ownership,space}.rs`, `spawner/tests/{card_tabs,managed,races,placement}.rs`, `ops/tests/{lifecycle,rollback}.rs`. Supervisor, server, session, settings, and snapshot tests remain adjacent to those smaller private modules, and all of them build on `crates/board-daemon/src/testkit.rs` (below). |
| `board-herdr` | Public envelope, event, and socket behavior is tested from `crates/board-herdr/tests/`, including ignored live probes. Event parsing, backoff, and stream handling are owned by `crates/board-herdr/src/events/`; all unsafe board-herdr Unix transport operations remain in `crates/board-herdr/src/transport.rs`. |
| `board-tui` | Public reducer, form, and rendering behavior is tested from `crates/board-tui/tests/` (`update.rs`, `forms.rs`, `layout.rs`, `mouse.rs`, and snapshots). Each of those declares `required-features = ["fake-client"]` in `Cargo.toml`, because they all link `crates/board-tui/src/testkit.rs`, which is compiled only under that feature; `help.rs` is the exception — it reads the handler *sources* as text to prove `view::HELP_KEYS` still lists every handled key, so it needs no client at all. The implementation is organized under `crates/board-tui/src/app/` (reducer, split into `state`/`effect`/`nav`/`drag` plus one module per screen), `crates/board-tui/src/driver/` (the effect loop), `crates/board-tui/src/forms/`, and `crates/board-tui/src/view/`; only view tests that need private rendering helpers remain adjacent in `crates/board-tui/src/view/tests.rs`. |

The public core suites cover deterministic engine decisions, schema-v14 migrations and atomic
run units of work, protocol-v1 serde, typed client boundaries, configuration, prompt assembly,
harness planning, and scoped-board behavior. Herdr suites cover the Herdr 0.8.0 / protocol 19
event and socket contract against an in-process fake server. Daemon private suites cover queue
claims, spawn/finalization atomicity, pane placement, configured and managed launch
characterization, request routing, watcher signals, timeout handling, per-session recovery, and
comment actor policy.
TUI suites cover fake-client reducer behavior, forms, and ratatui snapshots. Keep descriptions at
this ownership level; add a path only when it is a stable boundary or a useful entry point.

### Testkits: one construction per thing under test

Two crates own a `src/testkit.rs`. Neither is production code; both exist because the alternative is
a dozen near-identical hand-rolled setups that drift apart and then hide bugs in the differences.

- **`crates/board-daemon/src/testkit.rs`** (`cfg(test)` only) provides three things. `daemon()` is
  one builder for the twelve-argument `Daemon::new`, with an in-memory store, a `LocalSpawner` and
  dummy paths by default; `build()` also hands back the event and dispatch receivers, so a test can
  prove that a rolled-back operation emitted *nothing*. `herdr_server()` is one fake Herdr 0.8.0 /
  protocol 19 Unix socket with a settable protocol/version (so the compatibility gate can be served
  a **wrong** one), per-method canned responses, an optional accept count, and recorded-request
  inspection, plus the current-contract JSON constructors (`pane_info`, `agent_started`, …) used to
  build those responses. Finally the shared negative assertions `assert_no_events`, `assert_no_effects`,
  `assert_no_rollback_effects`, and `fault_db`, the armed lifecycle-fault `Db`.
- **`crates/board-tui/src/testkit.rs`** (feature `fake-client`) holds `DemoClient`, the driver and
  synthetic-input helpers, and the rendering helpers. It lives in the library rather than in a test
  file because each `tests/*.rs` is its own crate and its own link step — per-file helpers would be
  compiled once per test binary.

### Compatibility-gate coverage

The Herdr 0.8.0 / protocol 19 upgrade is tested at every compatibility-sensitive boundary, not
only at daemon startup: `daemon.status` checks the reported connection state; the supervisor checks
compatibility before `events.subscribe` and its acknowledgement/snapshot generation; detached
notifications check before `notification.show`; dispatch checks before workspace resolution and
placement; discovery checks include `space.list` and the read-only space preflight; and pane tests
cover `pane.set_title`, `run.focus`, placement, managed launch, and configured-runner operations.
The event tests also preserve the required subscribe-ack-before-snapshot order.

Cleanup and liveness for an already-owned pane remain intentionally ungated in the spawner's
`kill`/`pane.close` and `is_alive`/snapshot paths, so an incompatible Herdr cannot strand a pane the
daemon already owns. Those paths are not new discovery or launch authority; supervisor recovery
snapshots and caller-visible focus/rescue operations still use the checked connection.

### The fake-client ↔ daemon parity guard

`crates/board-daemon/src/ops/tests/parity.rs` is the mechanism that stops `FakeBoardClient` from
drifting away from boardd. The **entire** board-tui test tier runs against the fake, so a method the
daemon routes but the fake does not is a hole those suites cannot see: the reducer path that calls
it passes its own tests and fails only live.

Both sides are generated from the dispatch tables that actually answer requests — `ROUTED_METHODS`
from the daemon's `routes!`, `FAKE_CLIENT_METHODS` from the fake's `fake_methods!` — so neither list
can drift from the code. Four assertions hold: no duplicates on either side; the fake never answers
a method boardd does not route (which would let a TUI test pass against an RPC that does not exist);
every routed method is either implemented by the fake or named in `KNOWN_UNIMPLEMENTED`; and that
allowlist names only methods still routed.

`KNOWN_UNIMPLEMENTED` is the load-bearing half. Adding a method to the daemon **fails this test**
until someone either implements it in the fake (preferred) or lands it on the allowlist with a
reason. It currently holds `daemon.status`/`daemon.stop` (no daemon behind the fake),
`harness.capabilities`/`harness.list`/`session.list`/`space.list` (catalog RPCs answered from daemon
config and a live Herdr; the TUI's `DemoClient` stubs them on top of the fake),
`run.cancel`/`run.retry` (they mean "kill a pane" / "enqueue a run", which a DB-only fake with no
dispatcher cannot honestly model), and `run.pane_exited` (an internal wrapper callback no client
sends).

### RED → GREEN parity and schema-v14 coverage

The current parity/schema-v14 change is specified test-first:

- **RED contracts:** `crates/board-core/tests/comments_boards_contract.rs` covers board identity,
  visibility, comment CRUD, soft deletion, audit snapshots, system immutability, and author
  retention; `comments_migration_contract.rs` upgrades a v12 fixture to v14 without changing
  legacy board/card/comment/run values; `projects_contract.rs` covers project creation (existing
  directory required), per-project board isolation, selection persistence/recency, and the
  bootstrapped context.
- **GREEN implementation coverage:** `board-daemon/src/ops/tests/comments.rs` checks RPC routing,
  exact open-run actor ownership, fake-harness compatibility, immutable system comments, hidden
  deleted comments, and scoped change events; `board-daemon/src/ops/tests/projects.rs` checks the
  new `project.*`/`board.create`/`board.select` RPCs and their selection/recency side effects. The
  canonical CLI surface is covered by
  `board-cli/tests/integration/{cards,comments,columns,runs,projects}.rs` (one file per noun: CRUD,
  nullable
  clears, visibility, comment history and agent-run ownership, and the live-run verbs) plus
  `meta.rs` (command-tree refusals, `template apply`, version/status separation, `skill`, and the
  JSON error envelope); `compat.rs` protects top-level aliases and `exit_codes.rs` pins the process
  exit contract.
- **CLI/E2E boundary:** scenarios 01, 04, 06, 08, 09, 11, 16, and 17 exercise CLI comment
  creation, transition/silent-exit/timeout system comments, comment context in later prompts, and
  managed/configured comment completion against disposable Herdr. Scenarios 12 and 26 were updated
  for the project/board selector flow, and scenario 36 covers project create/select, selection and
  recency persistence across a daemon restart, per-project board isolation, and a cross-project
  card move that leaves the selection untouched. The managed and configured
  scenarios use the current Herdr 0.8.0 / protocol 19 contract; their files,
  `16-managed-p17.sh` and `17-configured-p17-runner.sh`, retain the historical `p17` names.
  CRUD/audit semantics stay in hermetic core/daemon/CLI tests; the live suite does not duplicate
  every management RPC.

The authoritative [`e2e/README.md`](../e2e/README.md) catalog currently covers scenarios 01–37,
and `e2e/run-all.sh` includes every numbered script. Scenarios 18–32 extend the live coverage with
nullable/validation, late-start and recovery, active-run timing, TUI layout and board transfers,
pane rescue, Pi catalog behavior, diagnostics, pane reuse, the managed Codex launch contract
(self-minted thread capture, delimited mint prompt, fork/reuse/rescue, fail-closed missing-report
rescue), and the managed OpenCode TUI launch contract (self-minted `ses_…` session capture,
`--agent herdr-board`/`--auto`/`-s`/`--fork` argv plus the exact `OPENCODE_CONFIG_CONTENT`
agent-config env — the root/TUI has no `--variant` — delimited mint prompt, fork/reuse/rescue,
fail-closed missing-report rescue). This document describes the intended coverage
and gate configuration; it does **not** claim that the full live E2E suite has passed.

Nullable update coverage in `board-core` is table-driven across every column/card nullable:
protocol tests verify omitted/null/value serde states, database tests verify set → clear and reopen
durability, and TUI reducer tests verify an emptied edit emits an explicit clear. The public board
protocol remains v1; no create DTO or non-null partial-update field uses `Patch<T>`. Shared core
validators merge the full row before mutation, apply capability/permission policy, and recheck
effective settings at enqueue time; daemon rejection tests assert no partial row or event. Live
scenario 18 covers the nullable and merged-validation wiring.

Inject clocks and paths; never sleep or read the wall clock in a unit test.

Configuration tests cover the missing-file and missing-section defaults, typed
`RootConfig`/daemon values, malformed TOML/type errors, and the fact that an existing malformed
file is never replaced by defaults. The CLI and TUI typed `BoardClient` boundary is tested without
SQLite I/O in production clients. Daemon settings tests
use an injected environment lookup to prove overrides win over TOML without
mutating process-global environment state; the daemon startup path parses the
shared root once and applies those overrides afterward.

### 2. Daemon + CLI integration (real boardd socket, no herdr)

`crates/board-cli/tests/integration.rs` is one test binary that delegates to
`integration/{cards,columns,comments,compat,events,exit_codes,harness,lifecycle,meta,runs,scope,stop}.rs`
over the shared `support.rs`. It exercises the whole daemon⇄CLI path without herdr by using the
**`LocalSpawner`** (agents are plain child processes) and a **fake harness script**.

- `TestDaemon::start(&[(k,v)])` (defined in `integration/support.rs` and shared
  by every module above, torn down on
  `Drop`) creates a `tempfile::TempDir`, writes a `config.toml`, points
  `BOARD_DB`/`BOARD_SOCKET`/`HERDR_BOARD_CONFIG`/`HOME` at it, spawns the real
  `board daemon --foreground` (`env!("CARGO_BIN_EXE_board")`), and polls
  `wait_ready`. Timing knobs keep it fast: `BOARD_TICK_MS=150`,
  `BOARD_LOCAL_POLL_MS=150`, `FAKE_AGENT_SLEEP=0.3`.
- Spawner selection is `BOARD_SPAWNER=local` + `[daemon] spawner = "local"`.
  `LocalSpawner` (`crates/board-daemon/src/spawner/local.rs`) launches agents via
  `std::process::Command` and tracks each `Child` for precise liveness/kill —
  no herdr, no Claude cost. Its sibling `HerdrSpawner`
  (`crates/board-daemon/src/spawner/herdr/`) launches herdr panes.
- The fake harness is `crates/board-cli/tests/fixtures/fake-agent.sh`, wired via
  `[harness.fake] argv = ["bash", "<path>"]`. It reads the board env, sleeps,
  then calls `$BOARD_BIN comment` + `$BOARD_BIN done`, so the real CLI request
  path is covered too. Behaviour is tunable with `FAKE_AGENT_SLEEP`,
  `FAKE_AGENT_OUTCOME`, `FAKE_AGENT_SILENT`.
- `TestDaemon::board(&[..])` runs the `board` CLI against the test daemon and
  captures output. Covered flows: happy pipeline, fail path, exit-without-done,
  timeout, queue serialization, cancel, retry-forks-a-run, template apply/refuse,
  archive/restore, the flock singleton, event subscription, and CLI-verb error surfacing.
- `exit_codes.rs` pins the process-status contract, which is scripted by agents:
  success is `0`; an RPC error in `1..=5` becomes `$?` verbatim (with or without
  `--json`); a CLI-local failure — a refused non-interactive `card delete`, an
  unknown verb, a bad enum value — exits `64` and, under `--json`, carries
  `{"code":64,"kind":"cli"}`. Out-of-range protocol codes need a **stub daemon**,
  a hand-rolled Unix listener answering one canned `Response::err(…, 256, …)`,
  because the real daemon cannot produce a code an exit status would silently
  turn into success: the exit clamps to `70` while the JSON envelope keeps the
  exact `256`. Two tests guard `--json` as a *parsed* global rather than an argv
  scan: a comment body of literally `--json` (after `--`) must still render a
  plain-text error, while a real leading `--json` in the same command line must
  still render JSON.

### 3. TUI fake-client tests (snapshots + reducer)

The TUI is tested against an in-memory client with no daemon and no herdr, under
the `fake-client` feature.

- `crates/board-tui/src/testkit.rs` defines `DemoClient` (wraps
  `board_core::client::FakeBoardClient` and additionally answers
  `harness.capabilities` / `session.list` / `space.list`; `without_caps()` /
  `without_spaces()` / `without_sessions()` force the form's free-text
  fallbacks). `demo_client()` seeds a full pipeline board.
- `crates/board-tui/tests/snapshots.rs` renders through the real `Driver` +
  `view()` into a `ratatui::backend::TestBackend` and asserts with **`insta`**.
  Determinism comes from a fixed `now` (`NOW_STR = "2026-07-14 12:00:00"`) and a
  `pin()` helper that rewrites active-run summary starts, so timers don't drift; it deliberately leaves
  the card `updated_at` at a conflicting value to prove card activity cannot reset a run timer.
- `crates/board-tui/tests/update.rs` (delegates to
  `update/{detail,editor,forms,modals,pane_title,scope,support_nav,switcher}.rs` over `helpers.rs`)
  unit-tests the pure reducer (`board_tui::app::update`) — detail popup/fullscreen toggle, edit→detail
  round-trip, comment/run scrolling and latest-item anchoring, `Enter` on `awaiting` confirmation,
  nullable-field explicit clears, space-kind visibility, form metadata loading without session/workspace
  RPCs, fetch-failure degradation, navigation wrapping/clamping, archive filtering/toggling, scoped board
  picker/switch, template guard on non-empty boards, jump-to-pane success/error, column drag/reorder,
  archived-card guard during column delete, and drag state.
- Column-form snapshots cover default and hostile Herdr origin contexts; form rebuild tests verify
  typed values and focus survive metadata refreshes while permission controls follow capabilities.
- `cargo run -p board-tui --example tui_fake --features fake-client` runs the
  full TUI against the seeded client for a manual look.

### 4. Live e2e scenarios (real herdr)

`e2e/` drives a **real** herdr with the **`HerdrSpawner`**, dispatching a
fake harness into **disposable** workspaces. This is the only layer that proves
the herdr wire integration end to end. It is covered in depth below.

## The live e2e harness

Layout under `e2e/`. The per-scenario catalog (use case ↔ scenario file ↔ status) lives in
[`e2e/README.md`](../e2e/README.md) and is maintained **only** there — this table covers the
shared infrastructure the scenarios sit on:

| File | Role |
|---|---|
| `lib.sh` | Shared harness sourced by every scenario: logging, isolated stack, cleanup registry, daemon + workspace helpers, pollers, JSON/`hrpc` helpers. |
| `test-harness.sh` | Deterministic Linux/macOS shell checks for signed ownership tokens, key scrubbing, every exact-resource kind, replacement, malformed ledger, and standalone parity; starts no live Herdr resources. |
| `process_identity.py` | Standard-library process backend: exact `/proc` identity plus owner environment on Linux; `proc_pidinfo`/`proc_pidpath`/`KERN_PROCARGS2` and signed direct-child transitions on Darwin. |
| `fake-agent.sh` | The fake harness dispatched instead of a real agent. Mirrors the crate fixture and adds `FAKE_AGENT_HOLD` (keep the pane alive after the run). |
| `hrpc.py` | One-shot raw herdr socket RPC (honours `HERDR_SOCKET_PATH`) for structural assertions (`tab.list`/`pane.list`/`pane.layout`). |
| `run-all.sh` | Builds once, runs every standard no-cost scenario, prints a PASS/FAIL/SKIP summary. |

Deterministic daemon tests cover working→running, blocked, Herdr's output-only `done` event →
`awaiting` (`agent_done`), idle grace→`awaiting` (`idle_expired`; never `lost`), timeout paused
while `awaiting`, pane exit without sleeps, and managed `agent_pane_busy` retry/cleanup without
allocating a second pane. The busy tests assert exact request preservation, bounded backoff, and
that persistent failure closes only the owned child rather than its pre-existing anchor. Herdr 0.8.0 / protocol 19 does not accept `done`
as a `pane.report_agent` input (`idle|working|blocked|unknown` only), so the live
`15-awaiting.sh` scenario uses Pi integration v8's supported report shape; on a managed
`agent.start` pane Herdr derives output `done` from the end-of-turn idle report. The scenario covers
blocked → working → Herdr done → `awaiting` → confirm → board `done` end to end. The opt-in real-Pi
smoke records live status when observable but does not require sampling `working` from a fast run.

Pane-first managed launch is covered separately by scenario 16. Its fake Pi and fake Claude
(via the same launch surface used with Pi integration v8 / Claude integration v7) are interactive
terminal fixtures, not provider stubs that can pass at process startup: each reports ordered
session identity then idle, waits for Herdr readiness, and refuses to call `board done` until the
exact card prompt arrives through `agent.prompt`. Scenario 17 proves configured harnesses remain
unmanaged and receive exact `BOARD_PROMPT`/`BOARD_SYSTEM_PROMPT` values through the generated
`pane run` bridge. Scenario 28 stages only isolated Pi `auth.json`/`models-store.json`, proves the auth-scoped
`harness.capabilities` result, and cycles the real TUI effort selector without submitting a card or
starting Pi. The real-Claude smoke is separate: it stages only completed onboarding/theme, exact
workspace trust, the current Claude integration v7 hook, credentials, and approved
`remote-settings.json`, so startup dialogs cannot consume `agent.prompt`; no broad personal Claude
state is copied. Its intended contract is one authorized Haiku/low attempt with no retry or fallback.

`scripts/e2e.sh` is a thin compat wrapper that `exec`s `run-all.sh`.

### How it stays isolated and safe

- **Ephemeral herdr session.** The suite **never** touches your real sessions.
  Each scenario generates a bounded `hb-e2e-<slug>-<pid>-<random64>` name (slug ≤8), checks the
  exact name in its marker-gated `/tmp/h<random32>` HOME before launching the server, and refuses to launch when a
  live Herdr socket already owns that exact name. Registry enumeration/parse failures fail closed;
  a stale or non-Herdr socket is reported as stale rather than treated as a collision. The boot,
  readiness, mutation, board-daemon signal, workspace-close, and session stop/delete paths capture and freshly verify a versioned HMAC-signed token containing PID, start time, parent, executable, and complete argv. Linux preserves the additional `/proc/environ` owner-token check. Darwin obtains process facts through `proc_pidinfo`, `proc_pidpath`, and `KERN_PROCARGS2`; because another process's environment is unavailable, a stable identity can only be minted from the same signed exact direct-child capability. The per-invocation signing key is copied and scrubbed at shell bootstrap, remains non-exported/non-persistent, and is delivered to Python only on fd 3. PID liveness alone never authorizes an operation. All scenario Herdr mutations use identity-gated CLI/RPC wrappers, and board commands that can trigger Herdr verify both boardd and the exact target session immediately before the request. This token gate is independent for primary and secondary sessions. Each scenario binds its isolated
  boardd to its own socket (`HERDR_SOCKET_PATH`). `run-all.sh` never boots or exports a shared
  session: it scrubs inherited session/plugin/provider variables and each child uses the same
  `e2e_init` ownership path as a **standalone** invocation.
  `03-sessions.sh` additionally boots a *second* ephemeral session to exercise the cross-session
  paths. Teardown stops/deletes only while that exact owner identity remains valid; it never
  pattern-kills or adopts/deletes a coincident replacement. **Keep mode** (`--keep` / `E2E_KEEP=1`)
  skips session stop/delete and workspace close so a run can be inspected; daemon/temp cleanup
  still runs, and cleanup failures propagate so a successful scenario cannot hide failed cleanup. Strict
  bounded mode-0700 root/artifact markers bind the current invocation token and owner; fake-managed roots
  are ledgered before any pre-init failure. Immediately after server spawn, an exact-child
  PID/start/parent/owner-token cleanup capability is armed and deferred before the provisional ledger
  validation. Its fresh verifier permits only the captured launcher or that same child's exact expected
  Herdr executable/argv after exec, so registration/transition failures terminate and reap the owner child.
  `run-all.sh` captures each child's pipeline status with `PIPESTATUS[0]`, stores per-scenario
  artifacts, and supports `--require-all` to treat any SKIP as failure. Stop requires a fresh full
  process token; delete is separately authorized only after that process is gone and the exact private
  registry name/ownership marker matches. An append-only ledger records full process identity tokens,
  exact sessions, marker-hashed scenario/managed roots and workspace evidence, and bounded configured/temp
  script paths with non-sensitive content digests; replacements and releases are validated. Marker and
  script digests are checked by the audit and
  immediately before destructive cleanup. Scenario/managed root reuse is process-local; suite artifact
  roots are stricter: `run-all.sh` refuses inherited `E2E_ARTIFACT_ROOT` and always creates a fresh private
  exact root without touching a pre-existing path. Standalone and suite cleanup run the same kind-specific audit. It checks only exact emitted entries—never a prefix/process-name scan or user
  inventory—and malformed ledgers fail closed. Sensitive prompt payload paths/content are not recorded.
  Standard children start from an environment allowlist with a fixed system-tool `PATH`, scrubbing
  inherited provider keys, endpoints, opt-ins, and shell functions; Herdr is resolved absolutely first.
- **Isolated stack and root.** The Herdr registry uses a marker-gated `/tmp/h<random32>` HOME;
  session socket paths are rejected above 92 bytes, preserving at least 15 bytes of AF_UNIX margin.
  `e2e_isolate` separately makes a short `/tmp/hb-e2e.XXXXXX` root and
  points `BOARD_DB`/`BOARD_SOCKET`/`HERDR_BOARD_CONFIG` there, sets a canonical disposable
  `BOARD_SCOPE_PATH`, and uses `BOARD_SPAWNER=herdr`. The daemon it starts is entirely separate
  from your real board — it never reads your board db or socket. (`/tmp`, not `$TMPDIR`: AF_UNIX
  socket paths cap at ~108 chars.) Standard managed fixtures additionally use one mode-`0700`
  `/tmp/hb-e2e-managed.XXXXXX` root with a marker; only the exact primary session owner may
  remove that marked root, and malformed/unmarked/out-of-root cleanup is refused.
- **Fake harnesses / no-provider boundary.** Config harness `fake` uses an env-wrapped bash
  script. The standard suite creates a mode-`0700` managed root with controlled `HOME`, `ZDOTDIR`,
  rc files, `PATH`, and exported fake-provider functions; it never sources user rc files. It
  resolves the Herdr executable to an absolute path before narrowing the managed pane `PATH`.
  Built-in managed agents see checked-in `e2e/fake-bin/pi`, `e2e/fake-bin/claude`,
  `e2e/fake-bin/codex`, and `e2e/fake-bin/opencode` only inside
  the disposable Herdr server/workspaces. The fixtures record argv/readiness/prompt evidence
  under the scenario temp dir and call only the isolated `board comment`/`board done`; they never
  replace user installations or make model calls.
- **Disposable workspaces + trap cleanup.** Every workspace the suite creates is
  registered for close via `e2e_defer`; `e2e_cleanup` (installed by `e2e_init`)
  runs the deferred commands in reverse on `EXIT` — workspaces close, then the
  daemon stops only after verifying its own captured identity token (independent of the Herdr
  server token), then the temp dir is removed. Cleanup failures propagate to the scenario result;
  cleanup is fail-closed: it removes only resources
  registered by the owning session and the marker-checked managed root owned by the exact session;
  generated configured-harness temp scripts are contained by setting `TMPDIR` to the exact isolated
  scenario root. It does not sweep shared/user paths or clean up a name after its owner dies. Mutations only ever
  hit workspaces the suite created; user workspaces/tabs are never touched. A configured runner
  script self-removes when it starts, but an asynchronously scheduled script whose pane never
  opens it can remain as the documented residual configured-script orphan.
- **HERDR MUTATION logging.** Every herdr-mutating call is printed with the
  `HERDR MUTATION:` prefix (via `mut`), so a run's side effects are auditable.
- **Raw asserts via `hrpc.py`.** For structural checks the CLI can't express, the
  scenarios call herdr directly: `hrpc tab.list '{"workspace_id":"…"}'`,
  `hrpc pane.list …`, `hrpc pane.layout '{"pane_id":"…"}'`. Set
  `HERDR_SOCKET_PATH` to target a specific session.

## Writing a new scenario

Add a numbered file `e2e/NN-name.sh`, source `lib.sh`, and follow this
skeleton:

```bash
#!/usr/bin/env bash
# NN-name.sh — one-line description of what this proves.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

# export E2E_FAKE_ENV="FAKE_AGENT_HOLD=300"   # only if you inspect live panes

e2e_boot          # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start
                  #   e2e_init          cleanup trap + private root/session (FIRST)
                  #   e2e_build         idempotent release build
                  #   e2e_isolate       temp db/socket/config with the fake harness
                  #   e2e_daemon_start  isolated boardd, stopped on cleanup
                  # Interleaving your own setup? Call the four in the long form
                  # instead — the order is a safety property.

step "Do the thing"
e2e_ws_create my-ws            # -> $E2E_WS, auto-closed on cleanup
WS="$E2E_WS"
# or: e2e_ws_standard my-ws    # step + create + WS_ID + echo, for the plain case
card_json="$("$BOARD_BIN" card new --title T -d D --harness fake \
  --space-kind workspace --space-ref "$WS" --json)"
CARD="$(printf '%s' "$card_json" | jget id)"
"$BOARD_BIN" move "$CARD" Execute --json >/dev/null   # auto column dispatches

outcome="$(wait_ok "$CARD")" || fail "outcome '$outcome' (expected ok)"

# structural assert
panes="$(hrpc pane.list "{\"workspace_id\":\"$WS\"}")"
# … parse with python3, fail "…" on mismatch …

step "NN-name: ALL CHECKS PASSED"
```

Then add the filename to the `SCENARIOS` array in `run-all.sh`.

Checklist:

- [ ] `set -euo pipefail`; source `lib.sh`.
- [ ] `e2e_boot` (or the same four calls in the same order) **before** creating anything.
- [ ] `e2e_init` **before** creating anything (it installs the cleanup trap and
      boots the invocation-owned ephemeral session, so partial runs still tear down).
- [ ] `e2e_init` boots a new invocation-owned session; it never adopts inherited session state.
- [ ] Use `e2e_isolate` — never the real board db/socket; keep all scenario state under its
      isolated `/tmp/hb-e2e.XXXXXX` root.
- [ ] Own every workspace you touch. Create with `e2e_ws_create` (auto-registers
      close). For a workspace the **daemon** creates, discover its id and register
      `e2e_ws_defer_close "$id" [session_socket]`. Never mutate a workspace you
      didn't create.
- [ ] Capture `e2e_ws_create`'s result from **`$E2E_WS`**, not `$(…)` — a command
      substitution runs in a subshell and loses the cleanup registration.
- [ ] Assert via the CLI's `--json` where possible; drop to `hrpc` for structure
      (tabs/panes/layout). Match agent pane labels as
      `card-<id>-<col>(-r<n>)?` — the `-r<run>` suffix appears on a name collision.
- [ ] **Skip, don't fail**, when a precondition is missing: call `skip "why"`
      (exit 3). `run-all.sh` reports SKIP, not FAIL.
- [ ] End with a clear `step "NN-…: ALL CHECKS PASSED"`.

## Field-tested gotchas

| Gotcha | What to do |
|---|---|
| **AF_UNIX 108-char limit** | Test db/socket must live under a short path. `e2e_isolate` uses `/tmp/hb-e2e.XXXXXX`, not `$TMPDIR` (which may be long). |
| **A unit test must not need `herdr` on `PATH`** | Layers 1–3 run without a live herdr, but `SessionRegistry::resolve` shells out to `herdr session list --json` even for the default session (only to name it), so any test reaching it passes on a developer machine and fails in CI, which has no herdr. Seed the listing with `SessionRegistry::with_entries` instead of `::new`. Reproduce CI locally before pushing: `env -u HERDR_BIN_PATH PATH="$(echo "$PATH" \| tr ':' '\n' \| grep -v "$HOME/.local/bin" \| paste -sd:):$HOME/.cargo/bin" cargo test --workspace --all-features`. |
| **done-race** | Managed built-ins still require a registered pane, so an instant `board done` for a queued built-in run is rejected. A configured harness is different: its exact `board done` is accepted even before runner registration, and the fake agent still sleeps `FAKE_AGENT_SLEEP` (default 1.5s) before reporting in ordinary scenarios. |
| **A pane dies with its process** | A herdr pane closes when its command exits. To inspect a live layout, keep the process alive — set `FAKE_AGENT_HOLD` (e.g. 300) so the agent sleeps **after** `board done`. Cleanup closes the workspace to end it. |
| **herdr closes the socket per request** | herdr serves one request per connection. `hrpc.py` (and `board-herdr`'s client) open a fresh connection every call — don't try to reuse one. |
| **Tab labels are not unique** | New runs resolve `card-<id>` tabs and shell anchors only by exact ids reconstructed from scoped durable panes; schema v14 retains the anchor id introduced in v12. Duplicate tab/anchor labels and legacy `kanban` are never adopted as ownership proof. A renamed exact anchor remains owned; a missing anchor is recovered only from an exact durable child, otherwise a fresh tab is created. Managed tabs are deliberately anchorless: a successful managed launch — and a successful managed `run.focus` rescue — closes the anchor (`pane_not_found` counts as closed; any other close failure warns and keeps the successful launch/rescue), so the next fresh run recovers from the exact durable child with a temporary anchor that is closed again after launch. Legacy rows retain their historical lookup. |
| **Agent names are exclusive** | While a pane is open its agent name is reserved. A collision (e.g. the session already has a `card-1-execute` pane) makes the daemon retry as `card-1-execute-r<run>`. Assertions must accept the optional `-r<n>` suffix. |
| **A newly split pane can be busy** | Herdr may return typed `agent_pane_busy` while the child still drains prior state or its login shell has not reached the interactive prompt yet. The daemon retries the exact managed `agent.start` request up to five times on that same owned child with 100ms backoff doubling per retry (≈3.1s window); persistent busy closes only that child and leaves the shell anchor. Do not treat it as `pane_not_found`: that error triggers one bounded full placement rediscovery from `tab.list`. |
| **Managed and configured pane identity differ** | Pane-first managed Pi/Claude panes expose the managed kind in `pane.agent`; configured panes are renamed to the daemon-assigned `card-<id>-<column>` label and remain unmanaged. Match the appropriate field and still accept the optional `-r<n>` name suffix. |
| **`pane.layout` nests under `layout`** | `hrpc pane.layout …` returns `{"type":"pane_layout","layout":{…panes,splits…}}`; read `.layout.panes`. |
| **Never `pkill` by "board daemon"** | That pattern matches your own shell too. Stop only the daemon you started after verifying its signed platform identity token (`e2e_daemon_stop`); PID liveness alone is insufficient. Linux uses `/proc`; Darwin uses native process APIs. Inspect only exact PIDs emitted by the invocation. |
| **Leaked ephemeral session from an aborted run** | If a run is killed before cleanup, an `hb-e2e-*` session may linger. Remove it wholesale: `herdr session stop <name> && herdr session delete <name>` (this closes its workspaces too). List leftovers with `herdr session list`. |

## Running

```bash
e2e/run-all.sh                  # build once, run all scenarios, print a summary
e2e/run-all.sh --keep           # keep each scenario's owned session/workspaces
e2e/run-all.sh --require-all    # fail if any selected scenario skips
bash e2e/ci.sh                  # CI-equivalent pin/verify/run/export wrapper (Linux x86_64)
e2e/run-all.sh 04 07            # only scenarios matching a filename filter
scripts/e2e.sh                  # compat wrapper -> run-all.sh
bash e2e/test-harness.sh         # static Linux/macOS safety gate; no Herdr
bash e2e/01-core.sh             # a single scenario (boots its own ephemeral session)
E2E_REAL_PI=1 e2e/real-pi-smoke.sh  # explicit real-provider opt-in; may incur cost
E2E_REAL_PI=1 E2E_REAL_PI_MODEL=openai-codex/gpt-5.3 E2E_REAL_PI_EFFORT=high e2e/real-pi-smoke.sh  # model/effort overrides; model must exist in `pi --list-models`
E2E_REAL_PI=1 E2E_REAL_PI_NEW_WORKSPACE=1 e2e/real-pi-smoke.sh  # daemon-created new_workspace space; asserts one card tab + one Pi pane, no anchor
E2E_REAL_CLAUDE_HAIKU=1 e2e/real-claude-haiku-smoke.sh  # one authorized Haiku/low attempt; may incur cost
E2E_REAL_CODEX=1 e2e/real-codex-smoke.sh  # one authorized Codex/low attempt; may incur cost
E2E_REAL_OPENCODE=1 e2e/real-opencode-smoke.sh  # one authorized OpenCode attempt; effort only via E2E_REAL_OPENCODE_EFFORT (default none — nemotron has no variants; effort rides the OPENCODE_CONFIG_CONTENT agent config, never argv); may incur cost
```

- Standard suite requires **exactly Herdr 0.8.0 / socket protocol 19**, `python3`, Bash ≥4, and `cargo`. It supports Linux and macOS; `run-all.sh` resolves Herdr and Bash absolutely before narrowing `PATH`. Every scenario preflights both `herdr --version` and a socket `ping`; older and unknown/future protocols fail before dispatch. The forced-build standard suite is configured to exercise scenarios 01–37 without provider calls; this is coverage guidance, not a claim that a full live run has passed. The real-Pi smoke additionally verifies Pi's runtime default model, current Herdr integration, and WezTerm. The real-Claude smoke is an intended-contract validation only: it requires a logged-in real Claude CLI plus current Herdr Claude integration v7, stages minimal completed onboarding/theme, exact workspace trust, the current Claude integration hook, credentials, and approved `remote-settings.json` under `/tmp` so startup dialogs cannot consume `agent.prompt`; no broad personal Claude state is copied, and it has   no retry or fallback. The real-Codex smoke is the same intended-contract shape for the Codex built-in: it requires the current Herdr Codex integration and hook, stages only codex auth/config/hook under a disposable `CODEX_HOME`, and authorizes one low-effort attempt with no retry or fallback. The real-OpenCode smoke is the same intended-contract shape for the OpenCode built-in: it requires the current Herdr OpenCode integration, stages only opencode config/auth under disposable `XDG_CONFIG_HOME`/`XDG_DATA_HOME` dirs, uses the env-selected model (default `opencode/nemotron-3-ultra-free`, which declares no variants — so no effort is passed by default and the model stays `-m`) and the permission mode to `--auto`; an effort can be opted into via `E2E_REAL_OPENCODE_EFFORT` (the root/TUI has no `--variant`, so the effort is transported through the `OPENCODE_CONFIG_CONTENT` agent-config env persisted in the run's launch spec, which the smoke validates as the exact `herdr-board` model+variant JSON; the default model has no variants, so only a model that supports the chosen effort works — override `E2E_REAL_OPENCODE_MODEL` accordingly). It authorizes one attempt with no retry or fallback. Their independent identity implementations remain Linux-only and are outside the portable provider-free gate. All four opt-ins compare user/repository state and clean exact resources. `run-all.sh` builds
  the release binary once; scenarios reuse it. Every scenario boots and cleans its own ephemeral
  session; scenario 03 additionally owns an independently tokened secondary session.
- Exit codes: scenario `0` = PASS, `3` = SKIP, other = FAIL; `run-all.sh` exits
  non-zero if any scenario FAILED.
- **The provider-free live suite is a CI gate.** After the static/Python/Rust jobs succeed,
  `bash e2e/ci.sh` installs or reuses only the pinned SHA-verified Herdr 0.8.0 Linux x86_64 binary,
  verifies protocol 19, and runs every standard scenario with `--require-all`. It never enables
  the real-Pi or real-Claude opt-ins or propagates provider credentials. The wrapper preserves the
  suite's newly-created private artifact root, copies only that exact validated root into
  `e2e-artifacts/`, and the workflow uploads the evidence under `if: always()` for 30 days. The
  static `bash e2e/test-harness.sh` gate remains separate and starts no Herdr — see the
  [test gates](README.md#test-gates-single-source).

### Multi-session (`03-sessions.sh`)

`03-sessions.sh` no longer needs a pre-existing second session. It boots its own
second ephemeral session `hb-e2e-<scenario-b>-<pid>-<random64>`
(`herdr --session <name> server &`), runs
the cross-session assertions against it, and stops+deletes it on cleanup (kept for
review under `--keep`/`E2E_KEEP=1`). The daemon reaches that session by name — session
enumeration shells out to `herdr session list --json` (`board-daemon/src/session.rs`),
so it is visible even though the daemon is bound to the primary ephemeral session.
