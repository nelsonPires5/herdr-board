# Live e2e scenarios

The end-to-end suite for herdr-board. Each scenario drives a **real** herdr (the
`HerdrSpawner`) with a **fake harness** (`fake-agent.sh`) dispatched into
**disposable** workspaces, on a fully isolated stack (its own temp DB + socket +
config), and tears everything down on exit. This is the only test layer that
exercises the herdr wire integration end to end.

For the layers below this one (unit, daemon+CLI integration, TUI snapshots), the
isolation/safety design, and the **how-to-write-a-scenario** guide, see
[`../docs/testing.md`](../docs/testing.md). This file is the authoritative use-case catalog for board protocol v1 / SQLite schema v13:
every numbered scenario from **01 through 30** must appear here and in `run-all.sh`. The provider-free
safe boundary is `fake-agent.sh`,
`fake-bin/{pi,claude}`, and `test-harness.sh`; prompt/system-prompt contents are never logged.
Scenario 21 is the active-run timer/event-refresh characterization. The CI live gate is configured to
exercise the complete catalog after the cheaper static checks succeed.

## Use case ↔ scenario ↔ status

| Use case | Scenario file | Status |
|---|---|---|
| Happy path: dispatch → run → outcome/comment (CLI) **and** create-a-card via the TUI | `01-core.sh` | live |
| Per-card tabs: several cards in one auto column use separate stable `card-<id>` tabs with shell anchors and run children | `02-kanban-grid.sh` | live |
| Multi-session: session/space scoping + cross-session dispatch against a **second** session the scenario boots itself; a daemon-created `new_workspace` adopts its own initial tab as the card tab (no unused tab left behind) | `03-sessions.sh` | live |
| `board done --outcome fail` → card follows the column's `on_fail_column_id` | `04-fail-on-fail.sh` | live |
| `board retry` re-runs a finished card as a NEW run row | `05-retry.sh` | live |
| Configured harness exits without `board done`; generated runner reports the silent exit → run failed, **no** auto-transition | `06-silent-exit.sh` | live |
| `board cancel` on a live run kills the herdr pane; run `cancelled`, card `failed` | `07-cancel.sh` | live |
| Run overruns its column `timeout_minutes` → killed and follows `on_fail` | `08-column-timeout.sh` | live |
| A stage-1 comment flows into the stage-2 run's prompt (`## Card comments` section) | `09-comment-context.sh` | live |
| Archive filter cycles scoped `ACTIVE/ALL/ARCHIVED` Herdr pane titles, keeps direct filter chips, and removes the persistent footer hint | `10-archive-filter-title.sh` | live |
| Built-in Pi mint/retry argv, session fork, protocol prompt, and agent comment through real Herdr | `11-pi-harness.sh` | live, checked-in fake `pi`, zero provider cost |
| Git-root/CWD board identity, independent pipelines/cards, scoped TUI title, and Global picker entry | `12-cwd-boards.sh` | live |
| Canonical CLI `card run focus` and card-detail `o` focus one **selected** run of a two-run card: `o` on the older run (whose pane was reclaimed) is refused without closing the overlay, `o` on the newest one focuses its held pane and closes the real plugin overlay | `13-jump-to-pane.sh` | live |
| A column `harness_override` (TUI select) drives a run via a config-defined harness; `harness.list` advertises config harnesses; effort/permission overrides flow into the run argv | `14-column-config.sh` | live |
| Integration-style status reports on a live managed pane: blocked → working → end-of-turn idle (Herdr derives `done`) → `awaiting` (`agent_done`), timeout paused; `board done ok` → `done` in the same column | `15-awaiting.sh` | live |
| Managed protocol-19/current Pi + Claude: pane-first placement, exact 0600 system file, readiness/session reports, exact `agent.prompt` task delivery, and held layout where each managed tab converges to exactly one harness pane (no anchor) | `16-managed-p17.sh` | live, checked-in fake `pi` + `claude`, zero provider cost; historical filename |
| Unmanaged protocol-19/current configured harness: exact argv/multiline env/cwd/socket bridge through CLI-only `pane run`, held layout, explicit completion | `17-configured-p17-runner.sh` | live, temporary runner, zero provider cost; historical filename |
| Nullable omitted/null/value semantics, merged capability validation, atomic rejection, and provider-free dispatch after clears | `18-nullable-clear.sh` | live, zero provider cost |
| Daemon starts before Herdr; late supervisor connection observes one exact pane exit | `19-daemon-before-herdr.sh` | live, zero provider cost |
| Proxy outage/restart preserves `Unknown` and timeout budget; reconnect snapshot repairs an event gap once | `20-herdr-recovery.sh` | live, zero provider cost |
| Active-run summary survives a card timestamp update and drives the timer in the real TUI | `21-active-run-timer.sh` | live, zero provider cost |
| The `M` (Shift+m) TUI mini-mode reorders the focused column via one `column.reorder` (Enter commits, Esc cancels) | `22-move-column-tui.sh` | live, zero provider cost |
| `agent_pane_busy` transient retry reuses one owned child after one anchor split; persistent busy cleans only that child and leaves the shell anchor intact; a successful managed launch closes its anchor so the tab holds only the harness pane | `23-agent-pane-busy-retry.sh` | live, zero provider cost |
| A card moves to a column of another board via `card.move` with `board_id` (atomic transfer, both columns recompacted); a board/column mismatch is rejected with nothing written | `24-cross-board-move.sh` | live, zero provider cost |
| Exact per-card tab/anchor ownership: duplicate labels are ignored, restart reuses the durable tab and shell anchor, and closed tabs/anchors are recreated safely | `25-card-tabs.sh` | live, zero provider cost |
| `LayoutMode::Compact` (< 60 cols) against the real TUI forced to a 40-col pane: three-row header (global count, board/filter controls, focused column with `(M/A)` and `n/N`), a focused-column card title, `b` opening the switcher directly at the board list, the `[ X ]` close affordance, and `Esc` closing the sheet outright | `26-compact-mobile.sh` | live, zero provider cost |
| A run whose pane was closed is reopened by resuming its harness conversation in a new ephemeral pane in the card tab (`action=rescued`), a second focus reuses that pane (`action=focused_rescued_pane`) without creating a second one, a managed rescue closes its anchor so the recreated card tab converges to exactly one harness pane, the `runs` row stays byte-for-byte unchanged, and a run that cannot be resumed is refused non-destructively | `27-rescue-dead-pane.sh` | live, zero provider cost |
| Pi's authenticated file catalog applies sparse/null `thinkingLevelMap` semantics in `harness.capabilities`, and the real TUI cycles the exact corrected effort order without submitting a card or launching Pi | `28-pi-effort-catalog.sh` | live, provider-free; no model invocation |
| Private daily NDJSON diagnostics prune only exact owned files beyond 30 days and record redacted board/Herdr success, error, and subscription metadata | `29-diagnostic-logs.sh` | live, provider-free; no payloads or model invocation |
| A non-fresh auto chain reuses ONE managed agent pane (and one conversation) across stages via `agent.prompt` (no new `pane.split`/`agent.start`); the managed tab stays anchorless with exactly one harness pane; a fresh column still mints a new pane, and a manual landing keeps the pane open | `30-pane-reuse.sh` | live, checked-in fake `pi`, zero provider cost |

