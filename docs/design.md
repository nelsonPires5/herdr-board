# herdr-board — design

## 1. Concepts

| Entity | What it is |
|---|---|
| **Board** | An independent pipeline (columns/config/cards) selected by canonical Git root or non-Git CWD. `Global` preserves the pre-v5 board. A fresh scoped board contains **only a `Todo` column**; everything else is user-created. |
| **Column** | A stage, entirely user-defined: create/rename/reorder/delete from the TUI (keyboard or mouse). Config: `system_prompt`, `trigger` (`auto` = entering the column starts a run; `manual` = waits for human), `on_success` / `on_fail` (move card to column X, or stay), optional overrides (model/effort/harness) applied to every card passing through. Nothing about column names or count is hardcoded. |
| **Card** | A unit of work. Title, **description = the base prompt**, harness, model, effort, permission mode, a **herdr session** (`session`, null = daemon default) AND a **space** within it (`workspace` = an already-open workspace id; `new_workspace` = a label + cwd the daemon opens on first dispatch), position, live status (`idle · queued · running · blocked · failed`), the harness `session_id` for resume, and an optional `archived_at` timestamp. Archiving is reversible and preserves comments/run history. |
| **Comment** | Timestamped note on a card. Author = `user`, `agent` (from a run), or `system` (daemon transitions). Comments are both the audit log **and** context for the next run. |
| **Run** | One agent execution of a card in a column: argv, herdr pane/workspace ids, session id, started/ended, exit status, result summary. Cards keep full run history (retries = new runs). |

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
   workspace.create · agent.start · agent.send
   events.subscribe(pane_agent_status_changed, pane_exited) · pane.read
   notification.show
```

- **boardd** is the only SQLite writer. One DB at `~/.local/share/herdr-board/board.db` stores every independent scoped board; cards still explicitly target Herdr sessions/workspaces.
- **TUI** is packaged as a herdr plugin: `herdr-plugin.toml` declares a `[[panes]]` entry (herdr spawns the TUI binary in a split/tab) and `[[actions]]` (e.g. "add focused pane's repo as a card") bindable via `[[keys.command]]`. Plugin processes receive `HERDR_BIN_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`.
- **`board` CLI** subcommands hit the boardd socket — never SQLite directly (single-writer rule).
- boardd holds a persistent connection to herdr's socket for `events.subscribe`; fallback is polling `herdr api snapshot`.

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
      status, session_id, created_at, updated_at, archived_at)
comments(id, card_id, author, body, created_at)
runs(id, card_id, column_id, harness, argv_json, prompt_snapshot,
     herdr_workspace_id, herdr_pane_id, session_id,
     session,                                    -- herdr session the run spawned into
     started_at, ended_at, outcome ('ok'|'fail'|'cancelled'|'lost'),
     result_summary, log_path)
```

Schema is versioned via `PRAGMA user_version` (current = **v5**). A fresh DB is built straight from
`schema.sql` and stamped v5. Existing v1→v4 migrations retain their space/session, archive, and Pi
effort behavior. v5 adds unique non-null `boards.scope_path`, preserves board `id=1` plus every
related row as `Global`, and leaves existing card harnesses unchanged.

### Session model

Cards target a **herdr session** plus a space in it. Because two sessions can each show their own workspaces, the daemon must talk to the right socket per card — the old single-socket model showed the wrong session's workspaces.

- **Registry**: session enumeration is not in the herdr socket API (a session only knows itself), so the daemon shells out to `herdr session list --json` (binary via `$HERDR_BIN_PATH`, else `herdr`), caching ~3s. It maps `name/default/running/socket_path`.
- **Default**: a card with `session = null` uses the daemon's own bound herdr socket; its display name is the registry entry whose `socket_path` matches (else the synthetic `"default"`).
- **Per-session client**: spawn / kill / liveness / workspace resolve-or-create all build a `HerdrClient` on the resolved session socket (carried on `SpawnReq`/`SpawnHandle` as `herdr_socket`, and persisted as `runs.session` so kill/liveness work after a daemon restart).
- **Per-session watchers**: a single watcher thread multiplexes one `HerdrEvents` stream **per session socket** that has active panes (agent-status subscriptions are validated per socket). It holds a `socket → stream` map, rebuilds all streams on a watch-set generation change, and (re)connects any watched socket missing a live stream — simpler than per-session thread lifecycles while fitting the existing loop.

### new_workspace flow

