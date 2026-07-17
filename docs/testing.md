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
suite does **not** (it needs a running herdr) — see [Running](#running).

## The pyramid

### 1. Unit / pure tests (per crate)

Each crate's `tests/` holds integration-style tests against its public API. They
do no I/O beyond in-memory SQLite and never touch herdr.

| File | Covers |
|---|---|
| `crates/board-core/tests/engine.rs` | The **pure column engine** — `decide_transition`, `decide_entry`, `validate_*`, `format_duration`. No wall clock: elapsed time is passed as an explicit seconds argument (e.g. `decide_transition(.., Some(252))` → `"4m12s"`), so results are deterministic. |
| `crates/board-core/tests/db.rs` | SQLite migrations (including v3 archive state), seed, CRUD, position compaction, FIFO queued-runs — on an in-memory db (`mem()` helper). |
| `crates/board-core/tests/{capability,config,prompt,harness,protocol,fake_client}.rs` | Harness catalog + pane-name slug rules; config defaults/parsing; prompt assembly + effective-settings; harness argv/session planning; protocol serde round-trips; the in-memory `FakeBoardClient`. |
| `crates/board-herdr/tests/{events,socket}.rs` | herdr event decoding; socket client against an **in-process fake herdr server** on a temp unix socket (`serve_calls`/`serve_stream`), covering one-request-per-connection, error mapping, and mid-call disconnect. |

Inject clocks and paths; never sleep or read the wall clock in a unit test.

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
  (`board_tui::app::update`) — navigation, archive filtering/toggling, form field cycling/visibility,
  selectors, drag state, template-only-on-empty-board.
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
| `fake-agent.sh` | The fake harness dispatched instead of a real agent. Mirrors the crate fixture and adds `FAKE_AGENT_HOLD` (keep the pane alive after the run). |
| `hrpc.py` | One-shot raw herdr socket RPC (honours `HERDR_SOCKET_PATH`) for structural assertions (`tab.list`/`pane.list`/`pane.layout`). |
| `01-core.sh` | CLI path (dispatch → run → outcome/comment) + TUI path (drive the new-card form via send-keys). |
| `02-kanban-grid.sh` | Several cards → one auto column → asserts the mesh grid (one `kanban` tab, one pane per card, tiled rects). |
| `03-sessions.sh` | Multi-session behaviour against a **second ephemeral session it boots itself** (`hb-e2e-b-<pid>`). |
| `04-fail-on-fail.sh` | `board done --outcome fail` → card follows the column's `on_fail_column_id`. |
| `05-retry.sh` | `board retry` spawns a NEW run row for a finished card (run count grows). |
| `06-silent-exit.sh` | Agent pane exits without `board done` → run failed, **no** auto-transition. |
| `07-cancel.sh` | `board cancel` on a live run kills the herdr pane; run `cancelled`, card `failed`. |
| `08-column-timeout.sh` | A run past its column `timeout_minutes` is killed and follows `on_fail`. |
| `09-comment-context.sh` | A stage-1 comment flows into the stage-2 run's `prompt_snapshot` (`## Card comments`). |
| `10-archive-filter-title.sh` | Archive filter → dynamic Herdr pane title (`Board [ACTIVE/ALL/ARCHIVED]`) + minimal footer. |
| `run-all.sh` | Builds once, runs every scenario, prints a PASS/FAIL/SKIP summary. |

The `idle-lost` watchdog has **no** live scenario — it keys off herdr
`pane.agent_status_changed`, which a bash fake harness never emits (panes report
`agent_status "unknown"`), so it cannot fire without a real harness status
integration. See [`e2e/README.md`](../e2e/README.md) for the full use case ↔
scenario ↔ status catalog, including that gap.

`scripts/e2e.sh` is a thin compat wrapper that `exec`s `run-all.sh`.

### How it stays isolated and safe

- **Ephemeral herdr session.** The suite **never** touches your real sessions.
  Each run boots a throwaway session `hb-e2e-<pid>` (`herdr --session <name>
  server &`, ~2s) and binds the isolated boardd to it (`HERDR_SOCKET_PATH`), so it
  is the daemon's "default" and every herdr CLI + `hrpc` call targets it.
  `run-all.sh` boots ONE session for the whole run (exports
  `E2E_SESSION`/`E2E_SESSION_SOCKET` to scenarios) and tears it down after,
  verifying no `hb-e2e-*` session lingers; a scenario run **standalone** boots and
  tears down its own via `e2e_session_ensure` (called by `e2e_init`).
  `03-sessions.sh` additionally boots a *second* ephemeral session to exercise the
  cross-session paths. **Keep mode** (`--keep` / `E2E_KEEP=1`) skips session
  stop/delete and workspace close so a run can be inspected; `run-all.sh` then
  prints an attach + cleanup one-liner per kept session.
- **Isolated stack.** `e2e_isolate` makes a short `/tmp/hb-e2e.XXXXXX` dir and
  points `BOARD_DB`/`BOARD_SOCKET`/`HERDR_BOARD_CONFIG` there, with
  `BOARD_SPAWNER=herdr`. The daemon it starts is entirely separate from your real
  board — it never reads your board db or socket. (`/tmp`, not `$TMPDIR`: AF_UNIX
  socket paths cap at ~108 chars.)
- **Fake harness via config argv.** herdr agent panes do **not** inherit the
  workspace's env — they only get what the daemon injects at `agent.start`
  (`BOARD_CARD_ID`/`BOARD_RUN_ID`/`BOARD_SOCKET`). So `lib.sh` bakes `BOARD_BIN`
  (and any `E2E_FAKE_ENV` knobs like `FAKE_AGENT_HOLD`) into an `env` wrapper in
  the harness argv:
  `argv = ["env", "BOARD_BIN=…", "FAKE_AGENT_HOLD=300", "bash", "fake-agent.sh"]`.
- **Disposable workspaces + trap cleanup.** Every workspace the suite creates is
  registered for close via `e2e_defer`; `e2e_cleanup` (installed by `e2e_init`)
  runs the deferred commands in reverse on `EXIT` — workspaces close, then the
  daemon stops (by **pid**), then the temp dir is removed. Mutations only ever hit
  workspaces the suite created; user workspaces/tabs are never touched.
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

e2e_init          # preconditions + cleanup trap + ephemeral session  (FIRST)
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
      boots/adopts the ephemeral session, so partial runs still tear down).
