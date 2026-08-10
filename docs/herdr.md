# Learning herdr

herdr-board is a client of **herdr** (the terminal workspace manager it dispatches
agent panes into). This page is about learning and **verifying** herdr itself — its
commands, wire shapes, and events — so you never guess. It is not about our own
board CLI (that is [`skill/SKILL.md`](../skill/SKILL.md); see [below](#ours-vs-herdrs)).

There is **no man page** for herdr. Do not rely on memory or on this repo's prose
for a herdr fact you are about to depend on in code — read it live from the
installed binary.

## Live sources of truth

Query the herdr that is actually installed. These are authoritative; docs (this
repo's included) are only a cache.

| Source | What it gives you |
|---|---|
| `herdr api schema --json` | The **full** socket-API spec: every method, its params/result types, every event, and the top-level `protocol` number. This is the contract board-herdr speaks. Pipe to `python3 -m json.tool`/`jq` and search for the method or type in question. `herdr api schema --output PATH` writes it to a file. |
| `herdr <subcommand> --help` | Human-readable usage for a CLI verb and its flags — e.g. `herdr agent --help`, `herdr pane --help`, `herdr workspace --help`, `herdr session --help`. Use it to confirm flag names/spellings before pinning an argv. |
| `herdr api snapshot` | The **live** runtime state (sessions, workspaces, tabs, panes) of the running server — the ground truth for "what is actually open right now" when debugging placement or liveness. |
| `herdr --help` / `herdr status` | Top-level command list and whether a server/client is up. |

Rule of thumb (mirrors [AGENTS.md](../AGENTS.md)): **never assume a herdr
command, flag, or JSON shape from memory — verify against `api schema` /
`--help`, and pin the argv you verified in a test comment.**

## Compatibility gate: Herdr 0.8.0 / socket protocol 19

The supported matrix is exact: **Herdr 0.8.0**, **socket protocol 19**, board protocol
v1, and SQLite schema v13. `board-herdr` rejects a different Herdr version or
protocol before the daemon performs workspace discovery, pane placement, an agent
launch, a configured runner action, or a notification mutation. This is a policy
gate, not a protocol-negotiation fallback.

Use these read-only probes before changing a wire call or debugging a live session:

```bash
test "$(herdr --version)" = "herdr 0.8.0"
herdr api schema --json | python3 -c \
  'import json, sys; s=json.load(sys.stdin); assert s["protocol"] == 19, s'
herdr api snapshot
herdr integration status
```

`herdr api schema --output PATH` saves the same authoritative schema. Use the
specific command help before pinning an argv: `herdr agent start --help`,
`herdr agent prompt --help`, `herdr pane report-agent --help`, and
`herdr integration install --help` are especially relevant to board integration.

## herdr ships its own agent integrations

herdr can install its **own** per-harness integration hooks so a harness reports
live agent status (idle / working / blocked / done) back to herdr. Manage them with
`herdr integration <subcommand>`:

- `herdr integration install <name>` / `herdr integration uninstall <name>`
- `herdr integration status [--outdated-only]`

Herdr 0.8.0's installable integration targets are: **pi, omp, claude, codex,
copilot, devin, droid, kimi, opencode, kilo, hermes, qodercli, cursor,
mastracode, antigravity-cli, grok**. Get the authoritative spelling and current
list from `herdr integration install --help`; the socket schema uses
`antigravity_cli` for the hyphenated CLI target. On the host inspected for this
update, `herdr integration status` reports Pi **current (v8)** and Claude
**current (v7)**.

Herdr 0.8.0 also provides `herdr --skill`, which prints Herdr's own agent-driving
skill. It is distinct from this repository's `board skill` / `skill/SKILL.md`.

Installing one **writes into that harness's own config** (`pi` installs
`~/.pi/agent/extensions/herdr-agent-state.ts`; `claude` installs a hook under
`~/.claude`). Because it mutates personal configuration, **herdr-board never installs or
updates integrations** — running `herdr integration install <harness>` is a **user
prerequisite** for live lifecycle signals. Check the reported version with
`herdr integration status`; the board's current support matrix expects Pi v8 and
Claude v7 when those harnesses are dispatched. For codex, the integration is what
reports `AgentInfo.agent_session` (the thread id the board captures so a minted
conversation can later be resumed/forked); without it a codex run still executes
and completes, but its conversation cannot be reopened by id.

### 0.8.0 lifecycle implications

Herdr 0.8.0 tightened lifecycle ownership around confirmed process exit. A known
agent can restart with the same saved session and retain lifecycle state; nested
or ephemeral Codex sessions do not replace the owning pane's resumable session;
and Pi RPC/JSON/print processes do not claim lifecycle authority intended for a
Pi TUI session. These rules reduce false ownership changes, but Herdr status is
still a signal rather than board completion: `pane.exited` and explicit
`board done` remain the reliable run boundaries.

What the integration buys you:

- **With it**, herdr reports precise `working` / `blocked` / `done` (plus `idle`) per pane, and
  the board maps them to card statuses: `working` → `running`, `blocked` → `blocked`, and
  `done` without `board done` → `awaiting` (reason `agent_done`) for human review.
- **Without it (degraded mode)**, herdr's `working`/`blocked`/`done` signals don't exist. Spawn,
  explicit `board done`, column timeout, and pane-exit handling still work; the only lifecycle
  hint left is herdr's own `idle` status, so `awaiting` can only be reached via `idle_expired`
  (`idle` sustained past `idle_grace_seconds`). If the pane status stays `unknown`, even that
  watchdog never arms and the card simply stays `running` until `board done`, timeout, or pane
  exit.

To verify what a running herdr actually reports, inspect the live state:
`herdr api snapshot` (panes carry their current agent status), plus
`herdr integration status` for which integrations are installed/current.

Pi users who need precise live working/blocked/done status and session references must run
`herdr integration install pi`; the matching integration is a prerequisite for whichever harness
is being dispatched. Without it, the board continues in the degraded mode described above. The
standard E2E uses checked-in fake Pi and Claude executables and is designed
to exercise watcher status mapping deterministically rather than changing integrations or calling a provider.

## Protocol 19 delta: additive upstream surface

The Herdr 0.8.0/protocol-19 schema keeps the RPC shapes herdr-board uses unchanged.
The board's typed calls still use the same request/result envelopes for `ping`,
`session.snapshot`, workspace list/create/close, tab create/list/rename, pane
split/list/get/focus/rename/read/send-text/send-keys/close/layout, agent
start/get/prompt/wait, `notification.show`, and `events.subscribe`.

The delta is additive and outside the board client surface:

- `workspace.move_block` accepts `workspace_ids` plus an optional
  `before_workspace_id` for atomic block movement, including worktree groups.
- `workspace.reordered` is a new emitted event and subscription selector. Its
  data includes `workspace_ids`, `workspaces`, and an optional
  `before_workspace_id`.
- `antigravity_cli` (CLI spelling `antigravity-cli`) and `grok` are new
  integration targets with session reporting/native restore. They do not add
  board built-in harnesses.
- `AgentInfo.agent_session` carries an `AgentSessionInfo | null` reference —
  `{agent, kind, source, value}` with `kind` ∈ {`id`, `path`} — for panes whose
  integration reported a session. It is absent on panes without one. `board-herdr`
  decodes it (`AgentInfo.agent_session`) because the codex built-in harness needs
  the reported thread id: a codex Mint persists `session_id = NULL` at enqueue,
  and the daemon's bounded post-launch `agent.get` capture accepts only an
  `id`-kind reference owned by the codex agent with a non-empty value, promoting
  it atomically onto the run and card. A wrong-agent, `path`-kind, blank, or
  never-reported session degrades the capture to `None` (run still executes);
  the pane then keeps no recorded conversation id, so reuse/rescue fail closed.

The client does not call the new workspace method. It preserves unknown additive
events rather than treating them as a change to the used transport. The current
schema has 90 request methods, 26 emitted event kinds, and 27 subscription
selectors; re-check those facts with `herdr api schema --json` rather than
copying them from this page.

## Pane-first managed launch contract

The stable transport rule is pane-first and intentionally independent of a
protocol number: create or split the target pane with its cwd/environment first,
then start the agent in that existing pane. Under the exact Herdr 0.8.0 / socket
protocol 19 gate, herdr-board first creates a shell root for a new durable card
tab and reserves it as `card-<id>-anchor`. When the dispatch itself just created
the workspace (`new_workspace` with no matching open workspace), the workspace's
own initial tab/root is adopted instead: the exact bootstrap ids are verified
(still live, sole pane, no agent), the tab is renamed to `card-<id>` via
`tab.rename` and the root to `card-<id>-anchor` via `pane.rename`, then the run
child is split from it — a daemon-created workspace therefore has no unused
initial tab. The adopted tab/root ids are remembered in the daemon's per-card registry under the
allocation lock before the first allocation, so a failed split/launch or a later retry in the same
daemon recovers the adopted tab by exact id instead of creating a second one. It then splits a run child with the required
cwd/environment and calls `agent.start` with `{name, kind, pane_id, args,
timeout_ms}` on that child only. The anchor is never an `agent.start` target.
`kind` selects Herdr's canonical agent executable and `args` contains only that
executable's arguments. Workspace/tab/split/env placement fields are not part of
`agent.start`.

After a **successful** managed launch the daemon closes the anchor pane, leaving
exactly the harness pane visible (closing a split parent is live-verified safe:
the child keeps its process and environment), and persists no anchor id with the
run — and a successful `run.focus` rescue of a managed run does the same, with
the same `pane_not_found`-counts-as-closed / warn-and-keep semantics. Same-conversation reuse hops re-prompt that one harness pane; a later fresh
managed run recovers from the exact durable prior child with a temporary anchor
that is closed again after launch. Configured (unmanaged) harnesses keep their
persistent anchor, because `pane run` exits close their child. A failed managed
launch never closes the anchor.

After start, `agent.get <target>` exposes `interactive_ready` and
`launch_pending`. herdr-board waits for `interactive_ready=true` and
`launch_pending=false`, then submits the exact card task with `agent.prompt`
instead of startup argv or synthetic keystrokes. A newly allocated child can
briefly retain prior agent state — or a slow login shell can still be booting — and return typed
`agent_pane_busy`; the board
retries the exact `agent.start` request on that same child at most five times, with
bounded 100ms backoff doubling per retry (100/200/400/800/1600ms). It never allocates a second child for that
response. Persistent busy is a launch failure whose cleanup closes only the
owned child pane and leaves the anchor; `pane_not_found` is handled separately
as a placement race that restarts discovery from `tab.list` and retries
complete placement once. Schema v13 persists the exact anchor id with the run
(the anchor was introduced in v12) and adds comment audit state; after restart
both tab and anchor are selected only from scoped durable pane identities.
Labels are display metadata and never authorize a tab or pane.
`agent.read` remains a terminal screen/scrollback read, not a semantic result
channel.

**Codex is a managed kind with no prompt file.** `kind:"codex"` starts the
interactive Codex CLI with the same pane-first readiness contract, but there is
no system-prompt-file equivalent: startup argv carries neither system nor task
text (no `--append-system-prompt*`, no `--` delimiter), and the prompt travels
exclusively through `agent.prompt` after readiness — a Mint receives one
delimited block (`## herdr-board system instructions` then
`## herdr-board card task`), a resume/fork fresh pane the task alone, same-pane
reuse the task alone, a rescue nothing. Between readiness and the prompt, the
daemon bounded-polls `agent.get.agent_session` (5 probes / 10s) for the
integration-reported thread id and persists it atomically with the run
promotion; a missing or unusable report degrades to `None` (see the protocol-19
delta above).

The rescue depends on two runtime facts that are not guaranteed by the request
shapes: a pane label set with `pane.rename` must survive a subsequent
`agent.start`; and when a managed agent's process exits, Herdr must **clear**
`PaneInfo.agent` while keeping the pane open as a plain shell (label included).
The second is why "does this pane still have a registered agent" is a sound
liveness test and a label match alone is not. The current validation target is
Herdr 0.8.0/protocol 19; do not infer either behavior from the schema alone.
`PaneInfo.agent` is also not the exclusive name passed to `agent.start` — the
schema carries `agent` and `name` separately — so it is only ever tested for
presence, never compared to a board-chosen name.

Reopening a run whose pane was closed (`run.focus` rescue) reuses this exact
contract, not a second one: `board-daemon::spawner::rescue` calls the same
`allocate_owned_pane` placement (with an empty reclaim list, so reopening one run
never closes another's pane), the same `require_supported_protocol` gate, and
the same `agent.start` + `interactive_ready` wait. Three things differ, all
deliberate: `agent.prompt` is **not** sent (the conversation already contains the
task, and re-sending it would re-run the work); the new pane is renamed to
`card-<id>-r<run>-rescue` before launch so a later reopen can find it again with
one `pane.list` scan (a name built from stable ids only, never from a column
name that a user can change — the launch contract relies on `agent.start` leaving
that label intact, so it is set once); and the pane env omits `BOARD_RUN_ID`, since a
rescued pane must not hold the credential that writes to a finished run. Nothing is persisted for that pane —
Herdr's own pane label/agent name is the only record of it, which is why the dedup scan
is a hint rather than proof (see `docs/design.md` → Limitations). The dead
`pane_id` is never reused or revived.

Configured harnesses are intentionally unmanaged. The current Herdr CLI has a
`herdr pane run <PANE_ID> <COMMAND>...` command but the socket schema has no
`pane.run` method, so the daemon invokes that CLI against the selected session
socket via a temporary runner script. Agents must still use `board comment` and
`board done`; the configured runner reports a silent child exit back to boardd
as a failed run with no automatic column transition.

## 0.8.0 runtime behaviors behind live validation

The release notes describe behavior that a schema diff cannot prove. Keep these
checks separate from the exact compatibility gate and do not treat a local
read-only probe as an end-to-end test result:

- **Prompt settling:** Herdr waits briefly after sending prompt text before it
  presses Enter. The board therefore checks readiness first and uses
  `agent.prompt` for the exact task, rather than sending text and a synthetic
  key sequence. A managed fixture must observe the multiline task after
  readiness before it can finish.
- **Headless resume:** a headless server can resume a restored agent session
  without a TUI client attaching. `run.focus` rescue and restart recovery must
  work from the socket/session boundary, not depend on a desktop client being
  present.
- **Plugin-root resolution:** relative commands in a plugin manifest resolve
  from that plugin's root. Install/open validation should therefore use a
  disposable session and a caller-independent cwd; it should exercise the
  manifest's relative build/action paths, not only invoke a script from the
  checkout root.
- **Read truncation and history:** `pane.read` and `agent.read` report a
  `truncated` flag when older terminal rows are omitted. Herdr may automatically
  collect text history for idle alternate-screen agents and restore the
  application viewport. Board code treats reads as bounded screen/scrollback
  diagnostics, never as a semantic result or completion channel.

These behaviors motivate the live validation cases around lifecycle signals,
managed prompt delivery, rescue/resume, plugin opening, and pane reads. The
provider-free scenarios use disposable sessions and fake harnesses; this page
makes no claim that a live suite has been run for the current checkout.

## Socket request and subscription bounds

`board-herdr` opens a fresh bounded connection for each request. Writes and responses have local
socket deadlines; methods with a wire `timeout_ms` allow that timeout plus a small transport grace.
Only the response whose ID exactly matches the request can complete it; unrelated responses are
ignored, and protocol errors are surfaced only for the matching ID.

Event subscriptions use a persistent connection, but each subscribe/add acknowledgement is
bounded and must carry that subscribe request's exact ID. Events arriving before the matching
acknowledgement are buffered in order rather than lost. After an acknowledgement or bounded
`poll_event`, the temporary read timeout is removed so ordinary event iteration remains blocking.
Calls and subscription handshakes emit one metadata-only completion record (method, duration,
outcome, and stable error category/code); typed result decoding is inside that same completion boundary.
Method labels come from the crate's closed typed-method set (arbitrary public `call` methods are logged
as `<unknown>` without changing the wire method). Initial subscription connection/setup failures and
later add-subscription failures each emit exactly one error completion. Records never include request
parameters, response results, or event payload bodies. Polling successful events does not emit per-poll
diagnostics.

## Version drift

`board-herdr` deliberately exposes only the typed Herdr methods used by the daemon and tests:
workspace, tab, pane, agent, notification, session, and events. The upstream worktree methods and
DTOs are not part of this crate's public surface; repository isolation belongs in the agent prompt.
The checked-in schema fixture is regenerated from the installed Herdr contract and
is not rewritten during unrelated API cleanup. The board fixture and typed client
are currently pinned to **Herdr 0.8.0 / protocol 19**; board protocol v1 and DB
schema v13 remain independent and unchanged.

This repo's current Herdr facts — [`docs/research.md`](research.md),
[`docs/design.md`](design.md), and the wire shapes hard-coded in `board-herdr` —
must be rechecked with the installed binary before a support change. The local
0.8.0 inspection for this update used `herdr api schema --json`, the relevant
`--help` commands, `herdr api snapshot`, and `herdr integration status`; it did
not substitute an end-to-end test run.

Herdr updates independently of this repo (`herdr update`, stable/preview channels).
When something that used to work misbehaves on a newer herdr — an unknown method, a
changed field name, a new error code — **re-verify against `herdr api schema
--json` FIRST**, before patching board code. Confirm the current `protocol` number
and the exact method/type shape, then reconcile `board-herdr` (and update the
"verified against" note here and in `AGENTS.md`) to match. Editing code against a
remembered shape is how drift bugs get baked in.

## Ours vs herdr's

Two different things document two different tools — keep them straight:

- **`skill/SKILL.md`** documents **our** `board` CLI (cards, columns, comments,
  `board done`/`move`/`cancel`/`retry`, the daemon). It versions **with this repo**
  and changes when we change the board.
- **herdr's integrations and `api schema`** document **herdr itself**. They version
  with the installed herdr, independently of this repo.

When you need a board fact, read `skill/SKILL.md` / `docs/`. When you need a herdr
fact, read it live from herdr per the table above.
