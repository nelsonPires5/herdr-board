# Testing

How herdr-board is tested, and how to add tests for a change. Four layers, cheap
and hermetic first, expensive and live last:

```
unit / pure (per crate)                 no I/O, no daemon        — cargo test
  └─ daemon + CLI integration           real boardd socket,      — cargo test
        over a LocalSpawner                fake harness, no herdr
     └─ TUI fake-client + snapshots      ratatui TestBackend,     — cargo test
                                           in-memory client
        └─ live e2e scenarios           REAL herdr, disposable   — e2e/
                                           workspaces
```

The first three run in CI (`cargo test --workspace --all-features`); the live e2e
suite does **not** (it needs Herdr 0.7.5 and boots a real ephemeral server) — see
[Running](#running).

## The pyramid

### 1. Unit / pure tests (per crate)

Each crate's `tests/` holds integration-style tests against its public API. They
do no I/O beyond in-memory SQLite and never touch herdr.

| File | Covers |
|---|---|
| `crates/board-core/tests/engine.rs` | The **pure column engine** — `decide_transition`, `decide_entry`, `decide_signal` (agent signals → `awaiting`/`running`/`blocked` decisions), Herdr-neutral `decide_lifecycle`/`FinalizePlan` (identity and queued-harness eligibility plus cancel/timeout/pane-exit policy), `decide_auto_hop`, `decide_resumability`, `validate_*`, `format_duration`. No wall clock: elapsed time is passed as an explicit seconds argument (e.g. `decide_transition(.., Some(252))` → `"4m12s"`), so results are deterministic. |
| `crates/board-core/tests/db_atomic.rs` | File-backed abort-trigger rollback/reopen checks for enqueue, promotion, finalization comments/transitions, and auto-hop; a subprocess hard-exit fault between an internal statement and commit with zero returned effect/event; schema-v8 duplicate rejection and exact partial-index SQL; schema-v9 fresh/v8/legacy timeout derivation-once, unlimited/ended exclusions, and transactional idempotent/saturating pause-resume. Daemon tests additionally inject comment and next-enqueue failures, reopen the DB to compare the exact prior run/card/comments, assert no pre-commit kill/event/wake, cover the board-done/cancel/timeout/pane-exit duplicate-and-stale winner matrix, and record the exact post-commit effect order. |
| `crates/board-core/tests/db.rs` | SQLite upgrade fixtures for every supported source version v1–v7 plus fresh creation (v5 scoped boards + preserved Global; v6 status/`awaiting_reason`; v7 nullable `runs.system_prompt_snapshot`; v8 one-open-run invariant), board-boundary invariants, seed/CRUD, position compaction, FIFO queued-runs, and latest pane lookup. |
| `crates/board-core/tests/{capability,config,prompt,harness,protocol,fake_client}.rs` | Harness catalog + pane-name slug rules; typed `RootConfig` board/daemon defaults and fatal parsing; prompt assembly + effective-settings; harness argv/session planning; protocol serde round-trips; the in-memory `FakeBoardClient`. |
| `crates/board-herdr/tests/{events,socket}.rs` | herdr event decoding; socket client against an **in-process fake herdr server** on a temp unix socket (`serve_calls`/`serve_stream`), covering one-request-per-connection, bounded hanging peers/subscription acknowledgements, exact response IDs and matching errors, buffered pre-ack events, timeout reset, error mapping, and mid-call disconnect. |
| `board-daemon::supervisor` and watcher tests | Scripted resolver/runtime probes enforce conservative restart classification: unresolved sessions and runtime timeout/malformed/panic failures are `Unknown`, only a successful missing-pane snapshot is `Gone`, and `Alive` adoption is revalidated and idempotent. The always-on watcher scopes streams and duplicate pane IDs by socket, subscribes before snapshot reconciliation, and treats duplicate terminal observations idempotently. |

Nullable update coverage in `board-core` is table-driven across every column/card nullable:
protocol tests verify omitted/null/value serde states, database tests verify set → clear and reopen
durability, and TUI reducer tests verify an emptied edit emits an explicit clear. The public board
protocol remains v1; no create DTO or non-null partial-update field uses `Patch<T>`. Shared core
validators merge the full row before mutation, apply capability/permission policy, and recheck
effective settings at enqueue time; daemon rejection tests assert no partial row or event. Live
scenario 18 covers the nullable and merged-validation wiring.

Inject clocks and paths; never sleep or read the wall clock in a unit test.

Configuration tests cover the missing-file and missing-section defaults, typed
spawner and daemon values, malformed TOML/type errors, and the fact that an
existing malformed file is never replaced by defaults. Daemon settings tests
use an injected environment lookup to prove overrides win over TOML without
mutating process-global environment state; the daemon startup path parses the
shared root once and applies those overrides afterward.

### 2. Daemon + CLI integration (real boardd socket, no herdr)

`crates/board-cli/tests/integration.rs` exercises the whole daemon⇄CLI path
without herdr by using the **`LocalSpawner`** (agents are plain child processes)
and a **fake harness script**.

- `TestDaemon::start(&[(k,v)])` (a helper struct in that file, torn down on
  `Drop`) creates a `tempfile::TempDir`, writes a `config.toml`, points
  `BOARD_DB`/`BOARD_SOCKET`/`HERDR_BOARD_CONFIG`/`HOME` at it, spawns the real
  `board daemon --foreground` (`env!("CARGO_BIN_EXE_board")`), and polls
  `wait_ready`. Timing knobs keep it fast: `BOARD_TICK_MS=150`,
  `BOARD_LOCAL_POLL_MS=150`, `FAKE_AGENT_SLEEP=0.3`.
- Spawner selection is `BOARD_SPAWNER=local` + `[daemon] spawner = "local"`.
  `LocalSpawner` (`crates/board-daemon/src/spawner.rs`) launches agents via
  `std::process::Command` and tracks each `Child` for precise liveness/kill —
  no herdr, no Claude cost. Its sibling `HerdrSpawner` launches herdr panes.
- The fake harness is `crates/board-cli/tests/fixtures/fake-agent.sh`, wired via
  `[harness.fake] argv = ["bash", "<path>"]`. It reads the board env, sleeps,
  then calls `$BOARD_BIN comment` + `$BOARD_BIN done`, so the real CLI request
  path is covered too. Behaviour is tunable with `FAKE_AGENT_SLEEP`,
  `FAKE_AGENT_OUTCOME`, `FAKE_AGENT_SILENT`.
- `TestDaemon::board(&[..])` runs the `board` CLI against the test daemon and
  captures output. Covered flows: happy pipeline, fail path, exit-without-done,
  timeout, queue serialization, cancel, retry-forks-a-run, template apply/refuse,
  archive/restore, the flock singleton, event subscription, and CLI-verb error surfacing.

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
  `pin()` helper that rewrites Running cards' `updated_at`, so timers don't drift.
- `crates/board-tui/tests/update.rs` unit-tests the pure reducer
  (`board_tui::app::update`) — navigation, archive filtering/toggling, scoped board picker/switch,
  scoped form submission, immediate column-form metadata loading without session/workspace RPCs,
  jump-to-pane success/error, selectors, drag state, and templates.
- Column-form snapshots cover default and hostile Herdr origin contexts; form rebuild tests verify
  typed values and focus survive metadata refreshes while permission controls follow capabilities.
- `cargo run -p board-tui --example tui_fake --features fake-client` runs the
  full TUI against the seeded client for a manual look.

### 4. Live e2e scenarios (real herdr)

`e2e/` drives a **real** herdr with the **`HerdrSpawner`**, dispatching a
fake harness into **disposable** workspaces. This is the only layer that proves
the herdr wire integration end to end. It is covered in depth below.

## The live e2e harness

Layout under `e2e/` (see [`e2e/README.md`](../e2e/README.md) for the full use
case ↔ scenario ↔ status catalog):

| File | Role |
|---|---|
| `lib.sh` | Shared harness sourced by every scenario: logging, isolated stack, cleanup registry, daemon + workspace helpers, pollers, JSON/`hrpc` helpers. |
| `test-harness.sh` | Deterministic shell checks for ownership tokens and every exact-resource kind, replacement, malformed ledger, and standalone parity; starts no live Herdr resources. |
| `fake-agent.sh` | The fake harness dispatched instead of a real agent. Mirrors the crate fixture and adds `FAKE_AGENT_HOLD` (keep the pane alive after the run). |
| `hrpc.py` | One-shot raw herdr socket RPC (honours `HERDR_SOCKET_PATH`) for structural assertions (`tab.list`/`pane.list`/`pane.layout`). |
| `01-core.sh` | CLI path (dispatch → run → outcome/comment) + TUI path (drive the new-card form via send-keys). |
| `02-kanban-grid.sh` | Several cards → one auto column → asserts the mesh grid (one `kanban` tab, one pane per card, tiled rects). |
| `03-sessions.sh` | Multi-session behaviour against a **second collision-resistant ephemeral session it boots itself** (`hb-e2e-<scenario-b>-<pid>-<random64>`). |
| `04-fail-on-fail.sh` | `board done --outcome fail` → card follows the column's `on_fail_column_id`. |
| `05-retry.sh` | `board retry` spawns a NEW run row for a finished card (run count grows). |
| `06-silent-exit.sh` | A configured harness exits without `board done`; its generated runner calls the private pane-exit fallback → run failed, **no** auto-transition. |
| `07-cancel.sh` | `board cancel` on a live run kills the herdr pane; run `cancelled`, card `failed`. |
| `08-column-timeout.sh` | A run past its column `timeout_minutes` is killed and follows `on_fail`. |
| `09-comment-context.sh` | A stage-1 comment flows into the stage-2 run's `prompt_snapshot` (`## Card comments`). |
| `10-archive-filter-title.sh` | Archive filter → scoped dynamic pane title (`Board [scope · ACTIVE/ALL/ARCHIVED]`) + minimal footer. |
| `11-pi-harness.sh` | Built-in Pi mint/retry through real Herdr with `e2e/fake-bin/pi`; validates model, low thinking, 0600 protocol system file, exact `agent.prompt` delivery, comments, and fork target without provider cost. |
| `12-cwd-boards.sh` | Git root/subdir sharing, non-Git CWD isolation, independent columns/cards, scoped TUI title, and picker including Global. |
| `13-jump-to-pane.sh` | Held fake-agent pane + real plugin overlay: detail `o` focuses the same-session target and closes the board pane. |
| `14-column-config.sh` | Column harness/effort/permission overrides flow into a config-defined harness. |
| `15-awaiting.sh` | Integration-style reports on a live managed pane: blocked → working → end-of-turn idle (Herdr derives `done`) → `awaiting` (`agent_done`, run/pane stay open, timeout paused); `board done ok` confirms → `done`, no column move. |
| `16-managed-p17.sh` | Pane-first protocol-17 Pi + Claude launch through checked-in no-provider fixtures: exact 0600 system files, readiness, session/idle reports, `agent.prompt`, and held layout. |
| `17-configured-p17-runner.sh` | Unmanaged protocol-17 configured-harness bridge: exact argv/multiline env/cwd/socket values, `pane run`, held layout, and explicit `board done`. |
| `18-nullable-clear.sh` | Omitted/null/value persistence, merged validation with atomic rejection, and provider-free dispatch after clearing overrides. |
| `19-daemon-before-herdr.sh` | Boardd starts before Herdr, then the always-on supervisor late-connects and observes an exact owned pane exit. |
| `20-herdr-recovery.sh` | An owned transparent proxy proves outage/restart remains `Unknown`, durable timeout budget survives, and reconnect snapshot closes a dropped-event gap once. |
| `real-pi-smoke.sh` | Separate opt-in (`E2E_REAL_PI=1`) real-provider poem smoke; never included by `run-all.sh`. |
| `real-claude-haiku-smoke.sh` | Opt-in intended-contract smoke (`E2E_REAL_CLAUDE_HAIKU=1`): exactly one authorized Claude Haiku/low attempt, stages only completed onboarding/theme, exact workspace trust, the installed Herdr hook, credentials, and approved remote-settings bytes, preventing startup dialogs from consuming `agent.prompt`; no broad personal Claude state, retry/fallback, or standard-suite inclusion. |
| `run-all.sh` | Builds once, runs every standard no-cost scenario, prints a PASS/FAIL/SKIP summary. |

Deterministic daemon tests cover working→running, blocked, Herdr's output-only `done` event →
`awaiting` (`agent_done`), idle grace→`awaiting` (`idle_expired`; never `lost`), timeout paused
while `awaiting`, and pane exit without sleeps. Herdr 0.7.5 / protocol 17 does not accept `done`
as a `pane.report_agent` input (`idle|working|blocked|unknown` only), so the live
`15-awaiting.sh` scenario uses Pi integration v6's supported report shape; on a managed
`agent.start` pane Herdr derives output `done` from the end-of-turn idle report. The scenario covers
blocked → working → Herdr done → `awaiting` → confirm → board `done` end to end. The opt-in real-Pi
smoke records live status when observable but does not require sampling `working` from a fast run.

Protocol-17 managed launch is covered separately by scenario 16. Its fake Pi and fake Claude
(via the same launch surface used with Pi integration v6 / Claude integration v7) are interactive
terminal fixtures, not provider stubs that can pass at process startup: each reports ordered
session identity then idle, waits for Herdr readiness, and refuses to call `board done` until the
exact card prompt arrives through `agent.prompt`. Scenario 17 proves configured harnesses remain
unmanaged and receive exact `BOARD_PROMPT`/`BOARD_SYSTEM_PROMPT` values through the generated
`pane run` bridge. The real-Claude smoke is separate: it stages only completed onboarding/theme, exact
workspace trust, the installed Herdr hook, credentials, and approved `remote-settings.json`,
so startup dialogs cannot consume `agent.prompt`; no broad personal Claude state is copied.
Its intended contract is one authorized Haiku/low attempt with no retry or fallback.

`scripts/e2e.sh` is a thin compat wrapper that `exec`s `run-all.sh`.

### How it stays isolated and safe

- **Ephemeral herdr session.** The suite **never** touches your real sessions.
  Each scenario generates a bounded `hb-e2e-<slug>-<pid>-<random64>` name (slug ≤8), checks the
  exact name in its marker-gated `/tmp/h<random32>` HOME before launching the server, and refuses to launch when a
  live Herdr socket already owns that exact name. Registry enumeration/parse failures fail closed;
  a stale or non-Herdr socket is reported as stale rather than treated as a collision. The boot,
  readiness, mutation, board-daemon signal, workspace-close, and session stop/delete paths capture
  and verify one Linux `/proc` identity token containing PID, start time, executable, complete argv,
  and exact `--session <name> server` full argv identity; PID liveness alone never authorizes an operation. All scenario Herdr mutations use identity-gated CLI/RPC wrappers, and board commands that can trigger Herdr verify both boardd and the exact target session immediately before the request. This token gate is independent for primary and secondary sessions. Each scenario binds its isolated
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
  Built-in managed agents see checked-in `e2e/fake-bin/pi` and `e2e/fake-bin/claude` only inside
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

e2e_init          # cleanup trap + private root/session + preconditions (FIRST)
e2e_build         # idempotent release build
e2e_isolate       # temp db/socket/config with the fake harness
e2e_daemon_start  # isolated boardd, stopped on cleanup

step "Do the thing"
e2e_ws_create my-ws            # -> $E2E_WS, auto-closed on cleanup
WS="$E2E_WS"
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
| **done-race** | Managed built-ins still require a registered pane, so an instant `board done` for a queued built-in run is rejected. A configured harness is different: its exact `board done` is accepted even before runner registration, and the fake agent still sleeps `FAKE_AGENT_SLEEP` (default 1.5s) before reporting in ordinary scenarios. |
| **A pane dies with its process** | A herdr pane closes when its command exits. To inspect a live layout, keep the process alive — set `FAKE_AGENT_HOLD` (e.g. 300) so the agent sleeps **after** `board done`. Cleanup closes the workspace to end it. |
| **herdr closes the socket per request** | herdr serves one request per connection. `hrpc.py` (and `board-herdr`'s client) open a fresh connection every call — don't try to reuse one. |
| **Tab labels are not unique** | Resolve the `kanban` tab by find-or-create, and filter panes by its `tab_id`; don't assume one tab per label globally. |
| **Agent names are exclusive** | While a pane is open its agent name is reserved. A collision (e.g. the session already has a `card-1-execute` pane) makes the daemon retry as `card-1-execute-r<run>`. Assertions must accept the optional `-r<n>` suffix. |
| **Managed and configured pane identity differ** | Protocol-17 managed Pi/Claude panes expose the managed kind in `pane.agent`; configured panes are renamed to the daemon-assigned `card-<id>-<column>` label and remain unmanaged. Match the appropriate field and still accept the optional `-r<n>` name suffix. |
| **`pane.layout` nests under `layout`** | `hrpc pane.layout …` returns `{"type":"pane_layout","layout":{…panes,splits…}}`; read `.layout.panes`. |
| **Never `pkill` by "board daemon"** | That pattern matches your own shell too. Stop only the daemon you started after verifying its captured `/proc` identity token (`e2e_daemon_stop`); PID liveness alone is insufficient. To find leaked daemons from an aborted run: `ps -C board -o pid=,args=`. |
| **Leaked ephemeral session from an aborted run** | If a run is killed before cleanup, an `hb-e2e-*` session may linger. Remove it wholesale: `herdr session stop <name> && herdr session delete <name>` (this closes its workspaces too). List leftovers with `herdr session list`. |

## Running

```bash
e2e/run-all.sh                  # build once, run all scenarios, print a summary
e2e/run-all.sh --keep           # keep each scenario's owned session/workspaces
e2e/run-all.sh --require-all    # fail if any selected scenario skips
e2e/run-all.sh 04 07            # only scenarios matching a filename filter
scripts/e2e.sh                  # compat wrapper -> run-all.sh
bash e2e/01-core.sh             # a single scenario (boots its own ephemeral session)
E2E_REAL_PI=1 e2e/real-pi-smoke.sh  # explicit real-provider opt-in; may incur cost
E2E_REAL_CLAUDE_HAIKU=1 e2e/real-claude-haiku-smoke.sh  # one authorized Haiku/low attempt; may incur cost
```

- Standard suite requires **exactly Herdr 0.7.5 / socket protocol 17**, `python3`, and `cargo`. Every scenario preflights both `herdr --version` and a socket `ping`; protocol 16 and unknown/future protocols fail before dispatch. The forced-build standard suite scenarios 01–20 pass with no provider calls. The real-Pi smoke additionally verifies Pi's runtime default model, current Herdr integration, and WezTerm. The real-Claude smoke is an intended-contract validation only: it requires a logged-in real Claude CLI plus current Herdr Claude integration v7, stages minimal completed onboarding/theme, exact workspace trust, the installed Herdr hook, credentials, and approved `remote-settings.json` under `/tmp` so startup dialogs cannot consume `agent.prompt`; no broad personal Claude state is copied, and it has no retry or fallback. Both opt-ins compare user/repository state and clean exact resources. `run-all.sh` builds
  the release binary once; scenarios reuse it. Every scenario boots and cleans its own ephemeral
  session; scenario 03 additionally owns an independently tokened secondary session.
- Exit codes: scenario `0` = PASS, `3` = SKIP, other = FAIL; `run-all.sh` exits
  non-zero if any scenario FAILED.
- **Not in CI.** CI runs `cargo fmt`/`clippy`/`test` (layers 1–3); the live e2e
  suite needs Herdr 0.7.5 to boot a real ephemeral protocol-17 server and is run
  by a human/orchestrator.

### Multi-session (`03-sessions.sh`)

`03-sessions.sh` no longer needs a pre-existing second session. It boots its own
second ephemeral session `hb-e2e-<scenario-b>-<pid>-<random64>`
(`herdr --session <name> server &`), runs
the cross-session assertions against it, and stops+deletes it on cleanup (kept for
review under `--keep`/`E2E_KEEP=1`). The daemon reaches that session by name — session
enumeration shells out to `herdr session list --json` (`board-daemon/src/session.rs`),
so it is visible even though the daemon is bound to the primary ephemeral session.
