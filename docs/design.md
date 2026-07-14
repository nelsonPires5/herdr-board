# herdr-board — design

## 1. Concepts

| Entity | What it is |
|---|---|
| **Board** | A pipeline definition: an ordered set of columns. One default board; more allowed (e.g. "features", "ops"). A fresh board contains **only a `Todo` column** — everything else is user-created. |
| **Column** | A stage, entirely user-defined: create/rename/reorder/delete from the TUI (keyboard or mouse). Config: `system_prompt`, `trigger` (`auto` = entering the column starts a run; `manual` = waits for human), `on_success` / `on_fail` (move card to column X, or stay), optional overrides (model/effort/harness) applied to every card passing through. Nothing about column names or count is hardcoded. |
| **Card** | A unit of work. Title, **description = the base prompt**, harness, model, effort, permission mode, target **space** (herdr workspace / cwd, optionally "fresh worktree"), position, live status (`idle · queued · running · blocked · failed`), and the harness `session_id` for resume. |
| **Comment** | Timestamped note on a card. Author = `user`, `agent` (from a run), or `system` (daemon transitions). Comments are both the audit log **and** context for the next run. |
| **Run** | One agent execution of a card in a column: argv, herdr pane/workspace ids, session id, started/ended, exit status, result summary. Cards keep full run history (retries = new runs). |

Separation card ↔ run is deliberate (vibe-kanban converged on task/attempt/execution after painful migrations): a card can be re-run, moved back, or forked without losing history.

## 2. Architecture

```
┌───────────────────────────── herdr session ─────────────────────────────┐
│  ┌────────────── pane ─────────────┐   ┌───────── pane (ws w4) ───────┐ │
│  │  board TUI (herdr plugin pane)  │   │  claude … (card #42 run)     │ │
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
   workspace.create · worktree.create · agent.start · agent.send
   events.subscribe(pane_agent_status_changed, pane_exited) · pane.read
   notification.show
```

- **boardd** is the only SQLite writer. State in `~/.local/share/herdr-board/board.db` (global — cards target spaces in different repos).
- **TUI** is packaged as a herdr plugin: `herdr-plugin.toml` declares a `[[panes]]` entry (herdr spawns the TUI binary in a split/tab) and `[[actions]]` (e.g. "add focused pane's repo as a card") bindable via `[[keys.command]]`. Plugin processes receive `HERDR_BIN_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`.
- **`board` CLI** subcommands hit the boardd socket — never SQLite directly (single-writer rule).
- boardd holds a persistent connection to herdr's socket for `events.subscribe`; fallback is polling `herdr api snapshot`.

## 3. Data model

See [`../schema.sql`](../schema.sql). Summary:

```
boards(id, name)
columns(id, board_id, name, position, system_prompt, trigger,
        on_success_column_id, on_fail_column_id,
        model_override, effort_override, harness_override, permission_override)
cards(id, board_id, column_id, position, title, description,
      harness, model, effort, permission_mode,
      space_kind ('workspace'|'cwd'|'worktree'), space_ref, worktree_base,
      status, session_id, created_at, updated_at)
comments(id, card_id, author, body, created_at)
runs(id, card_id, column_id, harness, argv_json, prompt_snapshot,
     herdr_workspace_id, herdr_pane_id, session_id,
     started_at, ended_at, outcome ('ok'|'fail'|'cancelled'|'lost'),
     result_summary, log_path)
```

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
- Column `system_prompt` is delivered via `--append-system-prompt` (never `--system-prompt`) so harness defaults/CLAUDE.md stay intact. It can invoke skills (`/quick-planner`, `/code-review`) — that's how "column triggers a skill" works, no special mechanism needed.
- `on_fail = "Execute"` from Review + comments-as-context gives the fix loop for free: the re-entered Execute run sees the reviewer's findings in its prompt.

### TUI interactions (v1)

- **Access: overlay only** — `[[keys.command]]` keybinding (e.g. `prefix+k`) → `plugin pane open --plugin herdr-board --placement overlay`; the board floats over the current workspace from anywhere, dismiss to drop back. No pinned workspace, no sidebar entry (herdr has no sidebar extension point — verified against api schema/config).
- Board view: columns side by side, cards with status glyphs (▶ running, ⏸ blocked, ✗ failed) and live run timer.
- Mouse **and** keyboard for everything: drag card between columns / `m` move; `n` new card, `N` new column; `e` edit card; `c` comment; `Enter` card detail (description, config, comments, run history); `o` jump to the card's herdr pane; `?` help overlay listing **all** keybinds; column config form (rename, system prompt, trigger, on_success/on_fail, overrides, reorder, delete).
- Long text (card description, column system prompt): modal textarea, `Ctrl+E` suspends the TUI into `$EDITOR`.
- Deleting a column with cards asks where to move them; a running card's column can't be deleted.
- Optional: apply a board template (e.g. the example pipeline above) onto an empty board.

## 5. Prompt assembly

For each run, boardd builds:

```
argv  = claude --model <card|column override> --effort <…> --permission-mode <…>
               --append-system-prompt <column.system_prompt>
               [--resume <card.session_id> | --session-id <new-uuid>]
prompt = <card.description>
         + "\n\n## Card comments so far\n" + last N comments (author, ts, body)
env    = BOARD_CARD_ID=<id>, BOARD_RUN_ID=<id>, BOARD_SOCKET=<path>
```

