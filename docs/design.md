# herdr-board — design

## 1. Concepts

| Entity | What it is |
|---|---|
| **Board** | An independent pipeline (columns/config/cards) selected by canonical Git root or non-Git CWD. `Global` preserves the pre-v5 board. A fresh scoped board contains **only a `Todo` column**; everything else is user-created. |
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
- boardd holds a persistent connection to herdr's socket for `events.subscribe`; fallback is polling `herdr api snapshot`.

### Herdr compatibility and launch boundary

The public boardd socket protocol remains **v1**. That is independent of the upstream Herdr socket
contract: this version supports **exactly Herdr 0.7.5 / protocol 17** and has no protocol-16 launch
path. On the card's selected session socket, dispatch first calls `ping` and requires both exact
values. This happens before workspace discovery or `workspace.create`; the spawner repeats the gate
as its first call before `tab.create`, `pane.split`, `agent.start`, or the configured-harness runner.
A mismatch fails the queued run without mutating the workspace.

Protocol 17 is pane-first. boardd creates the `kanban` tab with `cwd` and `env`, using its root pane,
or splits a selected pane in an existing tab with the same `cwd` and `env`. Only then does it start
a managed agent in that exact pane or run a configured harness there. Placement, cwd, and env are
never passed to `agent.start`.

## 3. Data model

See [`../schema.sql`](../schema.sql). Summary:

```
boards(id, name, scope_path)                 -- NULL = Global; canonical path otherwise
columns(id, board_id, name, position, system_prompt, trigger,
        on_success_column_id, on_fail_column_id,
        model_override, effort_override, harness_override, permission_override)
cards(id, board_id, column_id, position, title, description,
      harness, model, effort, permission_mode,
      session,                                   -- herdr session name (NULL = default)
      space_kind ('workspace'|'new_workspace'), space_ref, space_cwd,
      status, awaiting_reason,                   -- reason set in 'awaiting', NULL otherwise
      session_id, created_at, updated_at, archived_at)
comments(id, card_id, author, body, created_at)
runs(id, card_id, column_id, harness, argv_json, prompt_snapshot,
     system_prompt_snapshot,                    -- nullable; enqueue-time, trailer-inclusive
     herdr_workspace_id, herdr_pane_id, session_id,
     session,                                    -- herdr session the run spawned into
     started_at, ended_at, outcome ('ok'|'fail'|'cancelled'|'lost'),  -- 'lost' is legacy, no longer produced
     result_summary, log_path)
```

Schema is versioned via `PRAGMA user_version` (current = **v7**). A fresh DB is built straight from
`schema.sql` and stamped v7. Existing v1→v4 migrations retain their space/session, archive, and Pi
effort behavior. v5 adds unique non-null `boards.scope_path`, preserves board `id=1` plus every
related row as `Global`, and leaves existing card harnesses unchanged. v6 rebuilds `cards` to admit
the `awaiting`/`done` statuses and adds `cards.awaiting_reason` (NULL outside `awaiting`). v7 adds
nullable `runs.system_prompt_snapshot` without backfilling old rows. New runs atomically preserve the
fully resolved, board-protocol-trailer-inclusive system prompt at enqueue time, so a queued run is
not changed by later column edits. A legacy NULL remains a launch-version marker: pre-v7 built-ins
execute their persisted all-in-one argv unchanged, while pre-v7 configured rows retain their
historical spawn-time current-column reconstruction. `Run` deserialization defaults an omitted field
to NULL, but serialization always omits `system_prompt_snapshot` and its contents from boardd wire
responses.

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
only then writes SQLite and emits its coarse change event. A rejected merged state therefore
cannot leave a partial row or event. Card capability policy covers harness, model, effort,
permission, and space combinations; column permission overrides use `PermissionContext::ColumnOverride`
and never allow `bypassPermissions`, while an explicit Claude card value remains valid. Overrides
without a harness are resolved against the entering card at enqueue time, where effective settings
are validated again for legacy rows and concurrent changes.

### Session model

Cards target a **herdr session** plus a space in it. Because two sessions can each show their own workspaces, the daemon must talk to the right socket per card — the old single-socket model showed the wrong session's workspaces.