### How the live scenario produces Herdr `done`

Herdr 0.8.0 / protocol 19 exposes `done` as an output `AgentStatus`, but its
supported integration input, `pane.report_agent`, accepts only
`idle|working|blocked|unknown` (`herdr pane report-agent --help` and `herdr api
schema --json`). Pi integration v8 uses that API with `source=herdr:pi` and
reports `working`/`blocked`/`idle`; there is no supported `herdr ... --state
done` argv to inject. On a managed `agent.start` pane, Herdr
derives the output status `done` from the integration's end-of-turn idle report.
The live scenario reproduces that supported path and asserts Herdr
`agent_status=done`, board `awaiting_reason=agent_done`, an open run/live pane,
paused timeout, and explicit confirmation to board `done`.

`crates/board-daemon/src/watchers/` additionally covers idle grace →
`awaiting` (`idle_expired`), working/blocked signals, timeout pause, and pane exit
deterministically through an injected `check_at(now)` seam. One live scenario
is sufficient to prove the real Herdr event subscription and signal-application
path without a provider call or an unsupported status injection. The separate
opt-in real-Pi smoke records live working status when observable but does not
require the sample because a fast provider response can finish between polls.

## Prerequisites

- **Exactly Herdr 0.8.0 / socket protocol 19**, `python3`, and Bash ≥4. The provider-free standard suite supports Linux and macOS; `run-all.sh` resolves absolute Herdr and Bash paths before applying its controlled `PATH`. Every scenario checks both `herdr --version` and the ephemeral server's `ping` before dispatch; older and unknown/future protocols are rejected. Your real sessions are never
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
their normal self-removing script. The forced-build standard suite is configured and required to exercise
scenarios 01–30 without provider calls; this is a coverage requirement, not a claim that a live run
has completed. Scenarios 18–29 use only the configured or managed
fake harnesses and never record prompt or system-prompt bodies.

## Running