On first dispatch of a `new_workspace` card: list the session's workspaces; if one's label matches `space_ref` (case-insensitive) reuse it, else `workspace.create {label:space_ref, cwd:space_cwd}`. Then proceed identically to a `workspace` card (cwd snapshot, kanban tab, grid layout).

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
- Column `system_prompt` is delivered via `--append-system-prompt` (never `--system-prompt`) so harness defaults and context files stay intact. It can invoke skills (`/quick-planner`, `/code-review`) — that's how "column triggers a skill" works, no special mechanism needed.
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
- **Responsive board view:** visible columns divide the entire viewport while preserving a readable minimum width; when not all columns fit, the selected column drives a full-width sliding window. Cards use a status-colored marker, readable selected background, harness/model metadata, status glyphs (▶ running, ⏸ blocked, ✗ failed), and a live run timer.
- **Card detail:** opens as a contextual popup and toggles fullscreen with `f` or its clickable title
  action. Status fields use blue labels and white values. Description, comments, and runs size to
  their content; comments and runs scroll independently (`Tab` selects, arrows/`k`/`j` or mouse
  wheel scroll), with a blue divider for the focused history. Histories open at the latest item and
  show only directional arrows (no counts) when content is hidden. `e` edits the card and returns to
  detail after save/cancel.
- Mouse **and** keyboard for everything: `b` switches between `Global` and scoped boards; drag card
  between columns / `m` move; `n` new card, `N` new column; `e` edit card; `a` archive/restore; `v`
  cycles `ACTIVE` / `ALL` / `ARCHIVED`; `c` comment; `Enter` card detail; `o` focuses the latest
  recorded run pane when it belongs to the current Herdr session; `r` refreshes the selected board
  on demand); `?` help overlay listing **all** keybinds; column config form (rename, system prompt,
  trigger, on_success/on_fail, overrides, reorder, delete). The filter is rendered in the Herdr pane
  title (`Board [<scope> · ACTIVE|ALL|ARCHIVED]`) while the footer contains only `? help`. Archived cards are
  inert until restored and render dimmed with `▣ ARCHIVED` when visible.
- **Content-sized overlays:** card/column forms, move pickers, and help panels shrink to their content on large terminals and clamp to the available viewport on small terminals.
- **Guided card & column forms** share one metadata source: both fetch `harness.capabilities` (models/efforts/permissions via the daemon-side `HarnessMeta` adapter trait) and `harness.list` (built-ins + config-defined harnesses). For cards: Pi is selected for new cards; Claude remains selectable. On open/harness change the form also fetches `space.list`. Model starts at `(default)` (unset), then catalog aliases and `(custom)` when free-form is supported. Effort follows the selected/default model. Permission is hidden and submits `None` for Pi; Claude shows its modes. Switching harness resets only incompatible values. Workspace labels are shown but ids are persisted. Fetch failures degrade to free text with a warning. For column config the same source drives the override fields: `harness_override` is a **select** over the available harnesses (`(none)` = no override), `effort_override` follows the override harness's catalog, and `permission_override` is **hidden** when the driving harness has no permission modes (e.g. Pi); changing the override harness refetches capabilities and resets only overrides that became invalid.
- Long text (card description, column system prompt): modal textarea, `Ctrl+E` suspends the TUI into `$EDITOR`.
- Deleting a column with cards asks where to move them; a running card's column can't be deleted.
- Optional: apply a board template (e.g. the example pipeline above) onto an empty board.

## 5. Prompt assembly

For each run, boardd builds:

```
argv  = pi [--model <card|column override>] [--thinking <effort>]
           --append-system-prompt <column.system_prompt + board protocol trailer>
           [--session-id <exact-id> | --fork <old-id> --session-id <new-id>]
        # explicit --harness claude retains Claude's model/effort/permission argv
prompt = <card.description>
         + "\n\n## Card comments so far\n" + last N comments (author, ts, body)
env    = BOARD_CARD_ID=<id>, BOARD_RUN_ID=<id>, BOARD_SOCKET=<path>
```

- **Session strategy**: Pi's first auto column mints an exact `--session-id`; later stages reuse it; retry uses `--fork <old> --session-id <new>` and persists the new target. Claude keeps exact mint, `--resume`, and `--fork-session`. Column config can force `fresh_session = true`.
- `prompt_snapshot` is stored on the run — reproducibility and debugging.

## 6. Data flow — the canonical walkthrough