- **Registry**: session enumeration is not in the herdr socket API (a session only knows itself), so the daemon shells out to `herdr session list --json` (binary via `$HERDR_BIN_PATH`, else `herdr`), caching ~3s. It maps `name/default/running/socket_path`.
- **Default**: a card with `session = null` uses the daemon's own bound herdr socket; its display name is the registry entry whose `socket_path` matches (else the synthetic `"default"`).
- **Per-session client**: spawn / kill / liveness / workspace resolve-or-create all build a `HerdrClient` on the resolved session socket (carried on `SpawnReq`/`SpawnHandle` as `herdr_socket`, and persisted as `runs.session` so kill/liveness work after a daemon restart).
- **Per-session watchers**: a single watcher thread multiplexes one `HerdrEvents` stream **per session socket** that has active panes (agent-status subscriptions are validated per socket). It holds a `socket → stream` map, rebuilds all streams on a watch-set generation change, and (re)connects any watched socket missing a live stream — simpler than per-session thread lifecycles while fitting the existing loop. Event identity is the tuple `(session socket, pane id)`, not pane id alone.

### new_workspace flow

On first dispatch of a `new_workspace` card: preflight the selected socket for exact Herdr
0.7.5/protocol 17, then list the session's workspaces; if one's label matches `space_ref`
(case-insensitive) reuse it, else `workspace.create {label:space_ref, cwd:space_cwd, focus:false}`.
Then proceed identically to a `workspace` card (cwd snapshot, pane-first kanban-tab placement). If the reused or existing workspace snapshot fails, or contains no live cwd, dispatch fails; it never falls back to process cwd or a stale snapshot.

### Worktree removal

The `cwd` and `worktree` space kinds are gone. Worktree isolation is now the **agent's** job — instructed via the column/card prompt (create a worktree, work in it) — not a board primitive, keeping the board's space model to "which session, which workspace".

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
- Column `system_prompt` is combined with the mandatory board-protocol trailer and snapshotted at enqueue. For managed Pi it is delivered through a temporary file passed to `--append-system-prompt`; for managed Claude the file flag is `--append-system-prompt-file`. Neither replaces harness defaults/context files, and neither puts the system text directly in startup argv. It can invoke skills (`/quick-planner`, `/code-review`) — that's how "column triggers a skill" works, no special mechanism needed.
- `on_fail = "Execute"` from Review + comments-as-context gives the fix loop for free: the re-entered Execute run sees the reviewer's findings in its prompt.

### Scope selection

At the CLI/TUI boundary, non-empty `BOARD_SCOPE_PATH` wins. TUI otherwise uses
`HERDR_PLUGIN_CONTEXT_JSON.focused_pane_cwd`, then `workspace_cwd`, then process CWD. The candidate
is canonicalized; `git -C <candidate> rev-parse --show-toplevel` selects a canonical Git root,
while a non-Git directory keeps its exact canonical CWD. Subdirectories of one repo therefore share
a board; equal basenames at different paths do not. Moving/renaming a path does not migrate its old
board, which remains available in the picker.

`o` is daemon-mediated: `run.focus` chooses the card's newest run with a recorded pane, resolves the
run's session socket, canonicalizes and compares it with the invoking plugin's
`HERDR_SOCKET_PATH`, then calls Herdr socket method `pane.focus {pane_id}`. Cross-session jumps are
refused; success exits the overlay, while missing/stale panes and Herdr errors remain visible as a
toast.

### TUI interactions (v1)

- **Access: overlay only** — `[[keys.command]]` keybinding (e.g. `prefix+k`) → `plugin pane open --plugin herdr-board --placement overlay`; the board floats over the current workspace from anywhere, dismiss to drop back. No pinned workspace, no sidebar entry (herdr has no sidebar extension point — verified against api schema/config).
- **Responsive board view:** visible columns divide the entire viewport while preserving a readable minimum width; when not all columns fit, the selected column drives a full-width sliding window. Cards use a status-colored marker, readable selected background, harness/model metadata, status glyphs (▶ running, ⏸ blocked, ✗ failed, ⧗ queued, ? awaiting — yellow, ✓ done — green), and a live run timer.
- **Card detail:** opens as a contextual popup and toggles fullscreen with `f` or its clickable title
  action. Status fields use blue labels and white values. Description, comments, and runs size to
  their content; comments and runs scroll independently (`Tab` selects, arrows/`k`/`j` or mouse
  wheel scroll), with a blue divider for the focused history. Histories open at the latest item and
  show only directional arrows (no counts) when content is hidden. `e` edits the card and returns to
  detail after save/cancel. `Enter` on an `awaiting` card confirms completion (the same `run.done
  ok` channel as `board done ok`); the detail view shows the `awaiting` reason (agent reported
  done / idle past grace).
