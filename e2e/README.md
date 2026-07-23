# Live e2e scenarios

The end-to-end suite for herdr-board. Each scenario drives a **real** herdr (the
`HerdrSpawner`) with a **fake harness** (`fake-agent.sh`) dispatched into
**disposable** workspaces, on a fully isolated stack (its own temp DB + socket +
config), and tears everything down on exit. This is the only test layer that
exercises the herdr wire integration end to end.

For the layers below this one (unit, daemon+CLI integration, TUI snapshots), the
isolation/safety design, and the **how-to-write-a-scenario** guide, see
[`../docs/testing.md`](../docs/testing.md). This file is the authoritative use-case catalog for board protocol v1 / SQLite schema v11:
every numbered scenario from **01 through 21** must appear here and in `run-all.sh`. The provider-free
safe boundary is `fake-agent.sh`,
`fake-bin/{pi,claude}`, and `test-harness.sh`; prompt/system-prompt contents are never logged.
Scenario 21 is the active-run timer/event-refresh characterization. The catalog describes the live
gate, but this cleanup task runs only the static harness—not the full live suite.

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
| Nullable omitted/null/value semantics, merged capability validation, atomic rejection, and provider-free dispatch after clears | `18-nullable-clear.sh` | live, zero provider cost |
| Daemon starts before Herdr; late supervisor connection observes one exact pane exit | `19-daemon-before-herdr.sh` | live, zero provider cost |
| Proxy outage/restart preserves `Unknown` and timeout budget; reconnect snapshot repairs an event gap once | `20-herdr-recovery.sh` | live, zero provider cost |
| Active-run summary survives a card timestamp update and drives the timer in the real TUI | `21-active-run-timer.sh` | live, zero provider cost |

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

- **Exactly Herdr 0.7.5 / socket protocol 17**, `python3`, and Bash ≥4. The provider-free standard suite supports Linux and macOS; `run-all.sh` resolves absolute Herdr and Bash paths before applying its controlled `PATH`. Every scenario checks both `herdr --version` and the ephemeral server's `ping` before dispatch; protocol 16 and unknown/future protocols are rejected. Your real sessions are never
  touched — the suite boots its own **ephemeral** Herdr server/session.
- `cargo` on `PATH` — `run-all.sh` builds the release `board` binary once
  (`scripts/build.sh`); scenarios reuse it.
- **No second session needed.** `03-sessions.sh` boots its own second ephemeral
  collision-resistant session (`hb-e2e-<scenario-b>-<pid>-<random64>`) and tears it down;
  it no longer discovers or skips.

## Ephemeral session model

