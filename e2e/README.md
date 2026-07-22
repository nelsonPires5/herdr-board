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
| Configured harness exits without `board done`; generated runner reports the silent exit → run failed, **no** auto-transition | `06-silent-exit.sh` | live |
| `board cancel` on a live run kills the herdr pane; run `cancelled`, card `failed` | `07-cancel.sh` | live |
| Run overruns its column `timeout_minutes` → killed and follows `on_fail` | `08-column-timeout.sh` | live |
| A stage-1 comment flows into the stage-2 run's prompt (`## Card comments` section) | `09-comment-context.sh` | live |
| Archive filter cycles scoped `ACTIVE/ALL/ARCHIVED` Herdr pane titles and keeps the footer minimal | `10-archive-filter-title.sh` | live |
| Built-in Pi mint/retry argv, session fork, protocol prompt, and agent comment through real Herdr | `11-pi-harness.sh` | live, checked-in fake `pi`, zero provider cost |
| Git-root/CWD board identity, independent pipelines/cards, scoped TUI title, and Global picker entry | `12-cwd-boards.sh` | live |
| Card-detail `o` focuses a held same-session run pane and closes the real plugin overlay | `13-jump-to-pane.sh` | live |
| A column `harness_override` (TUI select) drives a run via a config-defined harness; `harness.list` advertises config harnesses; effort/permission overrides flow into the run argv | `14-column-config.sh` | live |
| Integration-style status reports on a live managed pane: blocked → working → end-of-turn idle (Herdr derives `done`) → `awaiting` (`agent_done`), timeout paused; `board done ok` → `done` in the same column | `15-awaiting.sh` | live |
| Managed protocol-17 Pi + Claude: pane-first placement, exact 0600 system file, readiness/session reports, exact `agent.prompt` task delivery, and held layout | `16-managed-p17.sh` | live, checked-in fake `pi` + `claude`, zero provider cost |
| Unmanaged protocol-17 configured harness: exact argv/multiline env/cwd/socket bridge through CLI-only `pane run`, held layout, explicit completion | `17-configured-p17-runner.sh` | live, temporary runner, zero provider cost |

### How the live scenario produces Herdr `done`

Herdr 0.7.5 / protocol 17 exposes `done` as an output `AgentStatus`, but its
supported integration input, `pane.report_agent`, accepts only
`idle|working|blocked|unknown` (`herdr pane report-agent --help` and `herdr api
schema --json`). Pi integration v6 uses that API with `source=herdr:pi` and
reports `working`/`blocked`/`idle`; there is no supported `herdr ... --state
done` argv to inject. On a managed `agent.start` pane, Herdr
derives the output status `done` from the integration's end-of-turn idle report.
The live scenario reproduces that supported path and asserts Herdr
`agent_status=done`, board `awaiting_reason=agent_done`, an open run/live pane,
paused timeout, and explicit confirmation to board `done`.

`crates/board-daemon/src/watchers.rs` additionally covers idle grace →
`awaiting` (`idle_expired`), working/blocked signals, timeout pause, and pane exit
deterministically through an injected `check_at(now)` seam. One live scenario
is sufficient to prove the real Herdr event subscription and signal-application
path without a provider call or an unsupported status injection. The separate
opt-in real-Pi smoke records live working status when observable but does not
require the sample because a fast provider response can finish between polls.

## Prerequisites

- **Exactly Herdr 0.7.5 / socket protocol 17** and `python3`. Every scenario checks
  both `herdr --version` and the ephemeral server's `ping` before dispatch; protocol
  16 and unknown/future protocols are rejected. Your real sessions are never
  touched — the suite boots its own **ephemeral** Herdr server/session.
- `cargo` on `PATH` — `run-all.sh` builds the release `board` binary once
  (`scripts/build.sh`); scenarios reuse it.
- **No second session needed.** `03-sessions.sh` boots its own second ephemeral
  collision-resistant session (`hb-e2e-b-<pid>-<random>-<random>`) and tears it down;
  it no longer discovers or skips.

## Ephemeral session model

Each run generates a collision-resistant `hb-e2e-<pid>-<random>-<random>` name and
preflights that exact name in the session registry before launching `herdr --session
<name> server`. A live Herdr socket with the exact name is a collision; registry
enumeration/parse failures fail closed, while stale or non-Herdr sockets are reported as stale.
The isolated boardd binds to the newly started session
(`HERDR_SOCKET_PATH`), so that session is the daemon's "default", and every herdr CLI
call + `hrpc` assert targets it. Boot/readiness, mutation, board-daemon signals, workspace close, and session stop/delete use one
captured Linux `/proc` identity token containing start time, executable, complete argv,
and expected session/name argv identity; PID liveness alone never authorizes an operation.
The same fail-closed gate applies to the primary and secondary sessions, run-all, standalone
scenarios, and future real-Claude smoke paths. `run-all.sh` boots ONE session for the whole run
and exports `E2E_SESSION`/`E2E_SESSION_SOCKET` to each scenario; a scenario run **standalone**
boots (and tears down) its own. Teardown stops+deletes only while that owner identity remains
valid, then verifies no `hb-e2e-*` sessions remain; it never cleans a coincident replacement.
Cleanup failures propagate, so a successful scenario cannot hide failed cleanup.