```bash
e2e/run-all.sh              # standard suite; fake Pi + Claude, no provider/model cost
bash e2e/ci.sh              # CI-equivalent pinned install, --require-all, artifact export
e2e/run-all.sh --keep       # keep sessions + each scenario's workspace for review
e2e/run-all.sh 04 07        # only scenarios whose filename matches a filter
scripts/e2e.sh              # compat wrapper -> e2e/run-all.sh
bash e2e/test-harness.sh    # static cross-platform safety gate; starts no Herdr
bash e2e/04-fail-on-fail.sh # run a single scenario standalone (boots its own session)
E2E_REAL_PI=1 e2e/real-pi-smoke.sh  # REAL provider, explicit opt-in, may incur cost
E2E_REAL_PI=1 E2E_REAL_PI_MODEL=openai-codex/gpt-5.3 E2E_REAL_PI_EFFORT=high e2e/real-pi-smoke.sh  # explicit model/effort overrides (validated in `pi --list-models`)
E2E_REAL_PI=1 E2E_REAL_PI_NEW_WORKSPACE=1 e2e/real-pi-smoke.sh  # card creates its workspace via new_workspace; asserts one card tab + one Pi pane
E2E_REAL_CLAUDE_HAIKU=1 e2e/real-claude-haiku-smoke.sh  # one authorized REAL Haiku/low attempt
```

**Keep mode** (`--keep`, or `E2E_KEEP=1`): skips session stop/delete **and** workspace close,
so each scenario's disposable workspace stays inside its kept session for inspection.
Scenario-level daemons/temp dirs are still cleaned up; cleanup failures still propagate
(only the explicitly kept Herdr session/workspace artifacts are exempt). The exact kept session name remains in each scenario artifact directory for review and explicit cleanup.

Exit codes: scenario `0` = PASS, `3` = SKIP (missing precondition), anything else =
FAIL. `run-all.sh` captures the scenario side of its logging pipeline via `PIPESTATUS[0]` and exits
non-zero if any scenario failed; `--require-all` also converts SKIP to failure. Per-scenario logs,
status, exact owned session name, and sanitized manifest events are written below the run artifact root. In CI,
`e2e/ci.sh` downloads or reuses the exact SHA-verified Herdr 0.8.0 Linux x86_64 asset, verifies
protocol 19, invokes `run-all.sh --require-all`, and validates the one private artifact root printed
by that invocation before copying it to the deterministic `e2e-artifacts/` upload directory. Runner
and scenario evidence is uploaded even on failure and retained for 30 days. The same wrapper is the
local CI equivalent; it never runs either real-provider smoke. The real-Claude smoke stages only completed onboarding/theme,
exact workspace trust, the installed current Claude integration v7 hook, credentials, and approved
`remote-settings.json`, so startup dialogs cannot consume `agent.prompt`; it copies no broad
personal Claude state. Its intended contract is one authorized attempt with no retry or fallback. The real-Claude smoke retains its independent Linux `/proc` identity implementation and is outside the portable standard-suite guarantee.

## Files