Each scenario generates a bounded collision-resistant
`hb-e2e-<slug>-<pid>-<random64>` name (slug ≤8 characters; 64-bit cryptographic suffix) and
preflights that exact name in its marker-gated `/tmp/h<random32>` HOME before launching the
verified `herdr --session <name> server` argv. A live Herdr socket with the exact name is a collision; registry
enumeration/parse failures fail closed, while stale or non-Herdr sockets are reported as stale.
The isolated boardd binds to the newly started session
(`HERDR_SOCKET_PATH`), so that session is the daemon's "default", and every herdr CLI
call + `hrpc` assert targets it. Boot/readiness, mutation, board-daemon signals, workspace close, and session stop/delete use a versioned signed identity token containing PID, start time, parent, executable, and complete argv; PID liveness alone never authorizes an operation. Linux reads those fields and the owner environment token from `/proc`. macOS uses `proc_pidinfo`, `proc_pidpath`, and `KERN_PROCARGS2`; because Darwin does not expose another process's owner environment, it requires an HMAC-signed exact direct-child capability before adopting the exact server/daemon/helper transition. The random signing key is scrubbed from the scenario environment before any target starts, never written or logged, and reaches the verifier only over an inherited file descriptor. Immediately after spawn, before full server capture can settle, cleanup is armed and deferred from that stable direct-child capability before the race-prone provisional ledger check. Its fresh verifier accepts only the captured launcher identity or that same child's exact expected Herdr executable and `--session <name> server` argv after exec, so every registration/transition failure terminates and reaps only the spawned child.
Scenario Herdr CLI/RPC mutations use identity-gated wrappers; board commands that can trigger Herdr
verify boardd and the exact target session immediately before the request. Primary and secondary
sessions have independent roots, PIDs, sockets, and tokens. `run-all.sh`
never boots or exports a shared session: it scrubs inherited session/plugin/provider variables and
each child follows exactly the same boot and teardown path as a **standalone** scenario. Teardown stops+deletes only while that owner identity remains
valid. Stop requires a fresh full token; delete is separately authorized only after the captured
process is gone and the exact private registry name/ownership marker matches. It never scans a prefix
or cleans a coincident replacement. Cleanup failures propagate, so a successful scenario cannot hide
failed cleanup. The append-only resource ledger records full identity tokens for session servers,
boardd, and any helper/proxy; marker hashes for scenario/managed roots and workspace ownership;
and bounded configured-runner/temp-script paths plus non-sensitive content digests. Marker/script digests are rechecked by audit and immediately
before destructive cleanup. Scenario and managed paths require bounded mode-0700 roots with strict
header/current-token/owner markers and process-local reuse. `run-all.sh` refuses `E2E_ARTIFACT_ROOT`
and always creates a fresh private exact artifact root, so it never writes to or changes a pre-existing path.
Both early roots are ledgered and deferred before any fake-managed pre-init failure. Replacement generations and releases are validated,
and both standalone cleanup and `run-all.sh` run the same kind-specific audit. Audits use only exact
ledger entries—never a prefix scan, process-name search, or user inventory—and malformed ledgers
fail closed. Prompt/system-prompt files and content are intentionally never individually recorded.
Standard children start from an environment allowlist with a fixed system-tool `PATH`, comprehensively
scrubbing inherited provider credentials, endpoints, opt-ins, and shell functions after resolving Herdr absolutely.

The provider-free harness uses a mode-`0700` `/tmp/hb-e2e-managed.XXXXXX` root with a marker,
controlled `HOME`, `ZDOTDIR`, rc files, `PATH`, and fake-provider functions; it never sources user
rc files. Herdr is resolved to an absolute path before the managed pane `PATH` is narrowed.
Cleanup removes the marked roots only for their exact primary-session owner and refuses malformed,
unmarked, or out-of-root paths. Named-session sockets must be at most 92 bytes, leaving at least
15 bytes below Linux's 108-byte AF_UNIX limit. The board DB/socket/config and scope remain under the separate
short `/tmp/hb-e2e.XXXXXX` isolated root. `TMPDIR` is pinned to that exact marker-owned root, so
generated configured-harness scripts remain contained even if asynchronous `pane run` never opens
their normal self-removing script. The forced-build standard suite passes
scenarios 01–21 without provider calls. Scenarios 18–21 use only the configured fake harness and
never records prompt or system-prompt bodies.

## Running

```bash
e2e/run-all.sh              # standard suite; fake Pi + Claude, no provider/model cost
e2e/run-all.sh --keep       # keep sessions + each scenario's workspace for review
e2e/run-all.sh 04 07        # only scenarios whose filename matches a filter
scripts/e2e.sh              # compat wrapper -> e2e/run-all.sh
bash e2e/test-harness.sh    # static cross-platform safety gate; starts no Herdr
bash e2e/04-fail-on-fail.sh # run a single scenario standalone (boots its own session)
E2E_REAL_PI=1 e2e/real-pi-smoke.sh  # REAL provider, explicit opt-in, may incur cost
E2E_REAL_CLAUDE_HAIKU=1 e2e/real-claude-haiku-smoke.sh  # one authorized REAL Haiku/low attempt
```

**Keep mode** (`--keep`, or `E2E_KEEP=1`): skips session stop/delete **and** workspace close,
so each scenario's disposable workspace stays inside its kept session for inspection.
Scenario-level daemons/temp dirs are still cleaned up; cleanup failures still propagate
(only the explicitly kept Herdr session/workspace artifacts are exempt). The exact kept session name remains in each scenario artifact directory for review and explicit cleanup.