The provider-free harness uses a mode-`0700` `/tmp/hb-e2e-managed.XXXXXX` root with a marker,
controlled `HOME`, `ZDOTDIR`, rc files, `PATH`, and fake-provider functions; it never sources user
rc files. Herdr is resolved to an absolute path before the managed pane `PATH` is narrowed.
Cleanup removes the marked root only for its exact primary-session owner and refuses malformed,
unmarked, or out-of-root paths. The board DB/socket/config and scope remain under the separate
short `/tmp/hb-e2e.XXXXXX` isolated root. The generated configured runner self-removes when it
starts; if asynchronous `pane run` scheduling succeeds but the pane never opens it, the residual
configured-script orphan is a documented limitation. The forced-build standard suite passes
scenarios 01–17 without provider calls.

## Running

```bash
e2e/run-all.sh              # standard suite; fake Pi + Claude, no provider/model cost
e2e/run-all.sh --keep       # keep sessions + each scenario's workspace for review
e2e/run-all.sh 04 07        # only scenarios whose filename matches a filter
scripts/e2e.sh              # compat wrapper -> e2e/run-all.sh
bash e2e/04-fail-on-fail.sh # run a single scenario standalone (boots its own session)
E2E_REAL_PI=1 e2e/real-pi-smoke.sh  # REAL provider, explicit opt-in, may incur cost
E2E_REAL_CLAUDE_HAIKU=1 e2e/real-claude-haiku-smoke.sh  # one authorized REAL Haiku/low attempt
```

**Keep mode** (`--keep`, or `E2E_KEEP=1`): skips session stop/delete **and** workspace close,
so each scenario's disposable workspace stays inside its kept session for inspection.
Scenario-level daemons/temp dirs are still cleaned up; cleanup failures still propagate
(only the explicitly kept Herdr session/workspace artifacts are exempt). At the end
`run-all.sh` prints a review block per kept session — the `herdr session attach <name>` line
and the `herdr session stop <n> && herdr session delete <n>` cleanup one-liner.

Exit codes: scenario `0` = PASS, `3` = SKIP (missing precondition), anything else =
FAIL. `run-all.sh` exits non-zero if any scenario FAILED. The suite is **not** part
of CI (it needs Herdr 0.7.5 to boot a real ephemeral protocol-17 server) — it is
run by a human/orchestrator. The real-Claude smoke stages only completed onboarding/theme,
exact workspace trust, the installed Herdr hook, credentials, and approved
`remote-settings.json`, so startup dialogs cannot consume `agent.prompt`; it copies no broad
personal Claude state. Its intended contract is one authorized attempt with no retry or fallback.

## Files

| File | Role |
|---|---|
| `lib.sh` | Shared harness sourced by every scenario: logging, isolated stack, cleanup registry, daemon + workspace helpers, pollers (`wait_ok`/`wait_runs`), JSON/`hrpc`/`brpc`/`col_create` helpers. |
| `fake-agent.sh` | Config-defined fake harness used by scenarios 01–10 and 13–15. |
| `fake-bin/pi` / `fake-bin/claude` | Executables exposed only inside disposable standard-E2E Herdr servers/workspaces. They emulate interactive readiness/session reports, require the exact `agent.prompt` bytes before completion, record evidence under isolated temp, and never modify installed tools or call a provider. |
| `16-managed-p17.sh` | Managed pane-first Pi/Claude protocol-17 launch and no-provider boundary. |
| `17-configured-p17-runner.sh` | Unmanaged configured-command `pane run` bridge and exact argv/env evidence. |
| `real-pi-smoke.sh` | Fail-closed real-provider poem smoke. Detects Pi's runtime default model, passes low thinking, isolates board/Pi session output under `/tmp`, verifies integration/WezTerm, poem/comments/argv/git/settings, and supports keep mode for visual audit. Not in `run-all.sh`. |
| `real-claude-haiku-smoke.sh` | Fail-closed intended-contract smoke. Requires exact opt-in, authorizes one Claude Haiku/low attempt with no retry/fallback, stages only completed onboarding/theme, exact workspace trust, the installed Herdr hook, credentials, and approved remote-settings bytes under `/tmp` so startup dialogs cannot consume `agent.prompt`; no broad personal Claude state is copied. Independently identity-gates the daemon and Herdr server and cleans exact resources. Not in `run-all.sh`. |
| `hrpc.py` | One-shot raw **herdr** socket RPC (honours `HERDR_SOCKET_PATH`) for structural asserts (`tab.list`/`pane.list`/`pane.layout`). |
| `12-cwd-boards.sh` | Scoped board identity/isolation plus real TUI title/picker. |
| `13-jump-to-pane.sh` | Same-session pane focus through a real plugin overlay. |
| `NN-*.sh` | The scenarios above. |
| `run-all.sh` | Builds once, runs every scenario in order, prints the summary. |

Columns have no `board` CLI verb, so scenarios configure them over the boardd
socket via `scripts/board-rpc.py` (wrapped by `lib.sh`'s `col_create` / `brpc`).