- [ ] Use `e2e_isolate` — never the real board db/socket.
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
| **done-race** | A run's `started_at` commits just after `agent.start`; an instant `board done` races it and gets "no active run". The fake agent sleeps `FAKE_AGENT_SLEEP` (default 1.5s) **before** reporting. |
| **A pane dies with its process** | A herdr pane closes when its command exits. To inspect a live layout, keep the process alive — set `FAKE_AGENT_HOLD` (e.g. 300) so the agent sleeps **after** `board done`. Cleanup closes the workspace to end it. |
| **herdr closes the socket per request** | herdr serves one request per connection. `hrpc.py` (and `board-herdr`'s client) open a fresh connection every call — don't try to reuse one. |
| **Tab labels are not unique** | Resolve the `kanban` tab by find-or-create, and filter panes by its `tab_id`; don't assume one tab per label globally. |
| **Agent names are exclusive** | While a pane is open its agent name is reserved. A collision (e.g. the session already has a `card-1-execute` pane) makes the daemon retry as `card-1-execute-r<run>`. Assertions must accept the optional `-r<n>` suffix. |
| **The agent name is the pane `label`** | In `pane.list`, the daemon-assigned agent name shows up as the pane's `label`, not the `agent` field (which only fills when a herdr integration reports status). |
| **`pane.layout` nests under `layout`** | `hrpc pane.layout …` returns `{"type":"pane_layout","layout":{…panes,splits…}}`; read `.layout.panes`. |
| **Never `pkill` by "board daemon"** | That pattern matches your own shell too. Stop only the daemon you started, by pid (`e2e_daemon_stop`). To find leaked daemons from an aborted run: `ps -C board -o pid=,args=`. |
| **Leaked ephemeral session from an aborted run** | If a run is killed before cleanup, an `hb-e2e-*` session may linger. Remove it wholesale: `herdr session stop <name> && herdr session delete <name>` (this closes its workspaces too). List leftovers with `herdr session list`. |

## Running

```bash
e2e/run-all.sh                  # build once, run all scenarios, print a summary
e2e/run-all.sh --keep           # keep sessions + workspaces for review
e2e/run-all.sh 04 07            # only scenarios matching a filename filter
scripts/e2e.sh                  # compat wrapper -> run-all.sh
bash e2e/01-core.sh             # a single scenario (boots its own ephemeral session)
```

- Requires a **running herdr** (`herdr 0.7.3+`) and `python3`. `run-all.sh` builds
  the release binary once; scenarios reuse it. **No second session** is needed —
  the suite boots its own ephemeral session(s) and cleans them up.
- Exit codes: scenario `0` = PASS, `3` = SKIP, other = FAIL; `run-all.sh` exits
  non-zero if any scenario FAILED.
- **Not in CI.** CI runs `cargo fmt`/`clippy`/`test` (layers 1–3); the live e2e
  suite needs a herdr and is run by a human/orchestrator.

### Multi-session (`03-sessions.sh`)

`03-sessions.sh` no longer needs a pre-existing second session. It boots its own
second ephemeral session `hb-e2e-b-<pid>` (`herdr --session <name> server &`), runs
the cross-session assertions against it, and stops+deletes it on cleanup (kept for
review under `--keep`/`E2E_KEEP=1`). The daemon reaches that session by name — session
enumeration shells out to `herdr session list --json` (`board-daemon/src/session.rs`),
so it is visible even though the daemon is bound to the primary ephemeral session.