- **Session strategy**: first auto column mints a `--session-id` (stored on the card); later columns `--resume` it so Execute literally continues the Plan conversation. A card moved back and re-run uses `--fork-session` to retry without polluting history. Column config can force `fresh_session = true` (e.g. Review should judge the diff, not trust its own memory).
- `prompt_snapshot` is stored on the run — reproducibility and debugging.

## 6. Data flow — the canonical walkthrough

1. **Create** card in *Todo*: title "Add retry to MELI scraper", description (prompt), harness=claude, model=sonnet, effort=high, permission=acceptEdits, space=workspace `w4` (or "worktree off main").
2. **User drags card → Plan** (TUI → boardd `card.move`).
3. Column engine: *Plan* is `trigger=auto` → **enqueue run** on the card's space queue.
4. Dispatcher (respecting per-space serial queue + global cap):
   a. Resolve space: reuse workspace `w4`, or `worktree.create --base main` + `workspace.create` for isolation.
   b. `herdr agent start board-card-42 --workspace w4 --env BOARD_CARD_ID=42 … -- claude --model sonnet --effort high --permission-mode acceptEdits --session-id <uuid> --append-system-prompt "<Plan system prompt>" "<card description + comments>"`.
   c. Card status → `running`; run row created with pane id. The pane is **visible** — you can watch or type into it anytime.
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
| `pane_agent_status_changed` → `working→idle` sustained | herdr events (install `herdr integration install claude` for hook-precise status) | agent finished but forgot to call `board done` → mark run `lost`, notify human instead of guessing |
| `pane_agent_status_changed` → `blocked` | herdr events | agent hit a permission prompt → card status `blocked`, herdr notification |
| `pane_exited` | herdr events | crash / closed pane → run `fail` |

Pane-idle scraping alone is the documented weak point of every tmux-style orchestrator (claude-squad); the explicit `board done` channel is what makes auto-transition trustworthy.

## 7. Queueing & concurrency

- **Per-space FIFO**: two agents mutating one working tree collide; cards targeting the same workspace/cwd run serially.
- **Global semaphore** (default 3) caps concurrent runs across spaces (cost + machine load).
- `space_kind=worktree` escapes the per-repo queue: each card gets its own worktree/branch (herdr `worktree create`), unbounded parallelism, merge at Human Review.

## 8. Failure & safety rails

- Per-run timeout (column-configurable) → kill pane, run `fail`, card to `on_fail`.
- `--max-budget-usd` per run (Claude supports it in print mode; interactive panes rely on timeout + human visibility).
- `bypassPermissions` requires explicit per-card opt-in, never a column default.
- Cards never auto-move into *Done*; last auto hop is always a human-gated column.
- Retry = new run (`--fork-session`); history preserved.

## 9. Decisions (user-confirmed 2026-07-14)

1. **Language: Rust** — ratatui TUI, rusqlite, tokio daemon; single binary `board` with subcommands (`tui`, `daemon`, `comment`, `done`, `move`, `card`).
2. **Access: overlay keybinding only** (no pinned workspace); `?` shows all keybinds.
3. **DB: `~/.local/share/herdr-board/board.db`** (XDG data; overridable via `BOARD_DB` for tests). Plugin config dir holds only config — DB survives plugin reinstall.
4. **Long-text editing: modal textarea + `Ctrl+E` → `$EDITOR`.**
5. boardd lifecycle: `board tui` auto-starts the daemon if absent; daemon outlives the overlay (runs continue with the board closed; `herdr notification show` covers "done while closed").

6. **One global board** (not per-space/per-repo): the space/path an agent must run in is configured on the card, never implied by board location.
7. **No MCP — CLI only.** Agents interact with the board exclusively through the `board` CLI.

## 10. The herdr-board skill

The repo ships a **skill** (`skill/SKILL.md`, installed to `~/.claude/skills/herdr-board/`) teaching agents the `board` CLI: command reference (`board card new/show/list`, `board comment`, `board move`, `board done --outcome ok|fail`), the card lifecycle, and the rules (always comment results *before* `board done`; `fail` means "this stage's goal was not met", not "I crashed").

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
| 3. Daemon integration | dispatch → run → done → auto-move, without burning tokens | **fake harness**: a stub script registered as harness `fake` that reads its prompt, sleeps, calls `board comment`/`board done --outcome ok`. Real boardd, real herdr spawn, zero Claude cost |
| 4. Full E2E | keyboard-drive the real TUI in a real pane | `herdr workspace create --label board-test` → open TUI pane (`pane split` + `pane run`) → drive with `herdr pane send-keys <pane> <keys>` / `send-text` → assert screen via `herdr pane read --source visible` + assert DB via `sqlite3 $BOARD_DB 'select …'` → `workspace close` |

Isolation rules for level 3–4: `BOARD_DB=/tmp/…` + dedicated daemon socket per test run so tests never touch the real board; prefer a separate `herdr --session board-test` (or headless `herdr server`) in CI so the user's session is untouched; inside an interactive dev loop, a throwaway workspace in the live session is fine.
