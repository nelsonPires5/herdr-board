# herdr-board — design

Responsive TUI: the board is Compact `< 60` (one column, a three-row
identity/board-filter/navigator header plus a divider), Regular `60..=119`, and
Wide `>= 120` (multi-column, one-line header). Compact shows `(M)`/`(A)` next
to the focused column and keeps the global running count only in the identity
row. Cards are boxed rows with status glyph, active-run timer, and harness/model
metadata; Board cards intentionally have no Edit/Delete controls, while keyboard
`e`/`d` remains available. Every visible button is a transparent `[ NAME ]`
chip with bold white text and brackets; close controls are `[ X ]`, selected
chips add an underline, and card status retains semantic color. Card detail/forms/pickers occupy only the board content region, so top and bottom
board action rails stay visible. Card movement is drag-only (keyboard `m`/`H`/`L`
remain), and Refresh/Quit are keyboard-only (`r`/`q`). There is no persistent
footer hint row; transient toasts use a row above the action rail only while
visible.

## 1. Concepts

| Entity | What it is |
|---|---|
| **Project / Board** | A **project** is a named collection of boards identified by a canonical filesystem path (Git root or plain existing directory); `Global` is the special project (scope NULL) preserving the pre-v14 boards. Each project's first board is named `main`. A **board** is an independent pipeline (columns/config/cards); names are unique case-insensitively within one project, while board ids are global stable selectors. A fresh board contains **only a `Todo` column**; everything else is user-created. |
| **Column** | A stage, entirely user-defined: create/rename/reorder/delete from the TUI (keyboard or mouse). Config: `system_prompt`, `trigger` (`auto` = entering the column starts a run; `manual` = waits for human), `on_success` / `on_fail` (move card to column X, or stay), optional overrides (model/effort/harness) applied to every card passing through. Nothing about column names or count is hardcoded. |
| **Card** | A unit of work. Title, **description = the base prompt**, harness, model, effort, permission mode, a **herdr session** (`session`, null = daemon default) AND a **space** within it (`workspace` = an already-open workspace id; `new_workspace` = a label + cwd the daemon opens on first dispatch), position, live status (`idle · queued · running · blocked · awaiting · done · failed`), the harness `session_id` for resume, and an optional `archived_at` timestamp. Archiving is reversible and preserves comments/run history. `awaiting` (agent finished/went idle without `board done`, run still open, pending human review) records an `awaiting_reason` (`agent_done` / `idle_expired`); `done` is confirmed completion with no target column. |
| **Comment** | Timestamped note on a card. Author = `user`, `agent` (from a run), or `system` (daemon transitions). Comments are both the audit log **and** context for the next run. |
| **Run** | One agent execution of a card in a column: startup argv, enqueue-time task/system-prompt snapshots, herdr pane/workspace ids, session id, started/ended, exit status, result summary. Cards keep full run history (retries = new runs). |

Separation card ↔ run is deliberate (vibe-kanban converged on task/attempt/execution after painful migrations): a card can be re-run, moved back, or forked without losing history.

## 2. Architecture

```
┌───────────────────────────── herdr session ─────────────────────────────┐
│  ┌────────────── pane ─────────────┐   ┌───────── pane (ws w4) ───────┐ │
│  │  board TUI (herdr plugin pane)  │   │  pi … (card #42 run)         │ │
│  └───────────────┬─────────────────┘   └──────────────┬───────────────┘ │
└──────────────────┼─────────────────────────────────── │ ────────────────┘
                   │ board API (unix socket, JSON)      │ `board comment/done`
                   ▼                                    ▼
             ┌──────────────────────────────────────────────┐
             │            boardd (daemon)                   │
             │  SQLite (WAL) · run queue · column engine    │
             └───────┬──────────────────────────────────────┘
                     │ herdr socket API (~/.config/herdr/herdr.sock)
                     ▼
   ping · workspace.create · tab.create / pane.split
   agent.start · agent.get · agent.prompt · pane.rename / pane.close
   events.subscribe(pane_agent_status_changed, pane_exited) · pane.read
   notification.show
```

- **boardd** is the only SQLite writer. One DB at `~/.local/share/herdr-board/board.db` stores every independent scoped board; cards still explicitly target Herdr sessions/workspaces.
- **TUI** is packaged as a herdr plugin: `herdr-plugin.toml` declares a `[[panes]]` entry (herdr spawns the TUI binary in a split/tab) and `[[actions]]` (e.g. "add focused pane's repo as a card") bindable via `[[keys.command]]`. Plugin processes receive `HERDR_BIN_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`.
- **`board` CLI** subcommands hit the boardd socket — never SQLite directly (single-writer rule).
- boardd's always-on supervisor owns one persistent `events.subscribe` stream per resolved Herdr
  session socket. Each socket has independent subscription generation, reconnect backoff, and
  bounded snapshot reconciliation; a disconnected or late Herdr session never blocks another.

The physical modules follow these boundaries. `board-core::db` is split into migrations,
board/column, card/comment, run-UoW, and row-mapping modules; `board-core::engine/` owns pure
lifecycle, transition, validation, and signal decisions, while `board-core::client/` owns the
blocking client traits, Unix transport, and fake client. `board-daemon::dispatch/` separates
enqueue preparation, dispatch passes, finalization, and space resolution; `board-daemon::ops/`
organizes request handling by boards, columns, cards, comments, runs, and discovery; and
`board-daemon::watchers/` separates timeout, local liveness, signals, and Herdr event supervision.
`board-daemon::spawner/` owns local/Herdr launch and pane placement. Their private invariant tests
remain in the corresponding `src/**/tests/` modules rather than widening production visibility.
`board-herdr::events/` owns event parsing, backoff, and streaming, while its transport module owns
all unsafe AF_UNIX operations. `board-tui::app/`, `forms/`, and `view/` separate interaction state,
form construction/submission, and rendering. Public API behavior is tested from each crate's
`tests/` target; these splits do not change crate ownership, wire formats, or the public board
protocol. The CLI keeps clap arguments, daemon lifecycle, scope, helpers, and
card/run/column/discovery command wiring in separate modules.

### Root configuration and startup

`board-core::config::RootConfig` is the typed representation of the complete
`config.toml`: board fields remain at the document root for compatibility,
while daemon runtime knobs live under `[daemon]`. `RootConfig::load` reads the
resolved path once; a missing file or omitted section yields defaults. An
existing file is not optional: malformed TOML, type errors, and invalid enum
values (such as an unknown `spawner`) return `Error::Config`.

At the daemon edge, `RootConfig` is parsed once, then `DaemonSettings` applies
injected process-environment overrides (`BOARD_SPAWNER`,
`BOARD_TIMEOUT_UNIT_SECS`, `BOARD_LOCAL_POLL_MS`, and `BOARD_TICK_MS`) with
environment taking precedence. There is no second best-effort TOML parser and
no `unwrap_or_default` fallback, so board and daemon settings cannot disagree
about whether the document is valid.

### Herdr compatibility and launch boundary

The public boardd socket protocol remains **v1**. That is independent of the upstream Herdr socket
contract: this version supports **exactly Herdr 0.8.2 / protocol 20**. The daemon opens a fresh
Herdr request connection per operation, so compatibility is checked at each boundary rather than
only once at startup:

- **Status:** `daemon.status` re-pings its configured Herdr socket and reports `herdr_connected`
  only when both exact values match.
- **Event subscription:** the per-session supervisor gates a request connection before opening the
  persistent `events.subscribe` stream. The typed subscription acknowledgement must complete before
  the supervisor takes its first snapshot; an incompatible socket receives neither subscription
  nor snapshot work.
- **Notifications:** the detached `notification.show` effect performs the same compatibility probe
  immediately before sending the cosmetic notification, so an incompatible Herdr is not mutated.
- **Dispatch and discovery:** card space resolution pings before `workspace.list` or
  `workspace.create`; `space.list` and the read-only space preflight use the same gate. The spawner
  repeats it as the first call before `tab.create`, `pane.split`, a managed-agent launch, or the
  configured-harness runner. The session registry's `herdr session list --json` enumeration is a
  separate CLI discovery step; every selected socket is still gated before socket discovery or use.
- **Pane operations:** caller-targeted focus/title operations, durable placement, rescue discovery,
  and managed/configured launch all use a checked connection before `pane.get`, `pane.focus`,
  `pane.rename`, `pane.split`, `agent.start`, `agent.prompt`, or the configured runner.

The deliberate exception is cleanup and liveness for an already-owned pane handle: the spawner's
`kill`/`pane.close` and `is_alive`/snapshot paths retain an ungated connection so the daemon can
close or observe its own pane after Herdr becomes incompatible. That exception does not authorize
new discovery, placement, focus, notification, or launch; supervisor reconciliation and the
caller-visible focus/rescue paths remain checked. A mismatch on a checked dispatch path fails the
queued run without mutating the workspace.