| File | Role |
|---|---|
| `lib.sh` | Shared harness sourced by every scenario: logging, isolated stack, cleanup registry, daemon + workspace helpers, pollers (`wait_ok`/`wait_runs`), JSON/`hrpc`/`brpc`/`col_create` helpers. |
| `test-harness.sh` | Deterministic Linux/macOS shell safety checks for signed ownership tokens, key scrubbing, every exact-resource ledger kind, replacement, malformed record, and standalone parity; starts no Herdr resources. |
| `process_identity.py` | Standard-library platform backend: Linux `/proc`; Darwin `libproc`/`KERN_PROCARGS2`; exact argv/start/executable capture and HMAC verification. |
| `fake-agent.sh` | Config-defined/shared fake harness used by provider-free configured-harness scenarios. |
| `fake-bin/pi` / `fake-bin/claude` | Executables exposed only inside disposable standard-E2E Herdr servers/workspaces. They emulate interactive readiness/session reports, require the exact `agent.prompt` bytes before completion, record evidence under isolated temp, and never modify installed tools or call a provider. |
| `16-managed-p17.sh` | Managed pane-first Pi/Claude protocol-19/current launch, no-provider boundary, and anchorless managed tabs that converge to exactly one harness pane (historical filename). |
| `17-configured-p17-runner.sh` | Unmanaged protocol-19/current configured-command `pane run` bridge and exact argv/env evidence (historical filename). |
| `18-nullable-clear.sh` | Nullable clearing, merged validation, atomic rejection, and post-clear configured dispatch; no prompt-body logging. |
| `19-daemon-before-herdr.sh` | Late Herdr availability and exact pane-exit observation. |
| `20-herdr-recovery.sh` / `herdr-proxy.py` | Controllable owned proxy for conservative outage/restart, dropped-stream recovery, and typed `agent_pane_busy` fault injection. |
| `21-active-run-timer.sh` | Real-TUI active-run timer and event-refresh check; provider-free. |
| `22-move-column-tui.sh` | TUI column reorder mini-mode and one committed `column.reorder`; provider-free. |
| `23-agent-pane-busy-retry.sh` | Transient/persistent `agent_pane_busy`: same-pane retry, no second split, owned-child cleanup, and anchor closure after a successful managed launch (persistent failure preserves the anchor). |
| `24-cross-board-move.sh` | Cross-board `card.move` transfer with source/destination recompaction and mismatch rejection; provider-free. |
| `25-card-tabs.sh` | Exact per-card tab and shell-anchor ownership across duplicate labels, daemon restart, anchor reuse, and closed-tab recreation; provider-free. |
| `26-compact-mobile.sh` | Real TUI forced to a 40-col `LayoutMode::Compact` pane: three-row header with global count, board/filter controls, and focused-column `(M/A)` navigator, focused-column card title, `b` opening the switcher directly at the board list, the `[ X ]` close affordance, and `Esc` closing the sheet outright; provider-free. |
| `27-rescue-dead-pane.sh` | `run.focus` rescue of a run whose pane is gone: resume-mode relaunch of the managed fixture with the recorded conversation id (no re-sent task), idempotent second focus, anchorless convergence of the recreated managed tab (the rescue closes its anchor), a byte-for-byte unchanged `runs` row, and an explicit non-destructive refusal for an unresumable run; provider-free. |
| `28-pi-effort-catalog.sh` | Auth-scoped Pi file discovery plus exact board-RPC and real-TUI proof of sparse/null thinking-level semantics; never starts Pi or submits a card. |
| `29-diagnostic-logs.sh` | Daily NDJSON validity, private modes, exact 30-day retention boundary, board/Herdr completion metadata, sentinel absence, and exact cleanup. |
| `30-pane-reuse.sh` | Non-fresh auto-chain pane reuse: one managed agent pane + one conversation across resume hops (no second `pane.split`/`agent.start`), anchorless managed tab with exactly one harness pane, fresh column still mints, manual landing keeps the pane open; checked-in fake `pi`. |
| `real-pi-smoke.sh` | Fail-closed real-provider poem smoke. Detects Pi's runtime default model (`E2E_REAL_PI_MODEL` overrides it, validated in `pi --list-models`), passes `E2E_REAL_PI_EFFORT` (default `low`) as the thinking level consistently in card create + evidence, and `E2E_REAL_PI_NEW_WORKSPACE=1` switches to a daemon-created `new_workspace` space (discovered for cleanup; final structure is exactly one `card-<id>` tab with one Pi harness pane and no anchor/unused tab). Isolates board/Pi session output under `/tmp`, verifies the current Pi integration v8/WezTerm, poem/comments/argv/git/settings, and supports keep mode for visual audit. Not in `run-all.sh`. |
| `real-claude-haiku-smoke.sh` | Fail-closed intended-contract smoke. Requires exact opt-in, authorizes one Claude Haiku/low attempt with no retry/fallback, stages only completed onboarding/theme, exact workspace trust, the installed current Claude integration v7 hook, credentials, and approved remote-settings bytes under `/tmp` so startup dialogs cannot consume `agent.prompt`; no broad personal Claude state is copied. Independently identity-gates the daemon and Herdr server and cleans exact resources. Not in `run-all.sh`. |
| `hrpc.py` | One-shot raw **herdr** socket RPC (honours `HERDR_SOCKET_PATH`) for structural asserts (`tab.list`/`pane.list`/`pane.layout`). |
| `12-cwd-boards.sh` | Scoped board identity/isolation plus real TUI title/picker. |
| `13-jump-to-pane.sh` | Canonical CLI and same-session TUI focus of a deliberately selected run through a real plugin overlay. |
| `NN-*.sh` | The scenarios above. |
| `run-all.sh` | Builds once, runs scenarios 01–30 as environment-scrubbed children with their own sessions, captures artifacts, and prints the summary (`--require-all` forbids skips). |
| `ci.sh` | Pins, caches, and verifies Herdr for Linux x86_64; runs the complete suite with `--require-all`; exports only its exact private artifact root to `e2e-artifacts/`. |

Columns have no `board` CLI verb, so scenarios configure them over the boardd
socket via `scripts/board-rpc.py` (wrapped by `lib.sh`'s `col_create` / `brpc`). The
scenario contract follows repository ownership boundaries: typed board requests/config enter through
the CLI/TUI, SQLite remains daemon-owned, Herdr placement/process ownership remains in
`board-daemon`, and the harness ledger authorizes only exact captured resources.
