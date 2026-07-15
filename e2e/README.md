# Live e2e scenarios

The end-to-end suite for herdr-board. Each scenario drives a **real** herdr (the
`HerdrSpawner`) with a **fake harness** (`fake-agent.sh`) dispatched into
**disposable** workspaces, on a fully isolated stack (its own temp DB + socket +
config), and tears everything down on exit. This is the only test layer that
exercises the herdr wire integration end to end.

For the layers below this one (unit, daemon+CLI integration, TUI snapshots), the
isolation/safety design, and the **how-to-write-a-scenario** guide, see
[`../docs/testing.md`](../docs/testing.md). This file is the use case catalog.

## Use case ↔ scenario ↔ status

| Use case | Scenario file | Status |
|---|---|---|
| Happy path: dispatch → run → outcome/comment (CLI) **and** create-a-card via the TUI | `01-core.sh` | live |
| Mesh grid: several cards in one auto column tile into one `kanban` tab (one pane per card) | `02-kanban-grid.sh` | live |
| Multi-session: session/space scoping + cross-session dispatch against a **second** running session | `03-sessions.sh` | live (skips if no second session) |
| `board done --outcome fail` → card follows the column's `on_fail_column_id` | `04-fail-on-fail.sh` | live |
| `board retry` re-runs a finished card as a NEW run row | `05-retry.sh` | live |
| Agent pane exits without `board done` → run failed, **no** auto-transition | `06-silent-exit.sh` | live |
| `board cancel` on a live run kills the herdr pane; run `cancelled`, card `failed` | `07-cancel.sh` | live |
| Run overruns its column `timeout_minutes` → killed and follows `on_fail` | `08-column-timeout.sh` | live |
| A stage-1 comment flows into the stage-2 run's prompt (`## Card comments` section) | `09-comment-context.sh` | live |
| **idle-lost watchdog**: an idle agent (no `board done`) is marked `lost` | — | **not reproducible in live e2e** (see below) |

### Why idle-lost has no live scenario

The idle-lost watchdog (`watchers.rs`, `AgentStatusChanged` arm + `timeout_ticker`
lost loop) keys off herdr `pane.agent_status_changed` events. `idle_since` is only
armed on an `Idle`/`Done` status transition; the `Unknown` status is a no-op. A
plain bash pane (our fake harness) has **no** herdr agent-status integration
installed, so it reports `agent_status "unknown"` forever and never arms the
watchdog. It therefore cannot be reproduced live without wiring a real harness
status integration (`herdr integration install <name>`; see
[`../docs/herdr.md`](../docs/herdr.md)).

It is also **not currently covered by a unit test**: `crates/board-daemon/src/watchers.rs`
has no `#[test]`/`#[tokio::test]` module, and there is no `Lost`-path test in
`crates/board-cli/tests/integration.rs` or `crates/board-core/tests/engine.rs`
(the only automated watcher-path coverage is the timeout test
`timeout_kills_and_applies_on_fail` in `crates/board-cli/tests/integration.rs`).
The pure transition rule the watchdog relies on — a `lost` outcome yields **no**
target column (card parks `failed`) — lives in `engine.rs::decide_transition`.
This is a known coverage gap; if you add a harness status integration, this is the
scenario to add.

## Prerequisites

- A **running herdr** (`herdr 0.7.3+`) reachable on its default session socket, and
  `python3`.
- `cargo` on `PATH` — `run-all.sh` builds the release `board` binary once
  (`scripts/build.sh`); scenarios reuse it.
- `03-sessions.sh` additionally needs a **second running herdr session**
  (non-default). It discovers one and **skips** (does not fail) if none is running.
  Start one with `herdr session attach <name>` (interactive) or
  `herdr --session <name> server &` (headless), confirm with `herdr session list`.

## Running

```bash
e2e/run-all.sh              # build once, run every scenario, print a PASS/FAIL/SKIP summary
scripts/e2e.sh              # compat wrapper -> e2e/run-all.sh
bash e2e/04-fail-on-fail.sh # run a single scenario standalone
```

Exit codes: scenario `0` = PASS, `3` = SKIP (missing precondition), anything else =
FAIL. `run-all.sh` exits non-zero if any scenario FAILED. The suite is **not** part
of CI (it needs a live herdr) — it is run by a human/orchestrator.

## Files

| File | Role |
|---|---|
| `lib.sh` | Shared harness sourced by every scenario: logging, isolated stack, cleanup registry, daemon + workspace helpers, pollers (`wait_ok`/`wait_runs`), JSON/`hrpc`/`brpc`/`col_create` helpers. |
| `fake-agent.sh` | The fake harness dispatched instead of a real agent. Knobs: `FAKE_AGENT_SLEEP`, `FAKE_AGENT_OUTCOME`, `FAKE_AGENT_COMMENT`, `FAKE_AGENT_SILENT`, `FAKE_AGENT_HOLD`. |
| `hrpc.py` | One-shot raw **herdr** socket RPC (honours `HERDR_SOCKET_PATH`) for structural asserts (`tab.list`/`pane.list`/`pane.layout`). |
| `NN-*.sh` | The scenarios above. |
| `run-all.sh` | Builds once, runs every scenario in order, prints the summary. |

Columns have no `board` CLI verb, so scenarios configure them over the boardd
socket via `scripts/board-rpc.py` (wrapped by `lib.sh`'s `col_create` / `brpc`).
