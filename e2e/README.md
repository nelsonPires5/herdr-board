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
| Multi-session: session/space scoping + cross-session dispatch against a **second** session the scenario boots itself | `03-sessions.sh` | live |
| `board done --outcome fail` → card follows the column's `on_fail_column_id` | `04-fail-on-fail.sh` | live |
| `board retry` re-runs a finished card as a NEW run row | `05-retry.sh` | live |
| Agent pane exits without `board done` → run failed, **no** auto-transition | `06-silent-exit.sh` | live |
| `board cancel` on a live run kills the herdr pane; run `cancelled`, card `failed` | `07-cancel.sh` | live |
| Run overruns its column `timeout_minutes` → killed and follows `on_fail` | `08-column-timeout.sh` | live |
| A stage-1 comment flows into the stage-2 run's prompt (`## Card comments` section) | `09-comment-context.sh` | live |
| Archive filter cycles `ACTIVE/ALL/ARCHIVED` in the Herdr pane title and keeps the footer minimal | `10-archive-filter-title.sh` | live |
| Built-in Pi mint/retry argv, session fork, protocol prompt, and agent comment through real Herdr | `11-pi-harness.sh` | live, checked-in fake `pi`, zero provider cost |
| **idle-lost watchdog**: an idle agent (no `board done`) is marked `lost` | — | deterministic daemon tests; not sampled by standard live fake |

### Why idle-lost has no live scenario

The idle-lost watchdog (`watchers.rs`, `AgentStatusChanged` arm + `timeout_ticker`
lost loop) keys off herdr `pane.agent_status_changed` events. `idle_since` is only
armed on an `Idle`/`Done` status transition; the `Unknown` status is a no-op. A
plain bash pane (our fake harness) has **no** herdr agent-status integration
installed, so it reports `agent_status "unknown"` forever and never arms the
watchdog. It therefore cannot be reproduced live without wiring a real harness
status integration (`herdr integration install <name>`; see
[`../docs/herdr.md`](../docs/herdr.md)).

`crates/board-daemon/src/watchers.rs` covers working→running, blocked, idle grace→lost,
and pane exit deterministically through an injected `check_at(now)` seam, with no sleeps. The
separate opt-in real-Pi smoke records live working status when observable but does not require the
sample because a fast provider response can finish between polls.

## Prerequisites

- A **running herdr** (`herdr 0.7.3+`) and `python3`. Your real sessions are never
  touched — the suite runs everything inside an **ephemeral** session it creates.
- `cargo` on `PATH` — `run-all.sh` builds the release `board` binary once
  (`scripts/build.sh`); scenarios reuse it.
- **No second session needed.** `03-sessions.sh` boots its own second ephemeral
  session (`hb-e2e-b-<pid>`) and tears it down; it no longer discovers or skips.

## Ephemeral session model

Each run gets its own throwaway herdr session `hb-e2e-<pid>` (booted via
`herdr --session <name> server &`, ~2s). The isolated boardd binds to it
(`HERDR_SOCKET_PATH`), so that session is the daemon's "default", and every herdr
CLI call + `hrpc` assert targets it. `run-all.sh` boots ONE session for the whole
run and exports `E2E_SESSION`/`E2E_SESSION_SOCKET` to each scenario; a scenario run
**standalone** boots (and tears down) its own. Teardown stops+deletes the session;
`run-all.sh` then verifies no `hb-e2e-*` sessions remain.

## Running

```bash
e2e/run-all.sh              # standard suite; fake Pi, no provider/model cost
e2e/run-all.sh --keep       # keep sessions + each scenario's workspace for review
e2e/run-all.sh 04 07        # only scenarios whose filename matches a filter
scripts/e2e.sh              # compat wrapper -> e2e/run-all.sh
bash e2e/04-fail-on-fail.sh # run a single scenario standalone (boots its own session)
E2E_REAL_PI=1 e2e/real-pi-smoke.sh  # REAL provider, explicit opt-in, may incur cost
```

**Keep mode** (`--keep`, or `E2E_KEEP=1`): skips session stop/delete **and**
workspace close, so each scenario's disposable workspace stays inside its kept
session for inspection. Scenario-level daemons/temp dirs are still cleaned up (only
herdr-side artifacts are kept). At the end `run-all.sh` prints a review block per
kept session — the `herdr session attach <name>` line and the
`herdr session stop <n> && herdr session delete <n>` cleanup one-liner.

Exit codes: scenario `0` = PASS, `3` = SKIP (missing precondition), anything else =
FAIL. `run-all.sh` exits non-zero if any scenario FAILED. The suite is **not** part
of CI (it needs a live herdr) — it is run by a human/orchestrator.

## Files

| File | Role |
|---|---|
| `lib.sh` | Shared harness sourced by every scenario: logging, isolated stack, cleanup registry, daemon + workspace helpers, pollers (`wait_ok`/`wait_runs`), JSON/`hrpc`/`brpc`/`col_create` helpers. |
| `fake-agent.sh` | Config-defined fake harness used by scenarios 01–10. |
| `fake-bin/pi` | Executable named exactly `pi`, prepended only to disposable standard-E2E Herdr server PATH; records argv under isolated temp and calls the candidate `board`. Never modifies the installed Pi. |
| `real-pi-smoke.sh` | Fail-closed real-provider poem smoke. Detects Pi's runtime default model, passes low thinking, isolates board/Pi session output under `/tmp`, verifies integration/WezTerm, poem/comments/argv/git/settings, and supports keep mode for visual audit. Not in `run-all.sh`. |
| `hrpc.py` | One-shot raw **herdr** socket RPC (honours `HERDR_SOCKET_PATH`) for structural asserts (`tab.list`/`pane.list`/`pane.layout`). |
| `NN-*.sh` | The scenarios above. |
| `run-all.sh` | Builds once, runs every scenario in order, prints the summary. |

Columns have no `board` CLI verb, so scenarios configure them over the boardd
socket via `scripts/board-rpc.py` (wrapped by `lib.sh`'s `col_create` / `brpc`).