1. **Create** card in *Todo*: title "Add retry to MELI scraper", description (prompt), harness=pi (default), model omitted (Pi configured default), effort=low, no permission mode, space=workspace `w4`.
2. **User drags card → Plan** (TUI → boardd `card.move`).
3. Column engine: *Plan* is `trigger=auto` → **enqueue run** on the card's space queue.
4. Dispatcher (respecting per-space serial queue + global cap):
   a. Resolve space: reuse workspace `w4`, or create/reuse the card's labeled `new_workspace`; repository worktree isolation remains an agent prompt responsibility.
   b. Place the pane in the workspace's **`kanban` tab** (find-or-create it; a fresh tab is filled unsplit then its leftover shell pane is closed, an existing tab splits its largest pane — `Right` when that pane is ≥ 2× as wide as tall in cells, else `Down`, so N panes tile ≈ square). `herdr agent start card-42-plan --workspace w4 --tab <kanban> [--split right|down] --env BOARD_CARD_ID=42 … -- pi --thinking low --session-id <uuid> --append-system-prompt "<Plan prompt + protocol>" "Card task:\n<description + comments>"`.
   c. Card status → `running`; run row created with pane id. The pane is **visible** — you can watch or type into it anytime.

   **Pane naming**: the herdr agent name is `card-<id>-<column-slug>` (e.g. `card-42-plan`, `card-42-execute`) — stable and readable per column. herdr agent names are exclusive while a pane is open, so on an `agent_name_taken` collision the daemon retries once with the run-scoped fallback `card-<id>-<column-slug>-r<run>`.
5. Agent plans, writes `docs/plans/meli-retry.md`, then calls `board comment 42 "Plan ready at docs/plans/meli-retry.md …"` and `board done 42 --outcome ok`.
6. boardd receives `done` → closes the run (`outcome=ok`), posts a `system` comment ("Plan finished in 4m12s, $0.38"), applies `on_success` → **card auto-moves to Execute** → step 3 repeats with the Execute column prompt, `--resume <session>`.
7. Execute finishes → comment → auto-move to *Review* → Review run (fresh session, model override) → verdict comment.
   - `--outcome ok` → card lands in **Human Review**: `trigger=manual`, boardd fires `herdr notification show "Card #42 ready for human review" --sound request`.
   - `--outcome fail` → card goes back to **Execute** with the findings as comments; loop.
8. **Human** opens the pane / diff, optionally comments, drags to *Done* (or back to Execute — manual moves into auto columns also trigger runs, so "drag back with a comment" = feedback loop).

### Completion detection (belt and suspenders)

| Signal | Source | Role |
|---|---|---|
| `board done <card> --outcome …` | agent itself (instructed by every auto-column's system prompt) | **primary** — explicit, carries semantics |
| `pane_agent_status_changed` → `working→idle` sustained | herdr events (optionally install `herdr integration install pi`; Claude has its equivalent) | agent finished but forgot to call `board done` → mark run `lost`, notify human instead of guessing |
| `pane_agent_status_changed` → `blocked` | herdr events | agent/integration reports blocked (provider retry exhaustion or input need) → card `blocked`, board change + notification |
| `pane_exited` | herdr events | crash / closed pane → run `fail` |

Pane-idle scraping alone is the documented weak point of every tmux-style orchestrator (claude-squad); the explicit `board done` channel is what makes auto-transition trustworthy. Without the optional Pi integration, spawn, explicit completion, timeout, and pane exit remain deterministic, while working/blocked/idle and idle-lost are unavailable.

## 7. Queueing & concurrency

- **Per-space FIFO**: two agents mutating one working tree collide; cards sharing a `(session, space_kind, space_ref)` key run serially.
- **Global semaphore** (default 3) caps concurrent runs across spaces (cost + machine load).
- A `new_workspace` card that opens a distinct workspace per label gets its own queue key, so distinct labels run in parallel (up to the global cap). Agent-driven worktree isolation (see §3) is what escapes a per-repo bottleneck now.

## 8. Failure & safety rails

- Per-run timeout (column-configurable) → kill pane, run `fail`, card to `on_fail`.
- `--max-budget-usd` per run (Claude supports it in print mode; interactive panes rely on timeout + human visibility).
- Pi has no board tool-permission mode; no permission/approval flag is added and explicit Pi permission is rejected. Claude `bypassPermissions` requires explicit per-card opt-in, never a column default.
- Cards never auto-move into *Done*; last auto hop is always a human-gated column.
- Retry = new run (`--fork-session`); history preserved.

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
| 4. Full E2E | real Herdr wiring | disposable named session/workspace; standard suite shadows only `pi` with a checked-in fake and asserts mint/fork argv with zero provider cost. A separate opt-in real-Pi smoke is never in `run-all.sh`. |

Isolation rules for level 3–4: `BOARD_DB=/tmp/…` + dedicated daemon socket per test run so tests never touch the real board; prefer a separate `herdr --session board-test` (or headless `herdr server`) in CI so the user's session is untouched; inside an interactive dev loop, a throwaway workspace in the live session is fine.
