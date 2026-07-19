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

Boards are independent pipelines keyed by canonical path. `Global` is board `id=1` with
`scope_path:null`; old requests that omit `board_id` continue to target it.

- `board.open {scope_path}` → `BoardSnapshot`; idempotently gets/creates the path board, seeding a
  new board with exactly one manual `Todo` column.
- `board.list {}` → `{boards:[Board…]}`; `Global` first, then scoped boards ordered by full path.
- `board.get {board_id?}` → `{board:{id,name,scope_path}, columns:[Column…ordered], cards:[Card…]}`.
  Omitted `board_id` means `Global`. Cards include active and archived rows for local filtering.
- `column.create {name, board_id?, position?, system_prompt?, trigger?, on_success_column_id?, on_fail_column_id?, fresh_session?, harness_override?, model_override?, effort_override?, permission_override?, timeout_minutes?}` → `Column`; omitted `board_id` means `Global`.
- `column.update {id, …any subset of the above}` → `Column` (name/trigger/etc.; unset a nullable by passing `null`)
- `column.reorder {id, position}` → `[Column…]`
- `column.delete {id, move_cards_to?}` → `{deleted:true}`; destination must belong to the same board; error 3 if cards lack a destination or any card is `running|queued`.
- `template.apply {name:"pipeline", board_id?}` → the requested board's full column set (omitted = `Global`; error 3 unless it has only seed `Todo` and no cards).

The store enforces board boundaries: card create/move, column-delete destinations,
`on_success`/`on_fail`, templates, and automatic transitions cannot reference another board.
Scheduler adoption and watchers still scan runs across every board.