- Mouse **and** keyboard for everything: `b` switches between `Global` and scoped boards; drag card
  between columns / `m` move; `n` new card, `N` new column; `e` edit card; `a` archive/restore; `v`
  cycles `ACTIVE` / `ALL` / `ARCHIVED`; `c` comment; `Enter` card detail; `o` focuses the latest
  recorded run pane when it belongs to the current Herdr session; `r` refreshes the selected board
  on demand); `?` help overlay listing **all** keybinds; column config form (rename, system prompt,
  trigger, on_success/on_fail, overrides, reorder, delete). The filter is rendered in the Herdr pane
  title (`Board [<scope> · ACTIVE|ALL|ARCHIVED]`) while the footer contains only `? help`. Archived cards are
  inert until restored and render dimmed with `▣ ARCHIVED` when visible.
- **Content-sized overlays:** card/column forms, move pickers, and help panels shrink to their content on large terminals and clamp to the available viewport on small terminals.
- **Guided card & column forms** share one metadata source: both fetch `harness.capabilities` (models/efforts/permissions via the daemon-side `HarnessMeta` adapter trait) and `harness.list` (built-ins + config-defined harnesses). For cards: Pi is selected for new cards; Claude remains selectable. On open/harness change the form also fetches `space.list`. Model starts at `(default)` (unset), then catalog aliases and `(custom)` when free-form is supported. Effort follows the selected model's declared set, or the catalog default for omitted/free-form models. Permission is hidden and submits `None` for Pi; Claude shows its modes. Switching harness resets only incompatible values. Workspace labels are shown but ids are persisted. Fetch failures degrade to free text with a warning. For column config the same source drives the override fields: `harness_override` is a **select** over the available harnesses (`(none)` = no override), `effort_override` follows the override harness's catalog, and `permission_override` is **hidden** when the driving harness has no permission modes (e.g. Pi); changing the override harness refetches capabilities and resets only overrides that became invalid.
- Long text (card description, column system prompt): modal textarea, `Ctrl+E` suspends the TUI into `$EDITOR`.
- Deleting a column with cards asks where to move them; a running card's column can't be deleted.
- Optional: apply a board template (e.g. the example pipeline above) onto an empty board.

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
```

After pane-first placement, boardd writes `system_prompt` to a temporary mode-`0600` file and calls
Herdr protocol 17 as follows:

```
agent.start {
  name, kind:"pi"|"claude", pane_id,
  args:<startup argv without executable> +
       ["--append-system-prompt", FILE]          # Pi
       ["--append-system-prompt-file", FILE],    # Claude
  timeout_ms:30000
}
agent.get {target:pane_id}       # bounded polling until interactive_ready && !launch_pending
agent.prompt {target:pane_id, text:task_prompt}
```

The temporary file is removed before spawn returns, on success or failure. The card prompt is never
part of `agent.start`; it is submitted only after readiness. An `agent_name_taken` response retries
once on the same owned pane with `card-<id>-<column-slug>-r<run>`.

Configured harnesses use the same pane-first cwd/env placement, then `pane.rename`. Because direct
`herdr pane run` does not preserve complex argv boundaries, boardd writes one mode-`0700`,
self-removing script with every configured argv element POSIX-quoted and invokes exactly
`herdr pane run <pane_id> <script_path>` with `HERDR_SOCKET_PATH` set to the selected session socket.
The script runs the exact child argv and preserves its exit status. When the child returns it invokes
the hidden `board __pane-exited --run-id "$BOARD_RUN_ID"` guard. That guard sends internal
`run.pane_exited {card_id,run_id}`; only the exact matching open queued or started configured run is failed, with no `on_fail`
transition. A callback before registration is accepted. The same narrow race rule applies to an
immediate configured-harness `board done`: the CLI forwards `BOARD_RUN_ID`, and only that exact
queued run may finalize before runner registration; a queued built-in Pi/Claude completion is
rejected because no managed pane exists yet. For an already-started run, `run_id` remains optional
so manual/TUI callers remain compatible, but a supplied mismatched id is rejected. Stale,
replaced, completed, and built-in callbacks are rejected, so a stale child cannot complete a
replacement. An already-completed or replaced run is rejected and the wrapper ignores that expected
error. The script deletes itself when it starts; if the pane runner fails synchronously, boardd
removes it and closes only the pane boardd allocated. If scheduling succeeds but the pane never
opens the script, the residual configured-script orphan is an accepted asynchronous limitation.

- **Session strategy**: Pi's first auto column mints an exact `--session-id`; later stages reuse it; retry uses `--fork <old> --session-id <new>` and persists the new target. Claude keeps exact mint, `--resume`, and `--fork-session`. Column config can force `fresh_session = true`.
- **Configured harnesses** remain unmanaged. Their exact configured argv is not inferred to be Pi or Claude; prompt channels arrive as `BOARD_PROMPT` / `BOARD_SYSTEM_PROMPT`. The configured runner resolves a nonempty `HERDR_BIN_PATH`, otherwise `herdr`.

## 6. Data flow — the canonical walkthrough

1. **Create** card in *Todo*: title "Add retry to MELI scraper", description (prompt), harness=pi (default), model omitted (Pi configured default), effort=low, no permission mode, space=workspace `w4`.
2. **User drags card → Plan** (TUI → boardd `card.move`).
3. Column engine: *Plan* is `trigger=auto` → **enqueue run** on the card's space queue.
4. Dispatcher (respecting per-space serial queue + global cap):
   a. Resolve the card's session socket and `ping` it. Anything except exact Herdr 0.7.5/protocol 17 fails before workspace discovery/creation. Then reuse workspace `w4`, or create/reuse the card's labeled `new_workspace`; repository worktree isolation remains an agent prompt responsibility.
   b. Preflight the selected socket again at the spawner boundary. In the workspace's **`kanban` tab**, `tab.create {workspace_id,cwd,env,…}` supplies a new root pane, or an existing tab's largest pane is selected and `pane.split {target_pane_id,cwd,env,…}` creates the owned pane (`Right` when the target is ≥ 2× as wide as tall in cells, else `Down`). There is no protocol-16 placement inside `agent.start` and no leftover root shell to close.
   c. For Pi/Claude, write the snapshotted system prompt to a mode-`0600` temporary file; issue `agent.start {name,kind,pane_id,args}` with prompt-free startup args; poll `agent.get` for readiness; then send only the task snapshot through `agent.prompt`. Remove the file. Card status → `running`; record the exact pane/workspace ids. The pane is **visible** — you can watch or type into it anytime.

   **Pane naming and ownership**: the managed agent name is `card-<id>-<column-slug>` (e.g. `card-42-plan`, `card-42-execute`). Herdr names are exclusive while a pane is open, so `agent_name_taken` retries once on the same pane with `card-<id>-<column-slug>-r<run>`. If a placement target disappears, boardd closes only the pane it created (a missing pane is already clean), restarts discovery from `tab.list`, and retries the complete placement once. A terminal launch error also closes only that board-owned pane; pre-existing user panes are never cleanup targets.
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
queued built-in Pi/Claude runs are deliberately not eligible. Pane-idle scraping alone is the
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

- **Atomic enqueue snapshot**: scheduler→store locking builds the card/column/comments/settings/task,
  system, and session snapshot together; the queued run is never assembled from stale reads and
  `run.session` matches the launch target.
- **Lifecycle policy ownership**: `board-core::engine` owns Herdr-neutral `LifecycleDecision`/
  `FinalizePlan`, auto-hop limits, and resumability evidence (a started run plus its
  `agent:<run_id>` comment). `board-daemon` gathers facts, performs DB/process effects, and
  preserves the scheduler→store lock order; this slice does not change transaction execution.
- **Atomic card finalization**: the scheduler claims a card while closing/removing its old run and
  keeps that claim through comments, transition/status writes, and any internal auto-target
  enqueue. Public enqueue/retry and conflicting card/column mutations reject the claimed card;
  only that finalizer's private enqueue token may create its next run.
- **Per-space FIFO**: two agents mutating one working tree collide; cards sharing a `(session, space_kind, space_ref)` key run serially.
- **Global semaphore** (default 3) caps concurrent runs across spaces (cost + machine load).
- A `new_workspace` card that opens a distinct workspace per label gets its own queue key, so distinct labels run in parallel (up to the global cap). Agent-driven worktree isolation (see §3) is what escapes a per-repo bottleneck now.

## 8. Failure & safety rails

- Per-run timeout (column-configurable) → kill pane, run `fail`, card to `on_fail`.
- `--max-budget-usd` per run (Claude supports it in print mode; interactive panes rely on timeout + human visibility).
- Pi has no board tool-permission mode; no permission/approval flag is added and explicit Pi permission is rejected. Claude `bypassPermissions` requires explicit per-card opt-in, never a column default.
- Cards never auto-move into *Done*; last auto hop is always a human-gated column.
- Retry = a new run; Pi uses `--fork <old> --session-id <new>`, Claude uses `--resume <old> --fork-session`; history is preserved.

## 9. Decisions (user-confirmed 2026-07-14)

1. **Language: Rust** — ratatui TUI, rusqlite, tokio daemon; single binary `board` with subcommands (`tui`, `daemon`, `comment`, `done`, `move`, `card`).
2. **Access: overlay keybinding only** (no pinned workspace); `?` shows all keybinds.
3. **DB: `~/.local/share/herdr-board/board.db`** (XDG data; overridable via `BOARD_DB` for tests). Plugin config dir holds only config — DB survives plugin reinstall.
4. **Long-text editing: modal textarea + `Ctrl+E` → `$EDITOR`.**
5. boardd lifecycle: `board tui` auto-starts the daemon if absent; daemon outlives the overlay (runs continue with the board closed; `herdr notification show` covers "done while closed").

6. **Independent canonical-path boards.** Git-root/CWD chooses the pipeline board; `Global` preserves legacy data. The agent's runtime session/workspace remains explicit card configuration and is never inferred from board scope.
7. **No MCP — CLI only.** Agents interact with the board exclusively through the `board` CLI.

## 10. The herdr-board skill

The repo ships a **skill** (`skill/SKILL.md`, optionally installed into an agent's skill directory) teaching agents the `board` CLI: command reference (`board card new/show/list`, `board comment`, `board move`, `board done --outcome ok|fail`), the card lifecycle, and the rules (always comment results *before* `board done`; `fail` means "this stage's goal was not met", not "I crashed").

Two consumers:

- **Dispatched card agents**: the column `system_prompt` stays short ("you are in the PLAN stage…, finish with `board done`") because the skill carries the full CLI knowledge; `$BOARD_CARD_ID`/`$BOARD_RUN_ID` arrive via env at spawn.
- **Any interactive agent session** (e.g. the user's main Claude Code): can create/inspect/move cards conversationally — "create a card to fix X in space w4, put it in Plan" — no MCP needed.

Permissions: allowlist `Bash(board *)` (or per-subcommand) so card agents can comment/done without prompts.

## 11. Still open

1. Verify herdr forwards mouse events into panes before promising drag-and-drop (keyboard covers everything regardless).

## 12. Automated testing

herdr panes are fully drivable from the CLI (`pane send-keys` with named keys, `pane send-text`, `pane read`, `workspace create/close`), so the board can be tested end-to-end inside a real herdr — in a disposable test workspace or an isolated named session.

| Level | What | How |
|---|---|---|
| 1. Unit | column engine, prompt assembly, queue, transitions | plain Rust tests, in-memory SQLite; no herdr |
| 2. TUI snapshot | every view/modal/keybind incl. `?` help | ratatui `TestBackend` + fed `KeyEvent`s + `insta` snapshots; no herdr, no terminal |
| 3. Daemon integration | dispatch → run → done → auto-move, without tokens | config fake harness plus built-in Pi adapter tests; real boardd paths, no provider call |
| 4. Full E2E | real Herdr wiring | disposable named session/workspace; the standard suite uses checked-in fake Pi/Claude/configured harnesses and asserts protocol-17 placement/prompt/argv contracts with zero provider cost. A separate opt-in real-Claude Haiku/low smoke is never in `run-all.sh`; its intended contract is one authorized attempt with no retry or fallback. |

Isolation rules for level 3–4: `BOARD_DB=/tmp/…` + dedicated daemon socket per test run so tests never touch the real board; prefer a separate `herdr --session board-test` (or headless `herdr server`) in CI so the user's session is untouched; inside an interactive dev loop, a throwaway workspace in the live session is fine.