The managed launch contract is pane-first. New durable runs place each card in a stable short
`card-<id>` tab. When the dispatch itself just created the workspace (`new_workspace` with no
matching open workspace), the workspace's own initial tab is **adopted** as the card tab: the
first card-tab allocation verifies the exact bootstrap tab/root ids are still live (root is the
tab's sole pane and carries no agent), renames the tab to `card-<id>` and the root to
`card-<id>-anchor`, and splits the run child from it — so a daemon-created workspace has no
unused initial tab. Any verification mismatch falls back to a fresh `tab.create` and never
touches that root; reused/existing/user workspaces never supply a hint.

The root of a card tab is reserved as a labeled shell anchor (`card-<id>-anchor`); it is never
an agent target. Every run, including the first, splits a child from that anchor with the run's `cwd`
and `env`, and only that child receives `agent.start` or the configured `pane run` bridge. The anchor
keeps a predictable strip: the prototype targets a 0.40 ratio for the initial split (clamped on
narrow terminals so the anchor remains reusable) and uses live geometry thereafter. Placement fails
closed below the minimum of 24x6 for the shell and 12x8 for the agent; it never launches an agent
on an undersized root or child.

**Managed tabs are deliberately anchorless.** After a *successful* managed (Pi/Claude/Codex/OpenCode) launch —
fresh, same-conversation reuse, or a `run.focus` rescue — the daemon closes the anchor pane, which
is live-verified safe
for its split child, so the tab holds exactly the one harness pane. The promoted run then persists
`herdr_anchor_pane_id` as NULL, and the shared registry's stale anchor entry is never treated as
live because anchor selection requires the exact pane to still exist. A failed launch never closes
the anchor (child-only cleanup, unchanged). A later fresh managed run in the converged tab
recovers from the exact durable prior child: it creates a temporary anchor by splitting that
child, splits the new run child (with its new `BOARD_RUN_ID` env), launches it, then closes the
temporary anchor and reclaims the ended prior child — one harness pane remains. Same-conversation
reuse eligibility is checked before anchor selection, so an anchorless managed tab still re-prompts
its exact prior harness pane. If the user closes the sole managed harness pane, Herdr removes the
tab and, when it was the last tab, the workspace; the next `new_workspace` dispatch then recreates
the workspace and adopts its fresh root. Configured (unmanaged) harnesses keep their persistent
anchor unchanged: `pane run` exits close their child, so the anchor is what the next run splits
from. Old live anchors from earlier releases converge on the next successful managed launch;
old durable child evidence remains valid tab proof.
Herdr labels are not unique, so neither tab nor anchor ownership is inferred from one. The current
schema v14 retains the exact anchor pane id introduced by v12 with each promoted run; after restart,
the daemon reconstructs the exact tab and anchor only from scoped durable pane identities in the same
session and workspace.
A renamed anchor remains owned by identity. If that exact anchor was closed, it is recreated only by
splitting a currently live durable board-run child; if no such proof remains, a fresh tab is created
instead of touching a foreign pane. Before a later split, exact ended run children may be reclaimed
to preserve anchor geometry; foreign or open panes are never closed. If older runs provide multiple
live identities, newest run id wins deterministically. A fresh/recovered split also requires enough
live geometry for both minimum pane sizes. Per-card first allocation is serialized by `(session
socket, workspace, card label)`. Legacy pre-v11 rows retain the `kanban` lookup and old root/split
behavior.
Placement, cwd, and env are never passed to `agent.start`. A newly allocated child can briefly retain
Herdr's previous agent state, or its login shell can still be booting toward an interactive prompt, so
a typed `agent_pane_busy` response gets at most five retries of the exact same `agent.start` request
on that same board-owned child, with 100ms backoff doubling per retry (100/200/400/800/1600ms). It
never allocates another pane for this transient. Persistent busy is terminal and closes only that
owned child; the shell anchor remains. `pane_not_found` is a separate placement race that closes the
child when present, restarts from `tab.list`, and retries complete placement once.

## 3. Data model

See [`../schema.sql`](../schema.sql). Summary:

```
projects(id, scope_path)                     -- NULL = the special Global project; canonical path otherwise
boards(id, project_id, name)                 -- per-project case-insensitive unique name; first board 'main'
selection(id=1, project_id)                  -- singleton: the selected project (absent = none yet)
board_selection(project_id, board_id)        -- the selected board per project
project_recents(project_id, rank)            -- recent projects, capped at 3
board_recents(project_id, board_id, rank)    -- recent boards per project, capped at 3
columns(id, board_id, name, position, system_prompt, trigger,
        on_success_column_id, on_fail_column_id,
        model_override, effort_override, harness_override, permission_override)
cards(id, board_id, column_id, position, title, description,
      harness, model, effort, permission_mode,
      session,                                   -- herdr session name (NULL = default)
      space_kind ('workspace'|'new_workspace'), space_ref, space_cwd,
      status, awaiting_reason,                   -- reason set in 'awaiting', NULL otherwise
      session_id, created_at, updated_at, archived_at)
comments(id, card_id, author, body, created_at, deleted_at)
comment_history(id, comment_id, card_id, author, body, created_at, deleted_at)
runs(id, card_id, column_id, harness, argv_json, prompt_snapshot,
     system_prompt_snapshot,                    -- nullable; enqueue-time, trailer-inclusive
     launch_spec_json,                          -- nullable pre-v11; tagged durable execution spec
     herdr_workspace_id, herdr_pane_id, session_id,
     session,                                    -- herdr session the run spawned into
     started_at, timeout_deadline_at_ms, timeout_paused_at_ms,
     ended_at, outcome ('ok'|'fail'|'cancelled'|'lost'),  -- 'lost' is legacy, no longer produced
     result_summary, log_path)
```

Schema is versioned via `PRAGMA user_version` (current = **v14**). A fresh DB is built straight from
`schema.sql` and stamped v14. Existing v1→v4 migrations retain their space/session, archive, and Pi
effort behavior. v5 adds unique non-null `boards.scope_path`, preserves board `id=1` plus every
related row as `Global`, and leaves existing card harnesses unchanged. v6 rebuilds `cards` to admit
the `awaiting`/`done` statuses and adds `cards.awaiting_reason` (NULL outside `awaiting`). v7 adds
nullable `runs.system_prompt_snapshot` without backfilling old rows. v8 adds the exact partial unique
index `idx_runs_one_open_per_card` on `runs(card_id) WHERE ended_at IS NULL`. There is no safe
residue normalization: one existing open run is retained byte-for-byte, while two or more are
ambiguous, so upgrade reports every duplicate card/run ID and changes neither schema nor
`user_version`. v9 persists each run's wall-clock timeout deadline and optional awaiting pause point.
Upgrade derives an open timed run's deadline once from `started_at` plus its column timeout; an
awaiting run uses `cards.updated_at` as the best durable pause point. Reopen never derives either
value again, so daemon restart cannot replenish the budget. v10 adds partial scheduler indexes for
queued and active open-run scans without rewriting run rows. v11 adds nullable
`runs.launch_spec_json`; existing v10 rows remain byte-identical with NULL, while every new run
stores a format-tagged (`version: 1`) fully materialized argv/env/prompt launch spec. Unsupported
format versions are rejected rather than interpreted. Dispatch uses that spec and the persisted
`runs.session`, so queued runs, retries, and auto-hops are unaffected by later card, column, or
configuration edits. New runs also atomically preserve the fully resolved,
board-protocol-trailer-inclusive system prompt at enqueue time. v12 adds the durable anchor identity;
v13 adds current comment soft deletion plus immutable audit snapshots. **v14 introduces projects**
(canonical-path identity, Global as the special project `id=1` with `scope_path NULL`), rebuilds
`boards` with `project_id` + per-project case-insensitive unique name, and adds the selection,
board_selection, project_recents, and board_recents tables. The v13→v14 migration runs with foreign
keys disabled (boards is the parent of columns/cards): every existing scoped board becomes its own
project's first board `main` and the Global board becomes the Global project's `main`, so **all**
board ids, columns, cards, runs, and comment history are preserved; a crash rolls the upgrade back
and `user_version` stays 13. A legacy NULL remains a
launch-version marker: pre-v7 built-ins
execute their persisted all-in-one argv unchanged, while pre-v7 configured rows retain their
historical spawn-time current-column reconstruction. `Run` deserialization defaults an omitted field
to NULL, but serialization always omits `system_prompt_snapshot` and its contents from boardd wire
responses. `launch_spec_json` is likewise internal and omitted in full from boardd wire responses.

Source ownership is explicit: `schema.sql` is the fresh schema source, `board-core::db` owns ordered
upgrades through v14, and `board-core::protocol` owns the v1 wire DTOs and additive compatibility
rules. The CLI and TUI use typed `BoardClient` wrappers; only boardd reads or writes SQLite. New
v1 fields such as `BoardSnapshot.active_runs` and RPC error `kind`/`details` are additive, so older
clients can continue decoding the existing fields.

### Partial updates

The board protocol stays v1 while nullable partial-update fields use an explicit tri-state:

- omitted means unchanged;
- JSON `null` means clear the stored nullable value;
- a JSON value means set/replace it.

`board-core::protocol::Patch<T>` owns this serde mapping, and the database applies it field by
field after merging with the current row. It is used only by update DTOs for nullable column
settings (`system_prompt`, transition targets, overrides, and timeout) and card settings
(`model`, `effort`, `permission_mode`, `session`, `space_ref`, and `space_cwd`). Create DTOs and
non-null partial-update fields remain unchanged. The TUI sends `null` for an intentionally empty
nullable edit rather than accidentally preserving the old value.

### Authoritative validation

The daemon merges an update with the stored card or column, validates the complete result, and
only then writes SQLite and emits its coarse change event. Schema v8 additionally enforces one open
run per card. Enqueue, promotion, and finalization—including a final comment, card transition, and
optional auto-hop enqueue—are core transaction-scoped units of work. Their return DTOs are not
available until commit; process/socket/event/notification effects occur only afterward. A rejected merged state therefore
cannot leave a partial row or event. Card capability policy covers harness, model, effort,
permission, and space combinations; column permission overrides use `PermissionContext::ColumnOverride`
and never allow `bypassPermissions`, while an explicit Claude card value remains valid. Overrides
without a harness are resolved against the entering card at enqueue time, where effective settings
are validated again for legacy rows and concurrent changes.

### Session model

Cards target a **herdr session** plus a space in it. Because two sessions can each show their own workspaces, the daemon must talk to the right socket per card — the old single-socket model showed the wrong session's workspaces.

- **Registry**: session enumeration is not in the herdr socket API (a session only knows itself), so the daemon shells out to `herdr session list --json` (binary via `$HERDR_BIN_PATH`, else `herdr`), caching ~3s. It maps `name/default/running/socket_path`.
- **Default**: a card with `session = null` uses the daemon's own bound herdr socket; its display name is the registry entry whose `socket_path` matches (else the synthetic `"default"`).
- **Per-session client**: spawn / kill / liveness / workspace resolve-or-create all build a `HerdrClient` on the resolved session socket. Runtime placement and identity live in daemon-owned `HerdrLaunchPlan` / `RuntimeHandle`; `runs.session` remains durable so kill/liveness target the correct socket after a daemon restart.
- **Per-session watchers**: one always-on supervisor multiplexes a `HerdrEvents` stream **per session socket** with active panes. Generation, subscription state, reconnect backoff, and periodic reconciliation deadlines are independent per socket, so one session's pane-set change or disconnect never resets another. Each generation completes subscribe acknowledgement before its snapshot and event polling. Event identity is `(session socket, pane id)`, not pane id alone.
- **Conservative restart reconciliation**: startup resolves each durable open run to `Default`, `Resolved`, or `Unresolved`, then probes a session snapshot outside scheduler/store locks. Only a successful snapshot that omits the pane is `Gone` and applies pane-exit failure policy. Transport errors, timeouts, malformed replies, resolver failures, and worker panics are `Unknown`: this pass leaves the run open and occupying queue capacity; a later daemon restart (or T10) may retry it. A present pane is adopted, watched, and its terminal status restores `awaiting`/`blocked`; durable state is revalidated after the probe so stale observations cannot beat `board done`. An always-on supervisor keeps independent stream generation/backoff per socket, reconnects after Herdr appears or recovers, subscribes before snapshot reconciliation, and periodically repairs missed-event gaps.

### new_workspace flow

On first dispatch of a `new_workspace` card: preflight the selected socket for exact Herdr 0.8.2 /
protocol 20, then list the session's workspaces; if one's label matches `space_ref`
(case-insensitive) reuse it, else `workspace.create {label:space_ref, cwd:space_cwd, focus:false}`.
Then proceed identically to a `workspace` card (cwd snapshot, pane-first per-card tab placement). If the reused or existing workspace snapshot fails, or contains no live cwd, dispatch fails; it never falls back to process cwd or a stale snapshot. A workspace this dispatch **created** additionally
threads its exact initial tab/root pane as a one-shot bootstrap hint: the first card-tab
allocation adopts that tab (renamed to `card-<id>`, root renamed to `card-<id>-anchor`) instead
of leaving an unused initial tab beside a fresh one. Verification is exact (workspace/tab/root
still exist, root is the sole pane, no agent); any mismatch falls back to `tab.create` without
touching the root. The hint's exact tab/root is remembered in the daemon's per-card registry
under the allocation lock **before** the first allocation, so a failed adoption split or launch —
or a later retry in the same daemon — recovers the adopted tab by exact id instead of creating a
second one; the allocator still revalidates the exact ids live on every use, so this memory is
never ownership on its own. The one residual edge is a daemon crash between the adoption rename
and the durable run promotion: the memory and the run row die together, no durable pane id proves
the adopted tab, and the next daemon process creates a fresh card tab next to it — the accepted
cost of identity-only ownership. If the user later closes the sole pane of a managed card
tab, Herdr removes
the tab and, if it was the workspace's last tab, the whole workspace — the next dispatch then
recreates the workspace and adopts its fresh root again.

### Worktree removal

The `cwd` and `worktree` space kinds are gone, and `board-herdr` no longer exposes the unused
`worktree.create`/`worktree.remove` client methods or result DTOs. Worktree isolation is now the
**agent's** job — instructed via the column/card prompt (create a worktree, work in it) — not a
board primitive, keeping the board's space model to "which session, which workspace". The pinned
Herdr schema fixture remains an upstream compatibility reference and is not edited by this cleanup.

## 4. Column configuration

Columns are pure data — created, renamed, reordered, deleted and configured from the TUI (keyboard or mouse, incl. a column-config form for system prompt / trigger / transitions / overrides). **Default board = a single `Todo` column**; the pipeline below is an optional example/template the user can apply or build by hand, not a built-in:

```toml
[[column]]
name = "Todo"
trigger = "manual"          # nothing happens automatically

[[column]]
name = "Plan"
trigger = "auto"
on_success = "Execute"
on_fail = "Todo"
system_prompt = """
You are in the PLAN stage. Use /quick-planner style planning: produce a written
implementation plan and save it under docs/plans/ (or .plans/). Do not write code.
When finished you MUST run:
  board comment $BOARD_CARD_ID "Plan ready at <filepath>. <3-line summary>"
  board done $BOARD_CARD_ID --outcome ok
"""

[[column]]
name = "Execute"
trigger = "auto"
on_success = "Review"
system_prompt = """
You are in the EXECUTE stage. Implement the plan referenced in the card comments.
Run tests. When finished:
  board comment $BOARD_CARD_ID "<what changed, files touched, test results>"
  board done $BOARD_CARD_ID --outcome ok    # or --outcome fail with reasons
"""

[[column]]
name = "Review"
trigger = "auto"
on_success = "Human Review"
model_override = "opus"        # cheaper/different reviewer if desired
system_prompt = """
You are in the REVIEW stage. Review the diff against the card description and the
plan/execution comments. Be adversarial. Then:
  board comment $BOARD_CARD_ID "<verdict + findings>"
  board done $BOARD_CARD_ID --outcome ok    # ok = ship to human; fail = back to Execute
"""
on_fail = "Execute"

[[column]]
name = "Human Review"
trigger = "manual"             # daemon sends herdr notification, waits for a human drag

[[column]]
name = "Done"
trigger = "manual"
```

Notes:
- Column `system_prompt` is combined with the mandatory board-protocol trailer and snapshotted at enqueue. For managed Pi it is delivered through a temporary file passed to `--append-system-prompt`; for managed Claude the file flag is `--append-system-prompt-file`; codex and opencode have **no system-prompt file equivalent**, so their system instructions are delivered inside the single Mint `agent.prompt` block (see [Prompt assembly](#5-prompt-assembly)). Neither replaces harness defaults/context files, and neither puts the system text directly in startup argv. It can invoke skills (`/quick-planner`, `/code-review`) — that's how "column triggers a skill" works, no special mechanism needed.
- `on_fail = "Execute"` from Review + comments-as-context gives the fix loop for free: the re-entered Execute run sees the reviewer's findings in its prompt.

### Cross-board / cross-project card move (prototype)

A card can be moved to a column of **another board**, including a board of **another project**.
`card.move` gains an optional `board_id` (destination board). When it is present and differs from the
card's current board, the daemon performs an atomic **transfer**:

- the store validates the target `column_id` belongs to the declared `board_id` (and that the board
  exists), then updates `cards.board_id` / `cards.column_id` in one transaction and recompacts the
  positions of **both** the source and destination columns (`Db::transfer_card`);
- a **blocking sanity check runs before any mutation**, scoped to the cross-board transfer: the
  merged effective harness/model/effort/permission for the target column is validated
  (`validate_effective_settings`, reused from enqueue), the card's herdr session must resolve, and —
  only when the destination is an auto column that would run — the card's workspace must be
  resolvable in a read-only preflight (`validate_space_resolvable`). An incompatible target or an
  unresolvable session/workspace aborts the move with an explicit error — nothing is written;
- the daemon emits one precise `CardMoved` per affected board. `board_changed` carries an optional
  `board_id`, so a transfer emits two events (source + destination) each scoped to its board rather
  than two coarse, board-agnostic refreshes.

The TUI `m` flow is a hybrid: `m` opens the active board's column picker directly (the fast
same-board path); pressing `b` inside it switches to the destination-board picker for a cross-board
move (board → column). The move form is now a **Project / Board / Column / Position** form that can
target another project's board (`--to-project` on the CLI); choosing another board reloads the
column options. A move — cross-board or cross-project — **never** changes the persistent selection
or recency, and its validation errors surface as the existing red footer toast (the `guard()` path
is unchanged). The help line reads `m  move card (board→column)`.

### Card duplication

`card.duplicate {id}` (CLI `board card duplicate <id>`, TUI `C` on the board or in card detail)
creates a fresh **idle** copy directly below the original in one transaction
(`Db::duplicate_card`): the copy inherits the full run configuration — title with a ` (copy)`
suffix, description, harness, model, effort, permission mode, session, and space settings — while
`status`, `awaiting_reason`, `session_id`, `archived_at`, runs, and comments all start empty, and
the timestamps are the copy's own. The insert + column renumber are atomic, so a failure leaves
the column untouched; the followers shift down and the column stays compacted.

Duplication is deliberately **not** a `card.create`: it never runs `decide_entry`, so a copy in an
auto column stays idle with no run row — the dispatcher only ever picks up queued runs, so the
copy waits for an explicit move/run like any idle card. The daemon emits the normal `CardCreated`
event for the copy, and the TUI shows a `card duplicated as #N` toast after the board refetch.

### Scope selection

At the CLI/TUI boundary, the **persisted selection prevails**: the selected project (and its
selected board) wins over the current directory, so board-aware commands keep their context across
cwd changes. When no selection exists yet (right after a v14 migration, before any explicit
open/create/select), the first board-aware command **bootstraps** it from the current directory:
non-empty `BOARD_SCOPE_PATH` wins, the TUI otherwise uses
`HERDR_PLUGIN_CONTEXT_JSON.focused_pane_cwd`, then `workspace_cwd`, then process CWD. The candidate
is canonicalized; `git -C <candidate> rev-parse --show-toplevel` selects a canonical Git root,
while a non-Git directory keeps its exact canonical CWD. Subdirectories of one repo therefore share
a project (and its boards); equal basenames at different paths do not. Moving/renaming a path does
not migrate its old project, which remains available in the picker.

`o` is daemon-mediated, and **run selection is explicit end to end**: `run.focus` takes a required
`{card_id, run_id}` and never implicitly picks a run. The TUI passes the run the user *selected* in
card detail's Runs section — `detail_run_sel`, a cursor separate from the viewport offset, which
defaults to the newest run (so `o` keeps its familiar meaning), moves with `↑`/`↓` and `k`/`j` while
the Runs section is focused (saturating at both ends, never wrapping, the offset following so the
selected row stays rendered), and survives a detail refresh instead of snapping back. `o` works from
either section; a card with no runs at all toasts locally. The daemon then loads that exact run
(rejecting a run id owned by another card), resolves the run's session socket, canonicalizes and
compares it with the invoking plugin's `HERDR_SOCKET_PATH`, checks the recorded pane still exists
(one targeted `pane.get`), then calls Herdr socket method `pane.focus {pane_id}`. Cross-session
jumps are refused. The result carries the focused run's full identity (card, column, harness, herdr
session name, harness conversation id, pane) plus an `action` saying what the daemon had to do, so
callers can name *which* run they landed on and *how*.

**Rescuing a run whose pane is gone.** A finished run's pane is an ordinary terminal: the user can
close it, and then the run's `herdr_pane_id` points at nothing. Rather than dead-ending, `o` reopens
the run by **resuming its harness conversation in a brand-new pane** in the card's `card-<id>` tab —
automatically, with no confirmation prompt, and reported after the fact. The same path covers a run
that never recorded a pane at all. End to end:

1. the user selects a run in card detail and presses `o` (`Effect::FocusRun(card_id, run_id)`);
2. the daemon loads that exact run, resolves its session socket, and enforces the cross-session
   guard;
3. one `pane.get` decides live vs. dead. Live ⇒ `pane.focus`, `action=focused_recorded_pane`, and
   the overlay exits (attention now belongs to Herdr);
4. dead ⇒ *rescue*. The run row + config alone must supply a harness conversation id
   (`run.session_id`, **not** `run.session`), a harness that explicitly declares
   `ResumeSupport::ByConversationId`, a durable `launch_spec_json`, and a recorded workspace.
   Anything missing is an explicit, actionable refusal — never a fresh-conversation fallback, which
   would silently re-run the card's task. These checks run before Herdr is involved, so "this run
   can never be reopened" is answerable with Herdr down;
5. the resume launch is derived from the **persisted** launch spec (preserving the original
   execution's model/effort/permission mode/env) with `initial_prompt` cleared and `BOARD_PROMPT`
   stripped. The session flags are re-threaded through `SessionPlan::Resume`, whose per-harness
   syntax lives in exactly one place (`board_core::harness::session_argv`), so there is no second
   resume argv path to drift; a persisted *legacy all-in-one* argv (task embedded after `--`) is
   refused instead of rewritten;
6. the board environment is added, and one variable is pointedly left out — see **Credentials**
   below;
7. the **placement workspace is decided**. The run's recorded workspace is probed with the same
   liveness test placement uses (a workspace is usable iff one of its live panes still has a cwd).
   When it is usable, the rescue places there exactly as before. When it is not — the user closed
   the workspace, or it lost its last pane — the rescue resolves a replacement from the card's
   **current** space config (`space_kind`/`space_ref`/`space_cwd`), the exact resolution dispatch
   uses (`dispatch::space::resolve_space`), inside the run's own Herdr session: a `new_workspace`
   card gets a fresh workspace created with its label and cwd (its initial tab is adopted as the
   card tab, same as a first dispatch), while a `workspace`-kind ref to an open workspace resolves
   to that one. The rescue never picks a workspace on its own: if the recorded workspace is gone
   and the card's current config cannot supply a replacement, the refusal names both the dead
   workspace and the config failure, and nothing is created. The choice is final before the plan
   exists because the per-card-tab allocation lock is keyed by the placement workspace;
8. `spawner::rescue` takes the same per-card-tab allocation lock as dispatch (so a focus racing
   another focus, or a dispatch, cannot each create a pane or a tab), scans for a *live* pane an
   earlier rescue left (`action=focused_rescued_pane` if found) in the placement workspace — which
   is also what makes a second `o` after a workspace recreation reuse the replacement (found by its
   label) and the pane it holds — closes dead remains carrying this
   run's marker, else splits a new child in the card tab using the same placement helpers as dispatch
   — with `reclaimable_pane_ids` deliberately empty, so reopening one run never closes another's pane
   — labels it, launches the harness, and focuses it (`action=rescued`). A managed rescue then closes
   the anchor too, with exactly the dispatch semantics: only after launch success, `pane_not_found`
   counts as closed, and any other close failure warns and keeps the successful rescue (configured
   rescues keep their persistent anchor). A failed launch closes the
   pane it created, plus the tab anchor when placement had to create the tab; it also registers the
   exact tab/anchor it kept, so a later dispatch reuses that tab instead of making another — and
   when this very resolution created the workspace, the failure additionally closes that workspace,
   so a rescue that created it leaves no partial resource behind and the next `o` resolves (and
   creates) a fresh one;
9. the TUI toasts what happened and **stays up** on a rescue (Herdr already moved focus to the new
   pane, so quitting would only discard the explanation). Refusals and Herdr errors stay visible as a
   non-fatal toast that leaves the board usable, and never fall back to a different run's pane.

**Credentials: a rescued pane is not the run.** Pane-first placement means the pane's
environment comes from the `pane.split` that creates it; the daemon installs the persisted run env
plus `BOARD_CARD_ID`, `BOARD_SOCKET`, `BOARD_BIN`, `BOARD_RESCUE=1`, `BOARD_RESUME_SESSION_ID` and
`BOARD_RESCUED_RUN_ID`. `BOARD_RUN_ID` is **cleared to empty on purpose** (empty is treated as unset). It is not documentation but the
actor credential: `board comment` authenticates as `agent:$BOARD_RUN_ID`, `board done` forwards it as
the run to finalize, and the configured-harness wrapper passes it to `run.pane_exited`. Since the
rescued pane belongs to no run and the historical row must stay immutable, granting it that id would
either be rejected anyway (`require_agent_run` refuses an ended run) or, for the narrow case of a
still-open run whose pane died, let an unwatched pane finalize that run while racing the liveness
watcher for the right to write its outcome. Withholding it fails closed and still leaves the useful
path open: `board comment` becomes an ordinary human comment on the card, which is the durable place
for a resumed conversation to report back.

**Limitations (accepted, by design).** The rescue writes **nothing** to the database: no new `runs`
row, no `herdr_pane_id` update, no reopened `ended_at`/`outcome`, no new column, no migration. The
historical run row is immutable, which is the point — a rescue is a way to *read and continue* a past
execution, not to resurrect it as a run. Two consequences follow and are not worked around:

- the rescued pane is **ephemeral and unmanaged**. With no run row there is no ownership record, no
  watcher, no idle grace, and no timeout: the daemon will not observe it, will not close it, and will
  not report anything it does. `board done` from inside it does not apply (the run is closed); the
  pane carries `BOARD_RESCUE=1` to say so. Closing it is the user's job.
- deduplication rests on a **name, not a record**. The rescued pane's label/agent name
  `card-<id>-r<run>-rescue` is the only trace a previous rescue can leave, so pressing `o` twice is
  reliably idempotent for panes the daemon created, but a user who renames the pane or its agent can
  defeat the scan. This is a diagnostic hint, not an authoritative record, and it is the direct cost
  of the no-database-writes decision. The marker derives from card id + run id only, precisely so
  that renaming a *column* cannot break it. When the recorded workspace is gone, the replacement
  is likewise found by the card's current space config: a `new_workspace` card reuses its
  replacement by **label** (`dispatch::space::resolve_space`'s find-or-create), so a user who
  renames the replacement workspace defeats that half of the dedup too — a second `o` then creates
  yet another workspace (with the configured label) and resumes in it, leaving the renamed one
  alone; the same no-DB-writes trade-off as the pane marker.
- for a **configured** (unmanaged) harness, a rescued pane that outlived its harness cannot be
  detected. Herdr tracks no `agent` for unmanaged panes, so the label is the only evidence and a
  leftover shell looks exactly like a live resume; `o` will focus it rather than resuming again.
  Managed `pi`/`claude` panes do not have this problem — Herdr's agent registration disappears with
  the process (observed live: the pane stays open as a labelled shell with `agent` absent), so the
  daemon re-rescues and reclaims the dead shell. That is a *presence* check: `PaneInfo.agent` is not
  the exclusive name the board picked — the schema carries `agent` and `name` separately — so it is
  never compared to the marker.
- a rescued pane cannot report through the run channel at all (see **Credentials** above): `board
  done` and the `__pane-exited` callback do not apply to it. This is the intended consequence of the
  immutable-history rule, not an oversight.

### TUI interactions (v1)

- **Access: overlay only** — `[[keys.command]]` keybinding (e.g. `prefix+k`) → `plugin pane open --plugin herdr-board --placement overlay`; the board floats over the current workspace from anywhere, dismiss to drop back. No pinned workspace, no sidebar entry (herdr has no sidebar extension point — verified against api schema/config).
- **Responsive board view, three layout modes by terminal width** (`LayoutMode::from_width`): **Compact** `< 60` cols, **Regular** `60..=119`, **Wide** `>= 120`. Regular/Wide: visible columns divide the content viewport while preserving a readable minimum width (`MIN_COL_W = 26`); when not all columns fit, the selected column drives a full-width sliding window, and the brand/count, centered board dropdown, and right filter rail share one header line. Compact renders exactly one full-width column with row one `◈ herdr-board` plus the global running count, row two the board dropdown plus direct visibility chips, and row three `[ ‹ ]  [ <column name> (M/A) · n/N · cards ]  [ › ]`; the navigator segments are independent tap/click targets and the running count is not repeated there. Cards use status-colored borders and a readable selected background; each status line owns exactly one semantic glyph (▶ running, ⏸ blocked, ✗ failed, ⧗ queued, ? awaiting — yellow, ✓ done — green), while title/id lines stay free of status markers. Cards also carry harness/model metadata and a live run timer; in Compact, card titles word-wrap to up to two lines instead of being truncated.
- **Per-column card scrolling, all modes:** each column carries its own scroll offset keyed by column id and draws a 1-cell scrollbar on its right edge once its card count exceeds what fits. Previously cards past the bottom of a column were simply not drawn, with no scroll state at all. Mouse wheel over a column scrolls that column's card list (whichever column the pointer hovers, not necessarily the focused one); wheel no longer reorders the focused card — card reordering stays keyboard-only (`H`/`L`; column reordering stays on `M`, below).
- **Column switcher, Compact only** (`Screen::Switcher`): now columns-only. Tapping the header's center button opens it; it lists the current board's columns with card counts, plus a trailing `⇄ Switch board →` row that opens the **board picker** (the old second level is gone — `b` and the header board chip open the same picker at every breakpoint) and, below it, an `⊞ Apply template` row. The template row is the touch counterpart of the board screen's `T` key — both route through one helper so the template name lives in a single constant — and it stays visible but dimmed when the board is not pristine, activating it then explaining why rather than silently doing nothing. `j`/`k` (or `↑`/`↓`) move, `Enter` activates the selected row, and `q` closes the sheet outright (it is the "get me out" key everywhere else in the TUI, and the switcher used to swallow it). `Esc` closes the sheet outright; there is no level-2 state to back out to anymore. The board picker opened from the switcher returns to the switcher when closed, restoring the selection that was active before drilling in. `SwitcherState::entered_at_boards` is gone; `b` always opens the board picker, at every breakpoint.
- **Sheets route through one placement rule, `sheet_area(mode, pref_w, pref_h, area)`:** fullscreen in Compact, the existing centered popup in Regular/Wide. Every overlay uses it — card/column form, move-card and board pickers, the `M` move-column mini-mode, confirm, help, card detail, and the switcher. Both branches derive from the board content region (below persistent header chrome and above the bottom action rail); an idle frame reserves no footer hint row, while a visible toast reserves one transient row above that rail.
- **Widget/hit-testing layer** (`board-tui/src/widgets/`): a `HitMap` of `Zone`s is rebuilt every frame in `view()` and consulted by the mouse handler on the next input event, instead of the mouse code recomputing overlay geometry inline; the last-pushed zone wins, so overlays drawn after the board shadow it at the same cell. Built on it: transparent white `[ Save ]  [ Cancel ]` chips on forms (touch has no keyboard to submit/cancel with), and `render_sheet_frame`, a bordered sheet with a short Compact-only title (truncated to leave room for the corner, a gap, and the chip) plus an in-border `[ X ]` close chip registered as a hit zone; centered Regular/Wide overlays expose the same exact `[ X ]` chip. Selected chips use bold/underline rather than a background. `windowed_rows` picks the widest contiguous run of whole rows around the focused one that fits the available height; it drives both form field scrolling (card/column forms keep the focused field wholly visible) and picker option scrolling.
- **Help is global, returns where it came from, and scrolls in every layout.** `?` is bound once in `app::on_key`, ahead of the per-screen dispatch, so every screen can reach help except the two forms (where `?` is a literal character) and help itself. Opening it records `App::help_return_to`, and closing returns to exactly that screen — including card detail, which previously dumped you back on the board. The keybinding table has since outgrown a fixed sheet, so both layouts scroll with `j`/`k`: Compact renders one entry per row with the full description plus a scrollbar and closes only on `Esc`/`q` (so `j`/`k` stay available for reading), while Regular/Wide keeps its two-column layout and its "any other key closes" behaviour but no longer closes on the keys a reader reaches for. Before this the table silently truncated at 80x24. Neither help layout reserves a trailing hint row, and Compact picker options wrap instead of clipping.
- **Touch is a first-class input alongside the keyboard**, not just mouse-as-touch: every Compact interaction — column step, switcher open/select, form save/cancel, sheet close — has a dedicated tappable zone sized and placed for a fingertip, not only a keyboard fallback.
- **Card detail:** opens as a contextual popup and toggles fullscreen with `f` or its clickable title
  action. Status fields use blue labels and white values. Description, comments, and runs size to
  their content; the description and each comment body word-wrap (`Wrap { trim: false }`) at the
  panel border instead of being truncated, and comments scroll by wrapped row. Runs stay one line.
  Compact card actions use transparent, wrapped `[ Edit ]`, `[ Archive ]`, optional `[ Confirm ]`,
  `[ Add ]`, `[ Open ]`, `[ Retry ]`, and `[ Cancel ]` chips; the Runs frame reserves its action
  row before drawing history, so a zero-row body never overwrites a control.
  Comments and runs scroll independently (`Tab` selects, mouse
  wheel scroll — a wheel notch is a raw offset move that then drags the section's cursor into the
  rows it brought into view, so the `▸` marker never leaves the screen and `o`/`e`/`d`/`h` can only
  ever act on a visible row), with a blue divider for the focused history. Histories open at the latest item and
  show only directional arrows (no counts) when content is hidden. Each run row is deliberately
  minimal — `#<id> <harness> · <outcome|active> · <duration>`, i.e. run number, harness, status, and
  how long the run took (an open run is measured against the injected `app.now`, so `active` rows
  count up) — still budgeted against the section width so a narrow detail truncates instead of
  overflowing. The column, the **harness conversation id** and a `pane ✓|-` marker are deliberately
  *not* in the row: the column is implied by the card, the conversation id and the herdr **session
  name** live in the status fields (and never in the same slot as each other), and since a run whose
  pane is gone is now reopened by resuming its conversation, a missing pane no longer predicts
  whether `o` works. With runs focused, arrows/`k`/`j` move a **selected-run** marker (the same
  bright-blue `▸` gutter and bold row the comments list uses — one shared `focus_row_marker`, so both
  lists mark their cursor identically; bright/intense blue, never the 256-palette navy `Blue`, which
  would lose contrast on a dark background) one run at a time, dragging the viewport along; with
  comments focused they instead move a **focused-comment** marker (the same `▸` gutter, bold
  body) one comment at a time, pulling the viewport along so the focused comment's wrapped rows stay
  visible. The focused comment is what comment management acts on: `e` edits it (the same one-field
  comment form, pre-filled), `d` deletes it behind the confirm overlay, and `h` opens its audit trail
  (`comment.history`) as a scrollable sheet, oldest → newest. The same three actions are tappable
  from an `[ Edit ]` `[ Delete ]` `[ History ]` bar on the section's last row, and tapping a comment focuses it; Board cards deliberately do not expose those card-level controls, so only Card Detail/comments retain their mouse actions;
  every comment and button is a registered hit zone, so the whole flow works by touch. Soft-deleted
  comments simply disappear — `card.get` projects through `list_comments`, which filters
  `deleted_at IS NULL` — while their history remains reachable up to the moment they are deleted.
  System comments are immutable at the database boundary, so their `[ Edit ]`/`[ Delete ]` chips remain clickable and both keys explain the refusal instead of issuing a call the daemon would reject; `h` still
  works on them. Daemon rejections (agent-owned comment, already deleted) surface as error toasts.
  With no comment focused — runs focused, or a card with no comments — `e` keeps its original
  meaning and edits the card, returning to detail after save/cancel. `Enter` on an `awaiting` card confirms completion (the same `run.done
  ok` channel as `board done ok`); the detail view shows the `awaiting` reason (agent reported
  done / idle past grace).
- Mouse **and** keyboard for everything: `p` opens the project picker and `b` the board picker (both chips are clickable); drag card
  between columns / `m` move; `n` new card, `N` new column; `e` edit card; `a` archive/restore; `v`
  cycles `ACTIVE` / `ALL` / `ARCHIVED`; `c` comment, with `e`/`d`/`h` managing the focused one in
  card detail; `Enter` card detail; `o` focuses the **selected** run's
  pane when it belongs to the current Herdr session (help: `o  jump to selected run pane`); `r` refreshes the selected board
  on demand); `?` help overlay listing **all** keybinds; column config form (rename, system prompt,
  trigger, on_success/on_fail, overrides, reorder, delete). **Column reorder** is reachable by mouse
  drag or the `M` (Shift+m) mini-mode: it mirrors the move-card picker's stage→commit→cancel shape —
  `←`/`→` (or `h`/`l`) slide the focused column locally, `Enter` commits a single `column.reorder`,
  and `Esc` restores the original order with no effect (`m` still moves a card, `H`/`L` shove a card).
  The filter is rendered in the Herdr pane
  title (`Board [<scope> · ACTIVE|ALL|ARCHIVED]`); the board has no persistent footer hint row, and transient toasts remain available above the action rail. Archived cards are
  inert until restored and render dimmed with `▣ ARCHIVED` when visible.
- **Content-sized overlays (Regular/Wide):** within the `sheet_area` centered popup, card/column forms, move pickers, and help panels shrink to their content on large terminals and clamp to the available viewport on small terminals; Compact always gets the fullscreen sheet described above instead.
- **Guided card & column forms** share one metadata source: both fetch `harness.capabilities` (models/efforts/permissions via the daemon-side `HarnessMeta` adapter trait) and `harness.list` (built-ins + config-defined harnesses). For cards: Pi is selected for new cards; Claude remains selectable. On open/harness change the form also fetches `space.list`. Model starts at the daemon-sent `default model` option (unset), then catalog aliases and `(custom)` when free-form is supported. Effort follows the selected model's declared set, or the catalog default for omitted/free-form models. For Pi, that declared set comes from its documented `thinkingLevelMap` tri-state contract: omitted standard levels (`off`–`high`) remain supported, omitted extended levels (`xhigh`/`max`) do not, strings opt levels in, and `null` opts them out. Codex models and per-model efforts come from `$CODEX_HOME/models_cache.json`, with free-form fallback; its permission selector labels the three stable wire presets as `Ask for approval`, `Approve for me`, and `Full access`. OpenCode models are free-form, with the daemon's live `opencode models --verbose` discovery (`$OPENCODE_BIN` else `opencode` on PATH) overlaying a static fallback that truthfully lists OpenCode Zen Nemotron 3 Ultra Free (which declares no variants and therefore offers no effort) plus a fixture model `opencode/deepseek-v4-flash-free` (low/high/max); its effort selector follows the selected model's discovered variants — empty for a variant-less model — or the full ladder for omitted/free-form models, with `off` mapped to the opencode variant `none` only when building the process-local agent config (the root/TUI has no `--variant` flag), and its permission selector offers exactly the two verified modes `Default` and `Auto-approve` (`--auto`). Permission is hidden and submits `None` for Pi; Claude shows its modes. The rule is one predicate — `!permission_modes.is_empty()` against the effective capabilities — never a harness-name comparison, so it holds in the *not-yet-fetched* case too: a harness board-core does not know answers with an empty permission vocabulary, and the selector stays hidden until `harness.capabilities` returns. Inventing another CLI's enum is never safe; the old fallback guessed a six-item list that included a value the daemon rejects. Switching harness resets only incompatible values. Workspace labels are shown but ids are persisted. Fetch failures degrade to free text with a warning. For column config the same source drives the override fields: `harness_override` is a **select** over the available harnesses (`(none)` = no override), `effort_override` follows the override harness's catalog, and `permission_override` is **hidden** when the driving harness has no permission modes (e.g. Pi); changing the override harness refetches capabilities and resets only overrides that became invalid. The column `system_prompt` field is **hidden when the trigger is `manual`** (a manual column never launches a run, so the prompt is unused) and reappears when the trigger is switched to `auto`; it is hidden from the UI, not omitted, so submit still sends a `system_prompt` Patch that preserves whatever the column already stores.
- Long text (card description, column system prompt): modal textarea, `Ctrl+E` suspends the TUI into `$EDITOR`. The form's multiline field value now renders as a real wrapped paragraph (`Wrap { trim: false }`) with the same strong focused reverse treatment and white unfocused readability as title/name values; windowed field scrolling keeps the focused field wholly visible, and `Ctrl+J`/`Shift+Enter` remain newline paths; it previously joined the textarea's lines with a literal `"  ⏎  "` separator and hard-truncated the result to one line with `…` at every width, so most multiline content was unreadable. Returning from `$EDITOR` now forces a full terminal repaint (`terminal.clear()`) before the next draw, and so does a terminal resize; `RealEditor` re-entering the alternate screen behind `ratatui::Terminal`'s back previously left the next frame diffing against a stale cached buffer, so the screen stayed blank until some other full redraw happened to occur.
- **Every destructive action confirms, and the confirmation is gated on the action being possible.** `x` (cancel a run) only opens the confirm sheet when the card actually has an open run, and otherwise toasts — `run.cancel` on a finished card is refused by the daemon, so "cancel the running run?" was a question with no true answer. `r` (retry) now confirms too: it relaunches a real agent, which is strictly more consequential than the cancel that already confirmed. Deleting a column with cards asks where to move them **and then confirms** — picking a destination is not consent to the delete, and that riskier path was the one left unguarded while the empty-column path had always confirmed. A running card's column can't be deleted at all. Both answers land on `Confirm::return_to`, the screen recorded on the way in.
- Optional: apply a board template (e.g. the example pipeline above) onto an empty board — `T` on the
  board screen, or the switcher's `⊞ Apply template` row in Compact.

## 5. Prompt assembly

At enqueue, boardd resolves and persists two independent channels:

```
task_prompt   = <card.description>
                + "\n\n## Card comments\n" + last 20 comments (author, ts, body)
system_prompt = <column.system_prompt, if any>
                + "\n\n" + <mandatory board protocol trailer>

env = BOARD_CARD_ID=<id>, BOARD_RUN_ID=<id>, BOARD_SOCKET=<path>, BOARD_BIN=<exact board executable>
```

`runs.prompt_snapshot` stores `task_prompt`; v7 `runs.system_prompt_snapshot` stores the exact
trailer-inclusive `system_prompt`. Both are enqueue-time values. For managed built-ins, persisted
startup argv is deliberately prompt-free:

```
Pi:     pi [--model M] [--thinking E]
            (--session-id ID | --fork OLD --session-id NEW)
Claude: claude [--model M] [--effort E] [--permission-mode P]
               --allowedTools "Bash(board:*)"
               (--session-id ID | --resume ID [--fork-session])
Codex:  codex [--model M] [-c model_reasoning_effort=E]
               [permission preset flags]
               (resume <id> | fork <id>)        # Mint: no session tokens at all
OpenCode: opencode [--agent herdr-board | -m M] [--auto]
               (-s <id> | -s <id> --fork)       # Mint: no session flag at all
```

Codex notes: board effort `off` maps to `model_reasoning_effort=none` while building argv (every
other protocol level keeps its spelling; cache-only `ultra` is filtered). `ask-for-approval` maps to
`--sandbox workspace-write --ask-for-approval on-request`, `approve-for-me` to `--approve-for-me`,
and `full-access` to `--dangerously-bypass-approvals-and-sandbox`. A codex Mint
persists `session_id = NULL` (the board never invents a uuid for a harness that mints its own); the
integration-reported thread id is captured after launch and promoted atomically onto run+card.

OpenCode notes: models are free-form `provider/model` via `-m`, and the board calls the variant
dimension **effort**. The root/TUI has **no `--variant` flag** (verified against opencode
1.18.15 — the spelling exists only on `opencode run`), so an effort is applied through a
process-local `OPENCODE_CONFIG_CONTENT` config env defining a stable custom agent `herdr-board`
with exactly `model` + `variant` (board `off` → opencode's own `none` vocabulary; every other
level keeps its spelling), selected with `--agent herdr-board`; `-m` is dropped because the agent
owns the model, and an effort with **no model is an error**. Without an effort the model stays
`-m` and no config is injected. Permission
modes are the two verified presets: `default` emits no flag, `auto-approve` emits `--auto`; any
other value is rejected by engine capability validation. An OpenCode Mint likewise persists
`session_id = NULL` — the TUI mints its own `ses_…` id, so the board never invents one — and the
integration-reported session id is captured after launch and promoted atomically onto run+card.

After pane-first placement, boardd writes `system_prompt` to a temporary mode-`0600` file and calls
the supported managed-agent interface as follows:

```
agent.start {
  name, kind:"pi"|"claude"|"codex"|"opencode", pane_id,
  args:<startup argv without executable> +       # codex/opencode: exactly the
       ["--append-system-prompt", FILE]          #   startup tail, no prompt file,
       ["--append-system-prompt-file", FILE],    #   no `--`, no task
  timeout_ms:30000
}
agent.get {target:pane_id}       # bounded polling until interactive_ready && !launch_pending
agent.prompt {target:pane_id, text:task_prompt}
```

If Herdr returns typed `agent_pane_busy`, boardd retries the exact `agent.start` parameters—including
pane, name, args, timeout, and the same system-prompt file—on that same owned pane at most five times,
backing off 100ms then doubling per retry (100/200/400/800/1600ms), long enough for a slow login
shell to reach its prompt. This bounded transient retry never allocates another pane. A
persistent busy response is terminal: the ordinary error path closes the board-owned child and
leaves the pre-existing split anchor intact. This is not the `pane_not_found` placement race;
that error closes the owned child when present, rediscovers from `tab.list`, and retries complete
placement once. The temporary file is removed before spawn returns, on success or failure. The card
prompt is never part of `agent.start`; it is submitted only after readiness. An `agent_name_taken`
response retries once on the same owned pane with `card-<id>-<column-slug>-r<run>`.

**Self-minting prompt transport** (codex and opencode; neither has a system-prompt file): after
readiness the daemon runs the bounded `agent.get.agent_session` capture (at most 5 probes, 10s
cap; accepts an `id`-kind reference owned by the expected agent with a non-empty `value` — opencode
additionally pins the exact source the current Herdr opencode integration reports; anything else
degrades to `None` with a warning and the launch continues), ordered per harness: **codex captures
before the prompt**, while **opencode captures after it** — real OpenCode mints its `ses_…` id and
reports `agent_session` only once the first `agent.prompt` lands, so a pre-prompt capture would
lose the id, and a prompt-less opencode rescue reduces to capture-after-readiness. The prompt goes
through `agent.prompt`: a Mint receives one delimited block — `## herdr-board system instructions`
then `## herdr-board card task` — while a resume/fork fresh pane receives the task alone (the
conversation already carries the system instructions), same-pane reuse the task alone, and a rescue
sends nothing.

Configured harnesses use the same pane-first cwd/env placement, then `pane.rename`. Because direct
`herdr pane run` does not preserve complex argv boundaries, boardd writes one mode-`0700`,
self-removing script with every configured argv element POSIX-quoted and invokes exactly
`herdr pane run <pane_id> <script_path>` with `HERDR_SOCKET_PATH` set to the selected session socket.
The script runs the exact child argv and preserves its exit status. When the child returns it invokes
the hidden `board __pane-exited --run-id "$BOARD_RUN_ID"` guard. That guard sends internal
`run.pane_exited {card_id,run_id}`; only the exact matching open queued or started configured run is failed, with no `on_fail`
transition. A callback before registration is accepted. The same narrow race rule applies to an
immediate configured-harness `board done`: the CLI forwards `BOARD_RUN_ID`, and only that exact
queued run may finalize before runner registration; a queued built-in (pi/claude/codex/opencode/antigravity) completion is
rejected because no managed pane exists yet. For an already-started run, `run_id` remains optional
so manual/TUI callers remain compatible, but a supplied mismatched id is rejected. Stale,
replaced, completed, and built-in callbacks are rejected, so a stale child cannot complete a
replacement. An already-completed or replaced run is rejected and the wrapper ignores that expected
error. The script deletes itself when it starts; if the pane runner fails synchronously, boardd
removes it and closes only the pane boardd allocated. If scheduling succeeds but the pane never
opens the script, the residual configured-script orphan is an accepted asynchronous limitation.

- **Session strategy**: Pi's first auto column mints an exact `--session-id`; later stages reuse it; retry uses `--fork <old> --session-id <new>` and persists the new target. Claude keeps exact mint, `--resume`, and `--fork-session`. Codex mints its own thread id (no session flag, `NULL` persisted), captures the reported id after launch, and re-attaches with the `resume <id>` / `fork <id>` subcommands; a fork's newly captured id supersedes the recorded source id atomically, and a fork whose new id was never captured keeps the source id instead of wiping it. OpenCode mints its own `ses_…` id the same way (no session flag, `NULL` persisted) and re-attaches with the trailing session flags `-s <id>` / `-s <id> --fork`; the same atomic supersede/keep rules apply. Column config can force `fresh_session = true` (for self-minting harnesses that means a fresh Mint rather than a resume).
- **Configured harnesses** remain unmanaged. Their exact configured argv is not inferred to be Pi or Claude; prompt channels arrive as `BOARD_PROMPT` / `BOARD_SYSTEM_PROMPT`. The configured runner resolves a nonempty `HERDR_BIN_PATH`, otherwise `herdr`.

## 6. Data flow — the canonical walkthrough

1. **Create** card in *Todo*: title "Add retry to MELI scraper", description (prompt), harness=pi (default), model omitted (Pi configured default), effort=low, no permission mode, space=workspace `w4`.
2. **User drags card → Plan** (TUI → boardd `card.move`).
3. Column engine: *Plan* is `trigger=auto` → **enqueue run** on the card's space queue.
4. Dispatcher (respecting per-space serial queue + global cap):
   a. Resolve the card's session socket and `ping` it. Anything except exact Herdr 0.8.2 / protocol 20 fails before workspace discovery/creation. Then reuse workspace `w4`, or create/reuse the card's labeled `new_workspace`; repository worktree isolation remains an agent prompt responsibility.
   b. Preflight the selected socket again at the spawner boundary. For a new durable run, the card's **`card-<id>` tab** is resolved by exact owned id (reconstructed from the newest matching durable pane in the same session/workspace when boardd restarts), or `tab.create {workspace_id,cwd,env,…}` supplies a new shell anchor — unless the dispatch just created the workspace, in which case the workspace's own initial tab is adopted (verified, then renamed) instead of leaving an unused tab. The anchor is labeled `card-<id>-anchor`, its exact id is persisted on the promoted run (NULL for managed runs, whose anchor is closed after a successful launch), and the run child is always created by `pane.split` from that anchor; `agent.start`/`pane run` never target the root. A renamed anchor is still selected only by exact identity; a closed anchor is recreated only from a durable board-run child in the exact proven tab, and missing proof creates a fresh tab without selecting a duplicate-label user tab. Exact ended children may be reclaimed before a later split so the anchor keeps usable geometry. The child receives the run env; the anchor receives only stable card identity. If multiple historical panes are live, newest run id wins; legacy rows retain their old lookup. Placement, cwd, and environment are not `agent.start` fields; the call receives neither the workspace placement nor the anchor pane id.
    c. For Pi/Claude, write the snapshotted system prompt to a mode-`0600` temporary file; issue `agent.start {name,kind,pane_id,args}` on the split child with prompt-free startup args; a typed `agent_pane_busy` retries the exact request on that same child with bounded 100ms/200ms backoff (never another split); poll `agent.get` for readiness; then send only the task snapshot through `agent.prompt`. Remove the file. For codex/opencode, no prompt file exists: after the same readiness poll the daemon bounded-polls `agent.get.agent_session` (at most 5 probes / 10s) for the integration-reported id (expected agent, `kind:"id"`, non-empty `value`; opencode also pins the integration source) and persists it atomically with the promotion — **codex captures before delivering the prompt, opencode after it** (real OpenCode mints `agent_session` only once the first `agent.prompt` lands) — and the prompt is a delimited `system + task` block on a Mint, the task alone on a resume/fork fresh pane. Card status → `running`; record the exact child pane/workspace ids. The pane is **visible** — you can watch or type into it anytime.

   **Pane naming and ownership**: the managed agent name is `card-<id>-<column-slug>` (e.g. `card-42-plan`, `card-42-execute`). Herdr names are exclusive while a pane is open, so `agent_name_taken` retries once on the same pane with `card-<id>-<column-slug>-r<run>`. A persistent `agent_pane_busy` closes only the board-owned child and leaves the pre-existing anchor. If a placement target disappears, boardd closes only the pane it created (a missing pane is already clean), restarts discovery from `tab.list`, and retries the complete placement once. A terminal launch error also closes only that board-owned pane; pre-existing user panes are never cleanup targets.
5. Agent plans, writes `docs/plans/meli-retry.md`, then calls `board comment 42 "Plan ready at docs/plans/meli-retry.md …"` and `board done 42 --outcome ok`. From a run, the CLI forwards `BOARD_RUN_ID`; manual/TUI completion omits it and remains compatible.
6. boardd receives `done` → closes the run (`outcome=ok`), posts a `system` comment ("Plan finished in 4m12s, $0.38"), applies `on_success` → **card auto-moves to Execute** → step 3 repeats with the Execute column prompt, `--resume <session>`.
7. Execute finishes → comment → auto-move to *Review* → Review run (fresh session, model override) → verdict comment.
   - `--outcome ok` → card lands in **Human Review**: `trigger=manual`, boardd fires `herdr notification show "Card #42 ready for human review" --sound request`.
   - `--outcome fail` → card goes back to **Execute** with the findings as comments; loop.
8. **Human** opens the pane / diff, optionally comments, drags to *Done* (or back to Execute — manual moves into auto columns also trigger runs, so "drag back with a comment" = feedback loop).

### Completion detection (belt and suspenders)

| Signal | Source | Role |
|---|---|---|
| `board done <card> --outcome …` | agent itself (instructed by every auto-column's system prompt) | **primary** — explicit, carries semantics |
| `pane_agent_status_changed` → `done` | herdr events (requires `herdr integration install pi`; Claude has its equivalent) | agent finished but forgot `board done` → card `awaiting` (`agent_done`), run stays open, notify human |
| `pane_agent_status_changed` → `idle` sustained past grace | herdr events | agent idle without `board done` → card `awaiting` (`idle_expired`), run stays open, notify human |
| `pane_agent_status_changed` → `blocked` | herdr events | agent/integration reports blocked (provider retry exhaustion or input need) → card `blocked`, board change + notification |
| `pane_exited` | herdr events | managed pane crash / close → run `fail`, no transition; events match `(session socket, pane id)` |
| configured runner exit guard | board-owned wrapper after its exact child argv returns | exact open (`queued` or `started`) configured run → `fail`, no transition; callback-before-registration is accepted, stale/completed and built-in runs are rejected |

**Golden rule:** herdr status is a HINT; `board done` is the only terminal success truth. For a
configured harness, that completion may arrive in the narrow queued-before-registration window;
queued built-in (pi/claude/codex/opencode/antigravity) runs are deliberately not eligible. Pane-idle scraping alone is the
documented weak point of every tmux-style orchestrator (claude-squad); the explicit `board done`
channel is what makes auto-transition trustworthy, and silent finishes park
the card in `awaiting` for review instead of guessing an outcome. Without the optional Pi
integration, spawn, explicit completion, timeout, and pane exit remain deterministic, while
working/blocked/done signals and the idle→`awaiting` watchdog are unavailable (degraded mode).

### The `awaiting` state and the single signal decider

Watchers only **observe**: herdr pane statuses and idle expiry are translated into `AgentSignal`s
(`working` / `blocked` / `done` / `idle_expired`), and the pure engine
(`board_core::engine::decide_signal`) is the **single decider** mapping a signal plus the current
card status onto a `SignalDecision` (new status, optional `awaiting_reason`, optional
notification). The daemon applies the decision in one place; pane-exit, column timeout, and cancel
keep their existing `finalize_run` paths. The same core-owned lifecycle policy exposes
`LifecycleDecision` and `FinalizePlan`: it validates supplied run identity, distinguishes
queued configured runs from queued built-ins, and selects kill/transition behavior for
cancel, timeout, pane exit, and explicit completion. The daemon supplies DB facts and
executes the returned plan; it performs no Herdr or SQLite I/O in the pure decision.

- `awaiting` = the agent finished(?) without `board done`. The run stays **OPEN** — it never
  becomes a failure on its own. The **column timeout is paused**: entering `awaiting` records the
  span and shifts the deadline forward by the review time on exit.
- Entry: herdr `done` (immediate, `agent_done`) or `idle` sustained past `idle_grace_seconds`
  (`idle_expired`). The reason is stored on the card and cleared when the card leaves `awaiting`.
- **Review cycle**: the human reads the pane and either confirms (`board done` / TUI `Enter` on the
  card detail → the same `run.done ok` channel → `done` or column move) or types feedback into the
  pane — the integration then reports `working`, the card goes back to `running`, and the cycle
  continues. `board cancel` still cancels.
- The run outcome `lost` is retained in the schema and enums for backward compatibility but is no
  longer produced; the old idle→`lost`→`failed` path is replaced by idle→`awaiting`.

## 7. Queueing & concurrency

- **Canonical run lifecycle API**: every run passes through exactly three core DB units of work —
  `enqueue_run_uow` (insert a queued run, set card `queued`) → optional
  `promote_run_uow` (set `started_at`/workspace/pane/deadline, set card `running`) →
  `finalize_run_uow` (close the run, write terminal comments, transition the card, optionally
  insert an auto-hop enqueue). There are no legacy methods. The daemon-side wrappers
  (`enqueue_run`, `finalize_run`, `finalize_run_timeout`) prepare inputs under scheduler→store
  locking and call only these three UoWs; queued cancel and spawn failure are themselves finalized
  through `finalize_run_uow`.
- **Lifecycle and launch ownership**: `board-core::engine` owns Herdr-neutral
  `LifecycleDecision` / `FinalizePlan`, auto-hop limits, and resumability evidence (a started run
  plus its `agent:<run_id>` comment). `board-core::launch` contains only the durable neutral
  `ExecutionSpec` / `RunLaunchSpec`. `board-daemon` owns `Spawner`, `HerdrLaunchPlan`,
  `RuntimeHandle`, placement, liveness, and process effects while preserving the scheduler→store
  lock order; this boundary does not change transaction execution or launch bytes.
- **Effects only post-commit**: no socket, process, notification, or other external I/O occurs
  inside any UoW transaction. The daemon executes post-commit effects in a fixed order: update
  scheduler bookkeeping, refresh watches, kill a pane, schedule notification, emit terminal events,
  then wake dispatch. The shared scheduler→store lock order supplies only transient mutual
  exclusion; no separate finalizing-card state participates in durable decisions. A rejected merged
  state or failed transaction therefore cannot leave a partial row, event, or process effect.
- **Per-space FIFO**: two agents mutating one working tree collide; cards sharing the typed
  `SpaceKey(session, space_kind, space_ref)` run serially. Null/default values remain typed and are
  never separator-encoded.
- **Active-run timer source**: board snapshots expose only additive summaries for started, open
  runs. The TUI joins those summaries by card id, so comments/card edits cannot reset the elapsed
  timer; `started_at` remains the authoritative clock across event refreshes.
- **Global concurrency cap** (default 3) limits active runs across spaces. A per-daemon async mutex
  serializes complete dispatch passes through launch registration/failure. Inside that lock, a pass
  claims capacity and each space's FIFO head before any launch starts; claimed independent spaces
  launch concurrently, while a second run for either space remains queued.
- A `new_workspace` card that opens a distinct workspace per label gets its own queue key, so distinct labels run in parallel (up to the global cap). Agent-driven worktree isolation (see §3) is what escapes a per-repo bottleneck now.

## 8. Failure & safety rails

- Per-run timeout (column-configurable) → kill pane, run `fail`, card to `on_fail`.
- Managed `agent_pane_busy` is retried only on the same newly owned child (five retries, 100ms backoff doubling to 1600ms, ≈3.1s window); persistent failure closes that child but never the pre-existing anchor. `pane_not_found` instead triggers the separate one-time full placement rediscovery path.
- `--max-budget-usd` per run (Claude supports it in print mode; interactive panes rely on timeout + human visibility).
- Pi has no board tool-permission mode; no permission/approval flag is added and explicit Pi permission is rejected. Claude `bypassPermissions` requires explicit per-card opt-in, never a column default.
- Cards never auto-move into *Done*; last auto hop is always a human-gated column.
- Retry = a new run; Pi uses `--fork <old> --session-id <new>`, Claude uses `--resume <old> --fork-session`, codex uses the `fork <id>` subcommand, opencode uses `-s <id> --fork` (the new id is captured after launch and supersedes the source id atomically); history is preserved.

### Diagnostic security boundary

Daemon diagnostics are local, private daily NDJSON. Board and Herdr transports emit completion
metadata at their single call boundaries; payload DTOs are never passed to tracing, so redaction does
not depend on a scrubber. Board correlation uses a daemon-generated per-connection sequence rather
than the arbitrary wire request ID. Startup and daily retention delete only exact regular ASCII
`daemon.YYYY-MM-DD.ndjson` files beyond 30 days and fail closed for every other path type/name.
A failed daily writer emits one fixed path-free detached fallback notice per daemon lifetime and
then drops unavailable records, bounding `bootstrap.log` without following symlinks. Lifecycle
traces omit socket paths even when debug filtering is enabled. There is deliberately no raw payload
mode, terminal capture, or telemetry upload.

## 9. Decisions (user-confirmed 2026-07-14)

1. **Language: Rust** — ratatui TUI, rusqlite, tokio daemon; single binary `board` with canonical nested subcommands (`board`, `card`, `column`, `comment`, `run`) plus retained top-level aliases.
2. **Access: overlay keybinding only** (no pinned workspace); `?` shows all keybinds.
3. **DB: `~/.local/share/herdr-board/board.db`** (XDG data; overridable via `BOARD_DB` for tests). Plugin config dir holds only config — DB survives plugin reinstall.
4. **Long-text editing: modal textarea + `Ctrl+E` → `$EDITOR`.**
5. boardd lifecycle: `board tui` auto-starts the daemon if absent; daemon outlives the overlay (runs continue with the board closed; `herdr notification show` covers "done while closed").

6. **Independent canonical-path boards.** Git-root/CWD chooses the pipeline board; `Global` preserves legacy data. The agent's runtime session/workspace remains explicit card configuration and is never inferred from board scope.
7. **No MCP — CLI only.** Agents interact with the board exclusively through the `board` CLI.

## 10. The herdr-board skill

The repo ships a **skill** (`skill/SKILL.md`, optionally installed into an agent's skill directory) teaching agents the canonical `board` CLI: board/card/column CRUD, nested comment and run operations, visibility, JSON errors, and the card lifecycle. It preserves `card new` and the top-level `comment`/`done`/`move`/`cancel`/`retry` aliases. The critical rules remain: always comment results before `board done`; `fail` means "this stage's goal was not met", not "I crashed".

Two consumers:

- **Dispatched card agents**: the column `system_prompt` stays short ("you are in the PLAN stage…, finish with `board done`") because the skill carries the full CLI knowledge; `$BOARD_CARD_ID`/`$BOARD_RUN_ID` arrive via env at spawn.
- **Any interactive agent session** (e.g. the user's main Claude Code): can create/inspect/move cards conversationally — "create a card to fix X in space w4, put it in Plan" — no MCP needed.

Permissions: allowlist `Bash(board *)` (or per-subcommand) so card agents can comment/done without prompts.

## 11. Still open

1. Verify herdr forwards mouse events into panes before promising drag-and-drop (keyboard covers everything regardless).

## 12. Automated testing

Public API behavior belongs in `crates/<crate>/tests/`, so those tests exercise the crate as an
external consumer. Private invariants remain adjacent to the implementation under `src/` in
`#[cfg(test)]` modules; they must not force internal helpers into the public API. The stable module
boundaries are responsibility-oriented: daemon operations/watchers/dispatch/spawner, TUI
app/forms/view, core engine/client, and Herdr events each own their corresponding implementation
and private tests. This is guidance for ownership, not an exhaustive file list.

herdr panes are fully drivable from the CLI (`pane send-keys` with named keys, `pane send-text`, `pane read`, `workspace create/close`), so the board can be tested end-to-end in a collision-resistant ephemeral named Herdr session plus a disposable workspace created for that test. Every interactive/live test must use both and never a user's live session, workspace, or tab.

| Level | What | How |
|---|---|---|
| 1. Unit | column engine, prompt assembly, queue, transitions | plain Rust tests, in-memory SQLite; no herdr |
| 2. TUI snapshot | every view/modal/keybind incl. `?` help | ratatui `TestBackend` + fed `KeyEvent`s + `insta` snapshots; no herdr, no terminal |
| 3. Daemon integration | dispatch → run → done → auto-move, without tokens | config fake harness plus built-in Pi adapter tests; real boardd paths, no provider call |
| 4. Full E2E | real Herdr wiring | collision-resistant ephemeral named Herdr session plus disposable workspace; the standard suite uses checked-in fake Pi/Claude/Codex/OpenCode/configured harnesses and asserts pane-first placement/prompt/argv contracts against the current Herdr 0.8.2 / protocol 20 gate with zero provider cost. Separate opt-in real-Claude Haiku/low, real-Codex low, and real-OpenCode low smokes are never in `run-all.sh`; each intended contract is one authorized attempt with no retry or fallback (and may incur cost). |

Isolation rules for level 3–4: `BOARD_DB=/tmp/…` + dedicated daemon socket per test run so tests never touch the real board; every interactive/live test must create and use a collision-resistant ephemeral named Herdr session plus a disposable workspace, never a user's session, workspace, or tab. The session may run headlessly in CI, but it must retain the same named-session and workspace requirements.