Exit codes: scenario `0` = PASS, `3` = SKIP (missing precondition), anything else =
FAIL. `run-all.sh` captures the scenario side of its logging pipeline via `PIPESTATUS[0]` and exits
non-zero if any scenario failed; `--require-all` also converts SKIP to failure. Per-scenario logs,
status, exact owned session name, and sanitized manifest events are written below the run artifact root. The suite is **not** part
of CI (it needs Herdr 0.7.5 to boot a real ephemeral protocol-17 server) — it is
run by a human/orchestrator. The real-Claude smoke stages only completed onboarding/theme,
exact workspace trust, the installed Herdr hook, credentials, and approved
`remote-settings.json`, so startup dialogs cannot consume `agent.prompt`; it copies no broad
personal Claude state. Its intended contract is one authorized attempt with no retry or fallback. The real-Claude smoke retains its independent Linux `/proc` identity implementation and is outside the portable standard-suite guarantee.

## Files

| File | Role |
|---|---|
| `lib.sh` | Shared harness sourced by every scenario: logging, isolated stack, cleanup registry, daemon + workspace helpers, pollers (`wait_ok`/`wait_runs`), JSON/`hrpc`/`brpc`/`col_create` helpers. |
| `test-harness.sh` | Deterministic Linux/macOS shell safety checks for signed ownership tokens, key scrubbing, every exact-resource ledger kind, replacement, malformed record, and standalone parity; starts no Herdr resources. |
| `process_identity.py` | Standard-library platform backend: Linux `/proc`; Darwin `libproc`/`KERN_PROCARGS2`; exact argv/start/executable capture and HMAC verification. |
| `fake-agent.sh` | Config-defined fake harness used by scenarios 01–10 and 13–15. |
| `fake-bin/pi` / `fake-bin/claude` | Executables exposed only inside disposable standard-E2E Herdr servers/workspaces. They emulate interactive readiness/session reports, require the exact `agent.prompt` bytes before completion, record evidence under isolated temp, and never modify installed tools or call a provider. |
| `16-managed-p17.sh` | Managed pane-first Pi/Claude protocol-17 launch and no-provider boundary. |
| `17-configured-p17-runner.sh` | Unmanaged configured-command `pane run` bridge and exact argv/env evidence. |
| `18-nullable-clear.sh` | Nullable clearing, merged validation, atomic rejection, and post-clear configured dispatch; no prompt-body logging. |
| `19-daemon-before-herdr.sh` | Late Herdr availability and exact pane-exit observation. |
| `20-herdr-recovery.sh` / `herdr-proxy.py` | Controllable owned proxy for conservative outage/restart and dropped-stream recovery. |
| `21-active-run-timer.sh` | Real-TUI active-run timer and event-refresh check; provider-free. |
| `real-pi-smoke.sh` | Fail-closed real-provider poem smoke. Detects Pi's runtime default model, passes low thinking, isolates board/Pi session output under `/tmp`, verifies integration/WezTerm, poem/comments/argv/git/settings, and supports keep mode for visual audit. Not in `run-all.sh`. |
| `real-claude-haiku-smoke.sh` | Fail-closed intended-contract smoke. Requires exact opt-in, authorizes one Claude Haiku/low attempt with no retry/fallback, stages only completed onboarding/theme, exact workspace trust, the installed Herdr hook, credentials, and approved remote-settings bytes under `/tmp` so startup dialogs cannot consume `agent.prompt`; no broad personal Claude state is copied. Independently identity-gates the daemon and Herdr server and cleans exact resources. Not in `run-all.sh`. |
| `hrpc.py` | One-shot raw **herdr** socket RPC (honours `HERDR_SOCKET_PATH`) for structural asserts (`tab.list`/`pane.list`/`pane.layout`). |
| `12-cwd-boards.sh` | Scoped board identity/isolation plus real TUI title/picker. |
| `13-jump-to-pane.sh` | Same-session pane focus through a real plugin overlay. |
| `NN-*.sh` | The scenarios above. |
| `run-all.sh` | Builds once, runs scenarios 01–21 as environment-scrubbed children with their own sessions, captures artifacts, and prints the summary (`--require-all` forbids skips). |

Columns have no `board` CLI verb, so scenarios configure them over the boardd
socket via `scripts/board-rpc.py` (wrapped by `lib.sh`'s `col_create` / `brpc`). The
scenario contract follows repository ownership boundaries: typed board requests/config enter through
the CLI/TUI, SQLite remains daemon-owned, Herdr placement/process ownership remains in
`board-daemon`, and the harness ledger authorizes only exact captured resources.