### cards
A card selects a **herdr session** (`session`, `null` = the daemon's default session) AND a **space** within it.
- `card.create {title, board_id?, description?, column_id?(default Todo), harness?(default "pi"), model?, effort?, permission_mode?, session?, space_kind?("workspace"|"new_workspace"), space_ref?, space_cwd?, position?}` → `Card`; omitted `board_id` means `Global`, and an explicit column must belong to that board.
  - Pi rejects a non-null `permission_mode` with error 1; Pi has no board-level tool permission mode.
  - `space_kind`:
    - `workspace` — an ALREADY-OPEN workspace in the session; `space_ref` = its workspace id (a case-insensitive label is also accepted at dispatch).
    - `new_workspace` — the daemon creates the workspace on first dispatch (label = `space_ref`, cwd = `space_cwd`), reusing an open workspace with that label if one exists. **Requires** non-empty `space_ref` and `space_cwd` on create (else error 1).
  - creating directly into an `auto` column dispatches immediately (same as move)
  - (v2 schema) the legacy `cwd`/`worktree` kinds and `worktree_base` are removed; worktree isolation is now the agent's job via prompt instructions, not a board concept. Existing DBs migrate `cwd`/`worktree` cards to `workspace` (best effort, `space_ref` kept).
- `card.update {id, …subset}` → `Card` (error 3 while `running|queued` for harness/`session`/space fields)
- `card.delete {id}` → `{deleted:true}` (error 3 while running; cancel first)
- `card.archive {id, archived:true|false}` → `Card` — archives or restores without deleting
  comments/runs. Archiving is refused while `queued|running|blocked|awaiting`; `done` cards can be
  archived. Archived cards must be restored before move/retry.
- `card.move {id, column_id, position?}` → `Card` — THE trigger: target must belong to the
  card's board; if it is `auto` and the card is `idle`, `failed`, or `done`, a run is enqueued.
  `awaiting` is not re-dispatched because its run remains open.
- `card.get {id}` → `{card, comments:[…], runs:[…]}`
- `card.list {board_id?, column_id?}` → `[Card…]`; omitted `board_id` means `Global`, and a column filter must belong to the requested board.

### comments / runs
- `comment.add {card_id, body, author?}` → `Comment`. CLI `board comment` sets author
  `agent:<run_id>` when `$BOARD_RUN_ID` is set, else `user`.
- `run.done {card_id, outcome:"ok"|"fail", summary?}` → `{run, card}` — backend of `board done`.
  Closes the card's active run, posts a `system` comment, applies the column transition
  (`ok`→on_success, `fail`→on_fail; no target → card stays, status `done`/`failed`).
  Also the confirm channel for an `awaiting` card (TUI `Enter` sends the same request).
  Error 2 if no active run.
- `run.cancel {card_id}` → `{run, card}` — kills the pane (herdr `pane.close`), outcome `cancelled`, card status `failed`, no transition.
- `run.retry {card_id}` → re-enqueue in current column (fresh run). Claude resumes with
  `--fork-session`; Pi uses `--fork <old-id> --session-id <new-id>` and persists the new id.
- `run.focus {card_id, origin_socket}` → `{run_id,pane_id}` — chooses the newest run with a
  recorded pane, resolves its Herdr session, and calls socket `pane.focus`. `origin_socket` and the
  target socket are canonicalized and must match; cross-session focus is error 3, unavailable/stale
  Herdr is error 4.

### harness / spaces
- `harness.capabilities {harness}` → `{harness, models:[{id, efforts:[…]}], model_freeform: bool, default_efforts:[…], permission_modes:[…]}`. `default_efforts` is serde-defaulted for backward-compatible clients and applies when model is omitted/free-form; a known model's own efforts remain authoritative.
  - Built-in `pi`: static `models:[]`, `model_freeform:true`, `default_efforts:["off","minimal","low","medium","high","xhigh","max"]`, `permission_modes:[]`. Pi's catalog is user/provider-specific, so the daemon overlays a **live** catalog when it can resolve the pi agent dir (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`): it reads `auth.json` for the authenticated providers, then `models-store.json` and keeps only those providers' models as `provider/model` ids with per-model efforts from each model's `thinkingLevelMap` (the full thinking ladder when a model has none). This reproduces `pi --list-models` (provider-auth scoped) with richer per-model effort data. If the files are missing/unreadable it falls back to shelling out to `pi --list-models`, and finally to the static free-form catalog. `model_freeform` stays `true`, so arbitrary model strings remain valid. Tests leave the agent dir unset, so the catalog stays the static `models:[]`.
  - Built-in `claude` (CLI 2.1.209): models `fable`/`opus`/`sonnet`/`haiku`, each with `low|medium|high|xhigh|max`; the same levels are `default_efforts`; `model_freeform:true`; permissions are `["acceptEdits","auto","bypassPermissions","manual","dontAsk","plan"]`.
  - config-defined harnesses report `model_freeform:true` and the declared `models`/`efforts`/`permission_modes`; declared efforts also populate `default_efforts`.
  - error 2 (not found) for an unknown harness, listing the known harnesses.
- `harness.list` (no params) → `{harnesses:[…]}` — every harness the daemon knows about: the built-ins `pi`/`claude` in their default order (pi first), then every config-defined `[harness.NAME]` sorted, de-duplicated. This is the single source for BOTH the card `harness` and column `harness_override` selects in the TUI, so every harness menu shares one list in one (default-first) order.
- `space.list {session?}` → `{spaces:[{id, label}]}` — workspaces in the given session (`null` = default), filled from that session's herdr `workspace.list`. Unknown/not-running session → error 4 listing the known sessions.
- `session.list` (no params) → `{sessions:[{name, default: bool, running: bool}]}` — the daemon shells out to `herdr session list --json` (session enumeration is not in the herdr socket API; a session only knows itself). Binary resolved via `$HERDR_BIN_PATH`, else `herdr` on `$PATH`. Error 4 if herdr is unavailable / the CLI fails.

## Card statuses & signals

`idle · queued · running · blocked · awaiting · done · failed`

| Status | Meaning |
|---|---|
| `idle` | At rest in a column; no active run. |
| `queued` | Enqueued for dispatch into an auto column. |
| `running` | A run is active and the agent is working. |
| `blocked` | The agent/integration reported blocked; the run stays active. |
| `awaiting` | The agent appears finished (or went idle) **without** `board done`. The run stays OPEN, the column timeout is paused, and the card never fails on its own — it waits for human review. |
| `done` | Completion confirmed: `run.done ok` (or the TUI confirm, same channel) with no `on_success` target column. Final visual state; moving the card into an auto column re-dispatches it like `idle`/`failed`. |
| `failed` | The run ended `fail`/`cancelled`, the pane exited, or the column timeout fired. |

`awaiting` carries an `awaiting_reason` (`cards.awaiting_reason`, set on entry,
cleared to NULL on exit): `agent_done` (herdr reported `agent_status=done`) or
`idle_expired` (`idle` sustained past `idle_grace_seconds`).

**Golden rule:** herdr's agent status is a HINT. `board done` (`run.done`) is the
only terminal success truth — no herdr signal ever finalizes a run with `ok`.

### Signal → state machine

Watchers only OBSERVE: herdr pane statuses and idle expiry are translated into
signals; the pure engine (`board_core::engine::decide_signal`) is the single
decider, and the daemon applies its decision in one place.

| Signal | Resulting card state |
|---|---|
| herdr `working` | `running`; clears `blocked`/`awaiting` (+reason). From `awaiting` this is the review loop: feedback typed into the pane wakes the agent. |
| herdr `blocked` | `blocked`; run stays active. |
| herdr `done` (run active, no `board done`) | `awaiting` + `agent_done` (immediate, no grace) + notification. On an already-`awaiting` card it refreshes the reason to `agent_done` without re-notifying. |
| `idle` past `idle_grace_seconds` (no `board done`) | `awaiting` + `idle_expired` + notification. On an already-`awaiting` card it's a no-op (keeps the more specific reason). |
| herdr `unknown`, or any signal on a non-live card | ignored. |
| `pane_exited` without `board done` | run `fail`, card `failed`, **no** transition (unchanged). |
| column `timeout_minutes` exceeded | **paused while `awaiting`** (the deadline shifts forward by the review span on exit); otherwise run `fail` + `on_fail`. |
| `run.done ok` | `on_success` target → move; no target → `done`. |
| `run.done fail` | `on_fail` target → move; no target → `failed`. |
| `run.cancel` | outcome `cancelled`, card `failed`, no transition. |

Only `running`/`blocked`/`awaiting` cards accept signals (a run may be active);
anything else is stale and ignored.

Exits from `awaiting`: herdr `working` → `running`; `board done` / TUI confirm →
finalize ok (`done` or column move); `board cancel` → cancelled.

Note: the run outcome `lost` is retained in the schema and wire enums for
backward compatibility but is **no longer produced** — the idle-expiry path now
parks the card in `awaiting` instead of failing the run.

## Events (streamed to subscribers)

Coarse by design — the TUI refetches only its selected `board.get {board_id}` on any event; payload is for logs/toasts.

- `{"event":"board_changed","reason":"card_moved|card_created|card_updated|card_deleted|card_archived|column_changed|comment_added|run_started|run_ended|run_blocked","card_id"?:N,"column_id"?:N}`
- `{"event":"run_ended","card_id":N,"run_id":N,"outcome":"ok|fail|cancelled|lost"}` (also emitted as board_changed; `lost` is legacy — no longer produced, see Card statuses)

## Dispatch semantics (column engine — lives in board-core, pure; daemon executes effects)

1. Card enters auto column → `runs` row `outcome=NULL,started_at=NULL` (queued), card status `queued`.
2. Queue key = `(session, space_kind, space_ref)`; one running card per key (FIFO); global semaphore default 3 (config `max_concurrent`). Session is part of the key so the same label/ref in two herdr sessions are distinct spaces.
3. Spawn (daemon, via `Spawner` trait):
   - resolve prompt: `description + "\n\n## Card comments\n" + last 20 comments` (skip section if none)
   - resolve settings: card value, overridden by column `*_override` when set
   - resolve session: card `session` (null = default) → herdr socket via the session registry; an unknown/not-running session fails the run with a clear error listing known sessions. The per-session herdr client (workspace resolve/create, spawn, kill, liveness) is built from that socket.
   - harness session: resume `card.session_id` unless `column.fresh_session` or none. Pi mint/resume use exact `--session-id`; Pi retry forks old → a newly minted target id. Claude retains its existing mint/`--resume`/`--fork-session` behavior. Existing cards keep their stored harness/session.
   - resolve space within the session: `workspace` → resolve `space_ref` by id or case-insensitive label; `new_workspace` → reuse an open workspace whose label matches `space_ref`, else `workspace.create {label:space_ref, cwd:space_cwd, focus:false}`. Workspace cwd comes from the workspace's pane snapshot (agent.start does not inherit it).
   - herdr spawn: `agent.start {name:"card-<id>-<column-slug>", workspace_id, tab_id?, split?, env:{BOARD_CARD_ID,BOARD_RUN_ID,BOARD_SOCKET}, argv}` on the session socket
   - pane name is `card-<id>-<column-slug>` (e.g. `card-14-execute`); on herdr `agent_name_taken` retry once with the run-scoped fallback `card-<id>-<column-slug>-r<run>`
   - placement: the agent lands in a `kanban` tab of the workspace — find-or-create the tab (first tab labeled `kanban`, lowest `number` on ties). A freshly-created tab is filled unsplit, then its leftover shell pane is closed; an existing tab splits its largest pane (`Right` if that pane's cell width ≥ 2× its height, else `Down`, to keep the mesh ≈ square). `agent_placement_not_found` (tab raced away) redoes find-or-create once.
   - card status `running`, store pane/workspace ids + `session` on run, emit `run_started`
4. Finish signals, priority order (the full signal→state mapping is under
   [Card statuses & signals](#card-statuses--signals); the engine is the single decider):
   - `run.done` from the agent (primary; semantics above)
   - herdr `pane_exited` while running → outcome `fail`, system comment "pane exited without board done", card status `failed`, **no** transition
   - herdr agent_status `done` with no `run.done` → card `awaiting` (reason `agent_done`), run stays OPEN, notification
   - herdr agent_status `idle` sustained > `idle_grace_seconds` (default 90) with no `run.done` → card `awaiting` (reason `idle_expired`), run stays OPEN, notification
   - `timeout_minutes` (column) exceeded → `pane.close`, outcome `fail`, apply on_fail; **paused while the card is `awaiting`**
   - agent_status `working` → card status `running`, clearing blocked/awaiting (idle tracking is disarmed while awaiting)
   - agent_status `blocked` → card status `blocked`, board change + Herdr notification (run stays active)
5. Every transition posts a `system` comment (e.g. "Plan ok in 4m12s → Execute").
6. Manual-trigger columns on entry: status `idle`, herdr notification if entered via auto-transition.

## Harness adapters (board-core)

- Built-in `pi` (the default for new cards):
  `pi [--model provider/model] [--thinking off|minimal|low|medium|high|xhigh|max] --append-system-prompt SP (--session-id ID | --fork OLD --session-id NEW) "Card task:\nPROMPT"`
  - omitted model/thinking means Pi uses its own configured defaults;
  - the prompt is a normal positional argument with a safe non-flag prefix, never Claude's `--` delimiter;
  - no permission, approval, or `--allowedTools` flag is added. Pi project trust is separate from tool permission.
- Built-in `claude`:
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
- All built-ins receive the column prompt plus the mandatory board-protocol trailer. Config-defined
  harnesses alone receive reconstructed `BOARD_PROMPT`/`BOARD_SYSTEM_PROMPT` env.

Pi lifecycle status comes from Herdr's official Pi integration and the existing event watcher; there
is no Pi-specific watcher. Without `herdr integration install pi`, explicit `board done`, spawn
failure, timeout, and pane exit still work, but working/blocked/done detection is
unavailable and the idle→`awaiting` watchdog does not arm while status remains `unknown`
(see [Card statuses & signals](#card-statuses--signals)).
