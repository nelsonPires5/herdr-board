# boardd socket protocol (v1) — CONTRACT

All components (TUI, CLI, tests) talk to boardd over this protocol. Serde types for every
request/response/event live in `board-core::protocol` — that module is the single source of
truth; this doc explains semantics.

## Transport

- Unix socket. Path resolution (both daemon and clients): `$BOARD_SOCKET` if set, else
  `~/.local/share/herdr-board/boardd.sock`.
- DB path resolution (daemon only): `$BOARD_DB` if set, else `~/.local/share/herdr-board/board.db`.
- Newline-delimited JSON (NDJSON), UTF-8. One JSON object per line, both directions.
- Request: `{"id":"<string>","method":"<name>","params":{...}}` (params may be omitted = `{}`).
- Response: `{"id":"<same>","result":<any>}` or `{"id":"<same>","error":{"code":<int>,"message":"..."}}`.
- Error codes: `1` bad request / unknown method, `2` not found, `3` invalid state
  (e.g. delete column with running card), `4` herdr unavailable, `5` internal.
- A connection may send `{"id":"...","method":"events.subscribe"}`; boardd replies
  `{"id":"...","result":{"subscribed":true}}` and then streams event objects
  (no `id` field) on that connection until it closes. A subscribed connection can still
  send further requests.

## Auto-start

`board tui` and every CLI subcommand try to connect; on failure they spawn
`board daemon` detached (double-fork / setsid, stdout+stderr → `~/.local/share/herdr-board/daemon.log`),
then retry with backoff for ~3s. Daemon takes an exclusive flock on `<db>.lock` — a second
daemon on the same DB must exit 0 silently (lost the race = someone else serves).

## Methods

### daemon
- `daemon.status` → `{version, db_path, herdr_connected: bool, active_runs: int, queued_runs: int}`
- `daemon.stop` → `{stopping:true}` (graceful: cancels nothing; running panes keep running, runs are re-adopted on next start via herdr pane liveness check)

### board / columns
One global board (`id=1`, created on first migration with a single `Todo` column, `trigger=manual`, position 0).

- `board.get` → `{board:{id,name}, columns:[Column…ordered], cards:[Card…ordered by column,position]}`
  (the TUI's one-shot fetch; it refetches on any event)
- `column.create {name, position?, system_prompt?, trigger?, on_success_column_id?, on_fail_column_id?, fresh_session?, harness_override?, model_override?, effort_override?, permission_override?, timeout_minutes?}` → `Column`
- `column.update {id, …any subset of the above}` → `Column` (name/trigger/etc.; unset a nullable by passing `null`)
- `column.reorder {id, position}` → `[Column…]`
- `column.delete {id, move_cards_to?}` → `{deleted:true}`; error 3 if it has cards and no `move_cards_to`, or if any card in it is `running|queued`
- `template.apply {name:"pipeline"}` → full column set (error 3 unless board has only the seed `Todo` column and no cards)

### cards
- `card.create {title, description?, column_id?(default Todo), harness?(default "claude"), model?, effort?, permission_mode?, space_kind?("workspace"|"cwd"|"worktree"), space_ref?, worktree_base?, position?}` → `Card`
  - creating directly into an `auto` column dispatches immediately (same as move)
- `card.update {id, …subset}` → `Card` (error 3 while `running|queued` for harness/space fields)
- `card.delete {id}` → `{deleted:true}` (error 3 while running; cancel first)
- `card.move {id, column_id, position?}` → `Card` — THE trigger: if target column `trigger=auto` and card idle/failed, a run is enqueued
- `card.get {id}` → `{card, comments:[…], runs:[…]}`
- `card.list {column_id?}` → `[Card…]`

### comments / runs
- `comment.add {card_id, body, author?}` → `Comment`. CLI `board comment` sets author
  `agent:<run_id>` when `$BOARD_RUN_ID` is set, else `user`.
- `run.done {card_id, outcome:"ok"|"fail", summary?}` → `{run, card}` — backend of `board done`.
  Closes the card's active run, posts a `system` comment, applies the column transition
  (`ok`→on_success, `fail`→on_fail; no target → card stays, status `idle`/`failed`).
  Error 2 if no active run.
- `run.cancel {card_id}` → `{run, card}` — kills the pane (herdr `pane.close`), outcome `cancelled`, card status `failed`, no transition.
- `run.retry {card_id}` → re-enqueue in current column (fresh run; claude gets `--fork-session` if a session exists).

## Events (streamed to subscribers)

Coarse by design — the TUI just refetches `board.get` on any of them; payload is for logs/toasts.

- `{"event":"board_changed","reason":"card_moved|card_created|card_updated|card_deleted|column_changed|comment_added|run_started|run_ended|run_blocked","card_id"?:N,"column_id"?:N}`
- `{"event":"run_ended","card_id":N,"run_id":N,"outcome":"ok|fail|cancelled|lost"}` (also emitted as board_changed)

## Dispatch semantics (column engine — lives in board-core, pure; daemon executes effects)

1. Card enters auto column → `runs` row `outcome=NULL,started_at=NULL` (queued), card status `queued`.
2. Queue key = `(space_kind, space_ref)`; one running card per key (FIFO); global semaphore default 3 (config `max_concurrent`).
3. Spawn (daemon, via `Spawner` trait):
   - resolve prompt: `description + "\n\n## Card comments\n" + last 20 comments` (skip section if none)
   - resolve settings: card value, overridden by column `*_override` when set
   - session: resume card.session_id unless column.fresh_session or none → mint UUID, store on card
   - herdr spawn: `agent.start {name:"board-card-<id>", workspace_id | cwd, env:{BOARD_CARD_ID,BOARD_RUN_ID,BOARD_SOCKET}, argv}`; `space_kind=worktree` → `worktree.create {cwd:space_ref, base:worktree_base, branch:"board/card-<id>"}` first, spawn with cwd=worktree path
   - card status `running`, store pane/workspace ids on run, emit `run_started`
4. Finish signals, priority order:
   - `run.done` from the agent (primary; semantics above)
   - herdr `pane_exited` while running → outcome `fail`, system comment "pane exited without board done", card status `failed`, **no** transition
   - herdr agent_status `idle` sustained > `idle_grace_seconds` (default 90) with no `run.done` → outcome `lost`, status `failed`, notification
   - `timeout_minutes` (column) exceeded → `pane.close`, outcome `fail`, apply on_fail
   - agent_status `blocked` → card status `blocked` + herdr notification (run stays active)
5. Every transition posts a `system` comment (e.g. "Plan ok in 4m12s → Execute").
6. Manual-trigger columns on entry: status `idle`, herdr notification if entered via auto-transition.

## Harness adapters (board-core)

- Builtin `claude`:
  `claude [--model M] [--effort E] [--permission-mode P] [--append-system-prompt SP] (--session-id UUID | --resume ID) [--fork-session] -- "PROMPT"`
  (prompt positional; interactive REPL in the pane).
- Config-defined harnesses (`~/.config/herdr-board/config.toml`):
  ```toml
  [harness.fake]
  argv = ["bash", "/path/to/fake-agent.sh"]   # receives BOARD_* env; prompt via $BOARD_PROMPT
  ```
  For custom harnesses the prompt/system prompt go in env `BOARD_PROMPT` / `BOARD_SYSTEM_PROMPT`
  (argv template supports `{model}`, `{effort}`, `{permission_mode}` placeholders, dropped if unset).
- `permission_mode=bypassPermissions` is refused unless the card explicitly sets it (never via column override).
