# boardd socket protocol (v1) — CONTRACT

All components (TUI, CLI, tests) talk to boardd over this **public board protocol v1**. Serde types
for every request/response/event live in `board-core::protocol` — that module is the single source
of truth; this doc explains semantics. The board protocol version is independent of Herdr's socket
protocol version.

## Transport

- Unix socket. Path resolution (both daemon and clients): `$BOARD_SOCKET` if set, else
  `~/.local/share/herdr-board/boardd.sock`.
- DB path resolution (daemon only): `$BOARD_DB` if set, else `~/.local/share/herdr-board/board.db`.
- Log directory resolution (daemon only): `$BOARD_LOG_DIR` if set, else `<data>/logs`.
- Newline-delimited JSON (NDJSON), UTF-8. One JSON object per line, both directions.
- Request: `{"id":"<string>","method":"<name>","params":{...}}` (params may be omitted = `{}`).
- Response: `{"id":"<same>","result":<any>}` or `{"id":"<same>","error":{"code":<int>,"message":"..."}}`.
  Error objects may add `kind` and `details`; old clients may ignore those members.
- Error codes: `1` bad request / unknown method, `2` not found, `3` invalid state
  (e.g. delete column with running card), `4` herdr unavailable, `5` internal. The CLI preserves
  this envelope for `--json` errors on stderr and emits no JSON on stdout. A request handler task
  that **panics** (or is cancelled) still answers, with `5`: dropping the request would leave the
  client waiting forever, and killing the connection would take every other in-flight request on it
  down as well.
- **Protocol codes are not exit codes.** `board` maps `1..=5` straight onto its process exit status
  so scripts can branch on `$?`; any other protocol code exits `70` (`EX_SOFTWARE`) because an exit
  status is taken modulo 256 and `256` would silently read as success — the `--json` envelope still
  carries the exact code the daemon sent. Errors raised by the CLI itself, before or instead of an
  RPC (usage/parse, a declined confirmation, a bad enum value, a column reference that resolves to
  nothing client-side, a missing `$BOARD_CARD_ID`), are **not** protocol errors: they exit `64`
  (`EX_USAGE`) and their envelope is `{"code":64,"kind":"cli"}`. `64` is deliberately outside
  `1..=5`, so a CLI-local failure can no longer be mistaken for the daemon's "not found".
- A connection may send `{"id":"...","method":"events.subscribe"}`; boardd replies
  `{"id":"...","result":{"subscribed":true}}` and then streams event objects
  (no `id` field) on that connection until it closes. A subscribed connection can still
  send further requests. Each connection has a bounded outbound queue: consecutive
  `board_changed` events may coalesce to the latest coarse refresh while preserving response and
  terminal-event order. Responses are never silently dropped. If a slow subscriber cannot accept
  a non-coalescible event (including a terminal `run_ended`), boardd disconnects it; clients must
  reconnect, resubscribe, and refetch board state. Broadcast lag likewise becomes a coarse
  `board_changed` refresh rather than silently losing the refresh signal.

## Herdr compatibility gate

boardd supports **exactly Herdr 0.8.0 / protocol 19**. The board protocol remains v1 and the
SQLite schema remains v13; neither version is the upstream Herdr socket contract. Because the
daemon opens a fresh Herdr request connection per operation, the compatibility probe is repeated
at each operation boundary rather than treated as a one-time startup check:

- `daemon.status` re-pings its configured socket and reports `herdr_connected: true` only for the
  exact supported version and protocol.
- The event supervisor gates a request connection before opening its persistent `events.subscribe`
  stream. The subscription acknowledgement must be the typed `subscription_started` success
  response, and it completes before the first reconciliation snapshot. An incompatible socket
  receives neither a subscription
  nor snapshot work.
- The detached notification effect gates immediately before `notification.show`; an incompatible
  but pingable Herdr is not sent even a cosmetic notification.
- Dispatch and workspace discovery gate before `workspace.list`, `workspace.create`, or a live
  workspace-cwd snapshot. The spawner repeats the gate as its first call before tab/pane placement,
  managed-agent launch, or a configured-harness runner. `space.list` and the read-only space
  preflight are checked the same way. The `herdr session list --json` registry enumeration is a
  separate CLI discovery step, not a socket call; once it selects a socket, socket operations are
  gated.
- New pane operations are checked before `pane.get`/`pane.focus` for `run.focus`, `pane.rename` for
  `pane.set_title`, and `pane.list`/`pane.layout`/`pane.split`/`pane.rename`, agent calls, and the
  configured runner used by placement and rescue.

There is one deliberate exception: cleanup and liveness for panes already owned by a daemon run.
The spawner's `kill`/`pane.close` and `is_alive`/snapshot paths retain an ungated connection so a
Herdr upgrade or incompatibility cannot prevent the daemon from closing or observing its own
handle. This exception does not authorize new discovery, placement, focus, notification, or launch;
supervisor reconciliation and caller-visible focus/rescue operations remain checked. A mismatch on
a checked dispatch path fails the run before workspace mutation.

## Auto-start

`board tui` and every CLI subcommand try to connect; on failure they spawn one detached
`board daemon` child (pre-subscriber stderr → private, truncated `logs/bootstrap.log`; structured
runtime diagnostics → daily `logs/daemon.YYYY-MM-DD.ndjson`), then retry with
backoff for ~3s. The child owns a new process group led by its exact PID; there is deliberately no
double-fork or `setsid`, so diagnostics and the safe harness retain an unambiguous owner token.
Lifecycle control uses the daemon socket (`daemon.stop`), never a broad process-group kill. Daemon
takes an exclusive flock on `<db>.lock` — a second daemon on the same DB must exit 0 silently
(lost the race = someone else serves).

## Client boundary

`board-core::client::BoardClient` is the shared typed client boundary used by the CLI and TUI.
Its wrappers encode the v1 method parameters and decode result DTOs; the CLI and TUI do not build
method strings or inspect raw result values for these operations. `UnixClient` keeps the blocking
NDJSON `call(method, params)` primitive internally for transport implementations and compatibility
with lightweight recording/fake clients. Production clients perform no SQLite I/O; only boardd owns
and mutates the database.

The typed catalog/action surface includes `harness.capabilities`, `harness.list`,
`space.list`, `session.list`, `run.cancel`, `run.retry`, and `pane.set_title`, in addition to the
existing board, column, card, comment, and run wrappers. `space.list(None)` deliberately serializes
as `{}` while a named session serializes as `{ "session": "..." }`, preserving the v1 wire contract.

The daemon owns **all** Herdr interaction (`AGENTS.md`): no client opens a Herdr socket or runs the
`herdr` binary for itself. `pane.set_title` exists for exactly that reason — it is how the TUI
plugin pane relabels its own border.

## Request diagnostics

boardd emits one metadata-only completion diagnostic for every parsed request and subscription
acknowledgement: method, daemon-generated numeric `(conn, req_id)` correlation, duration, outcome,
and protocol error code when failed. The arbitrary wire request ID is preserved in its response but
never becomes diagnostic metadata; unknown method strings are recorded as `<unknown>`. Malformed
input receives bounded parse metadata. Request parameters and response results are never attached
to diagnostics.

## Methods

### daemon
- `daemon.status` → `{version, db_path, herdr_connected: bool, active_runs: int, queued_runs: int}`
- `daemon.stop` → `{stopping:true}` (graceful: cancels nothing; running panes keep running, runs are re-adopted on next start via herdr pane liveness check). The CLI stop command treats this as an acknowledgement, not completion: it returns success only after the listener disappears. RPC errors, a listener that remains live past the bounded wait, and socket-path replacement are errors and preserve the socket. If the initial connect fails, a stale socket is removed only after a fresh failed connect and matching device/inode/socket-type identity checks; a replacement path is preserved. `board daemon stop --json` reports that outcome as `{"stopped": bool, "was_running": bool}` — `was_running:false` is the "nothing to stop" success, distinguishable from a real stop without parsing text. The human line is unchanged (`boardd stopped` / `boardd not running`).

### board / columns

Boards are independent pipelines keyed by canonical path. `Global` is board `id=1` with
`scope_path:null`; old requests that omit `board_id` continue to target it.

- `board.open {scope_path}` → `BoardSnapshot`; idempotently gets/creates the path board, seeding a
  new board with exactly one manual `Todo` column.
- `board.list {}` → `{boards:[Board…]}`; `Global` first, then scoped boards ordered by full path.
- `board.get {board_id?}` → `{board:{id,name,scope_path}, columns:[Column…ordered], cards:[Card…], active_runs:[{card_id,started_at}…]}`.
  Omitted `board_id` means `Global`. `board.open` returns the same snapshot shape. Both `board.get`
  and `board.open` include **all active and archived cards** in `cards`, so the TUI can filter
  locally and templates can evaluate the complete board state. `active_runs` is additive in
  protocol v1 and contains only started, open runs whose cards belong
  to the requested board; queued, ended, and other-board runs are omitted. Older clients may omit
  the field when decoding a snapshot, which is treated as an empty list.
- `column.create {name, board_id?, position?, system_prompt?, trigger?, on_success_column_id?, on_fail_column_id?, fresh_session?, harness_override?, model_override?, effort_override?, permission_override?, timeout_minutes?}` → `Column`; omitted `board_id` means `Global`.
- `column.update {id, …any subset of the above}` → `Column` (name/trigger/etc.; nullable update fields use the tri-state encoding below)
- `column.reorder {id, position}` → `[Column…]`
- `column.delete {id, move_cards_to?}` → `{deleted:true}`; destination must belong to the same board; error 3 if cards lack a destination or any card has an open run (`queued|running|blocked|awaiting`; `done` is not open).
- `template.apply {name:"pipeline", board_id?}` → the requested board's full column set (omitted = `Global`; error 3 unless it has only seed `Todo` and no cards).

The store enforces board boundaries: card create, column-delete destinations,
`on_success`/`on_fail`, templates, and automatic transitions cannot reference another board;
`card.move` with `board_id` is the single intentional exception (a validated transfer).
Scheduler adoption and watchers still scan runs across every board.

### Partial update fields (v1)

Nullable fields in `column.update` and `card.update` distinguish three wire states:

| JSON member | Meaning |
|---|---|
| omitted | leave the stored value unchanged |
| present with `null` | clear the stored value |
| present with a value | replace the stored value |

For columns this applies to `system_prompt`, `on_success_column_id`, `on_fail_column_id`,
`harness_override`, `model_override`, `effort_override`, `permission_override`, and
`timeout_minutes`. For cards it applies to `model`, `effort`, `permission_mode`, `session`,
`space_ref`, and `space_cwd`. The remaining partial-update fields retain their existing
non-null/optional semantics; create DTOs are unchanged. A TUI edit submits the current value
or an explicit `null` when the user clears a nullable field.

### Validation and effective settings

Card and column mutations are validated against the complete merged row before
SQLite is updated or a `BoardChanged` event is emitted. Omitted update members
remain unchanged while `null` clears them, so validation applies to the
post-merge state (including `new_workspace` requiring both a non-empty
`space_ref` and `space_cwd`). Unknown harnesses and unsupported model, effort,
or permission combinations return error 1 without a partial update or event.

A column may provide model/effort/permission values without a harness override;
they are checked against the entering card at enqueue time. A column
`bypassPermissions` override is always refused, while an explicit card
`bypassPermissions` value is allowed for Claude. Pi has no permission modes.
The daemon repeats validation against the effective card+column settings at the
enqueue boundary, including for legacy rows.

### cards
A card selects a **herdr session** (`session`, `null` = the daemon's default session) AND a **space** within it.
- `card.create {title, board_id?, description?, column_id?(default Todo), harness?(default "pi"), model?, effort?, permission_mode?, session?, space_kind?("workspace"|"new_workspace"), space_ref?, space_cwd?, position?}` → `Card`; omitted `board_id` means `Global`, and an explicit column must belong to that board.
  - Pi rejects a non-null `permission_mode` with error 1; Pi has no board-level tool permission mode.
  - `space_kind`:
    - `workspace` — an ALREADY-OPEN workspace in the session; `space_ref` = its workspace id (a case-insensitive label is also accepted at dispatch). An optional non-empty `space_cwd` is an explicit launch-directory override. Without it, all non-empty cwd values in the workspace's live panes must agree; heterogeneous values fail dispatch with the pane/cwd candidates instead of choosing by snapshot order.
    - `new_workspace` — the daemon creates the workspace on first dispatch (label = `space_ref`, cwd = `space_cwd`), reusing an open workspace with that label if one exists. **Requires** non-empty `space_ref` and `space_cwd` on create (else error 1).
    - The wire vocabulary is exactly `"workspace"` and `"new_workspace"`: `SpaceKind` is serde
      `snake_case`, so a hyphenated `"new-workspace"` on the socket is still rejected (error 1,
      unknown variant). The hyphen is a **CLI spelling** — `SpaceKind::parse_str` accepts
      `new-workspace` as an alias so `board card create --space-kind new-workspace` resolves through
      board-core instead of a hand-rolled match in the CLI, and the CLI then emits the canonical
      underscored form. `parse_str` is also what reads the value back out of SQLite, where only the
      canonical form is ever stored.
  - creating directly into an `auto` column dispatches immediately (same as move)
  - In the legacy v1→v2 migration, `cwd`/`worktree` kinds and `worktree_base` were removed;
    current schema v13 treats worktree isolation as the agent's job via prompt instructions, not a
    board concept. Existing databases migrate those cards to `workspace` (best effort,
    `space_ref` kept).
- `card.update {id, …subset}` → `Card`; nullable update fields use the tri-state encoding below. Harness/model/effort/permission/session/space fields are refused while the card has an open run (`queued|running|blocked|awaiting`). Title/description remain editable; `done` is not open.
- `card.delete {id}` → `{deleted:true}`; refused while the card has an open run (`queued|running|blocked|awaiting`; cancel first). `done` is not open.
- `card.duplicate {id}` → `Card` — creates a fresh idle copy inserted **immediately below** the original, shifting the following cards (the column stays compacted). The copy inherits the full run configuration — title with the ` (copy)` suffix, description, harness, model, effort, permission mode, session, and space settings — but none of the execution state: status `idle`, no `awaiting_reason`, no `session_id`, no runs/comments/history, no archive flag, and fresh timestamps. The original is never modified, the insert + renumber is one transaction, and — unlike `card.create` — duplication **never enqueues a run**, even in an `auto` column (the copy stays idle until moved or run). The normal `CardCreated` event is emitted for the copy.
- `card.archive {id, archived:true|false}` → `Card` — archives or restores without deleting
  comments/runs. Archiving is refused while the card has an open run (`queued|running|blocked|awaiting`); `done` cards can be archived. Archived cards must be restored before move/retry.
- `card.move {id, column_id, board_id?, position?}` → `Card` — THE trigger: target must belong to the
  card's board; if it is `auto` and the card is `idle`, `failed`, or `done`, a run is enqueued.
  `awaiting` is not re-dispatched because its run remains open. An optional `board_id` declares a
  **cross-board transfer**: when present and different from the card's board, the card's
  `board_id`/`column_id` are moved atomically (both columns recompacted) after a blocking sanity
  check (merged effective capabilities + session resolve) — incompatible settings/sessions abort
  the move with an explicit error. Omitted (or equal to the card's board) keeps the historical
  intra-board move. **Same-column reorder:** when `column_id` equals the card's current column,
  the move is a pure reorder — `position` (zero-based, clamped, appended when omitted) places the
  card inside its column and every card's position is recompacted contiguously. A same-column
  reorder never enqueues, never changes status (open runs included), and never triggers the
  column's automatic dispatch, even on an `auto` column with a dispatchable card.
- `card.get {id}` → `{card, comments:[…], runs:[…]}`. Run objects deliberately omit the internal
  `system_prompt_snapshot` field and its contents; missing snapshot input deserializes as legacy
  `null`, but the field is never serialized onto the board wire. Schema v7 writes this nullable
  snapshot only for new runs; legacy `NULL` rows are not backfilled and retain their historical
  launch behavior.
- Every `Card` served by a read/card op carries `labels: {session, effort, permission, model}` —
  **ready display strings stamped by the daemon** (clients render them verbatim; they are never
  round-tripped). `labels.session` is the resolved default-session name (the herdr session
  matching the daemon's bound socket, normally named `default`), or `default session` when the
  card's `session` is unset and nothing resolves; `labels.effort` / `labels.permission` /
  `labels.model` are the value, or `default effort` / `default permission` / `default model` when
  the card has no override. Wire fields keep their `None`-means-default semantics unchanged.
- `card.list {board_id?, column_id?, visibility?}` → `[Card…]`; omitted `board_id` means `Global`,
  and a column filter must belong to the requested board. `visibility` defaults to `"active"` and
  accepts `"active"`, `"all"`, or `"archived"`.

### comments / runs

- `comment.add {card_id, body, author?, actor_run_id?, actor_pane_id?}` → `Comment`. CLI `board
  comment` and `board card comment add` set `author` to `agent:<BOARD_RUN_ID>`, send `actor_run_id`
  when `$BOARD_RUN_ID` is set, and forward `$HERDR_PANE_ID` as `actor_pane_id` when present;
  otherwise the author defaults to `user`. Ordinarily the actor run is checked against the comment
  author, card, and open run. A managed same-conversation resume keeps one process/pane, whose
  immutable `BOARD_RUN_ID` names its first stage: when `actor_pane_id` exactly matches the card's
  current open run pane, that pane is the credential and the comment is attributed to the current
  run. A missing/different pane id retains the strict actor-run check.
- `comment.get {id}` → `CommentRecord` (`id,card_id,author,body,created_at,deleted_at`). It returns
  the current row even after a soft delete.
- `comment.update {id,body,actor_run_id?}` → `CommentRecord`. It preserves the original author and
  appends an immutable audit snapshot. An agent actor may update only its own comment from its exact
  open run; a human actor may update non-system comments.
- `comment.delete {id,actor_run_id?}` → `{deleted:true}`. This is a soft delete: the current row and
  audit history remain, while ordinary card detail and future run prompts omit the comment. A deleted
  comment cannot be edited or deleted again.
- `comment.history {id}` → `[CommentHistory…]` ordered from creation through the latest edit. Every
  insert creates the initial snapshot; edits append a snapshot; deletion marks the final snapshot's
  `deleted_at` without changing its body. System comments are immutable at the database boundary and
  cannot be edited or deleted by any actor.
- `run.done {card_id, outcome:"ok"|"fail", summary?, run_id?, actor_pane_id?}` → `{run, card}` —
  backend of `board done`. `run_id` is optional for compatibility: manual and TUI callers may omit
  it, and an omitted id completes the current active run. When supplied, it ordinarily must exactly
  match the current active run, so a stale child cannot complete a replacement run. The CLI forwards
  `BOARD_RUN_ID` and `$HERDR_PANE_ID` when present. A managed same-conversation resume keeps one
  process/pane, whose immutable `BOARD_RUN_ID` names its first stage: when `actor_pane_id` exactly
  matches the current open run's recorded pane, the pane is the actor credential and `run.done`
  applies to that current run. This intentionally makes all commands from that still-live pane act
  as its current stage; a missing/different pane id retains exact run-id rejection. It closes the
  active run, posts a `system` comment, and applies the column transition (`ok`→on_success,
  `fail`→on_fail; no target → card stays, status `done`/`failed`). It is also the confirm channel for
  an `awaiting` card (TUI `Enter` and `card run confirm` send the same request). The only queued
  exception is a configured harness: its `board done` must provide the exact queued run id and may
  arrive before runner registration. A queued built-in (pi/claude/codex/opencode) run is rejected because
  managed completion requires a registered pane. A mismatched id/pane, missing id for the queued
  exception, or otherwise ineligible run returns an error.
- `run.cancel {card_id}` → `{run, card}` — kills the pane (herdr `pane.close`), outcome `cancelled`, card status `failed`, no transition.
- `run.retry {card_id}` → `{run, card}` — re-enqueue in the current column as a fresh run. Claude
  resumes with `--fork-session`; Pi uses `--fork <old-id> --session-id <new-id>` and persists it;
  codex retries with the `fork <id>` subcommand and persists the new thread id the integration
  reports; opencode retries with `-s <id> --fork` and persists the new session id the integration
  reports (the captured id supersedes the recorded source id atomically; a fork whose new id was
  never captured keeps the source id instead of wiping it).
- `run.focus {card_id, run_id, origin_socket}` →
  `{action, recorded_pane_id?, run_id, card_id, column_id, harness, session, session_id, pane_id}`
  — focuses **one exact run's** pane, reopening it if necessary. `run_id` is required: the daemon
  never implicitly picks the latest run, so a caller that wants "newest run with a pane" resolves
  that itself from the card's `runs[]`. The run must belong to `card_id`; an unknown card, an
  unknown run, and a run owned by another card are error 2 (the message names the requested card
  and run and never discloses another card's identity). The result echoes the focused run's full
  identity, where `session` is the **herdr session name** (`None` = default session) and
  `session_id` is the **harness conversation id** — the two are distinct and never interchangeable.
  `pane_id` is always the pane that now has focus; `recorded_pane_id` is what the run row stores
  (absent when it stores none), which on a rescue is the **dead** pane, kept for diagnostics.

  Liveness is checked explicitly with one targeted `pane.get` before focusing, so a stale
  `herdr_pane_id` never produces an opaque `pane.focus` failure. `action` says what happened:

  | `action` | meaning |
  |---|---|
  | `focused_recorded_pane` | the recorded pane was alive and got focus (the default for payloads serialized before this field existed) |
  | `focused_rescued_pane` | the recorded pane is gone, but a pane from an **earlier rescue of this run** was still alive in the card tab, so that one got focus; nothing was created |
  | `rescued` | the recorded pane is gone; a **new** pane was created in the `card-<id>` tab and the harness conversation was resumed in it |

  **Rescue.** When the run has no live pane (nothing recorded, or the recorded pane no longer
  exists), the daemon reopens the run by resuming its harness conversation in a fresh pane. It never
  reuses or revives the dead pane id and never falls back to another run's pane. The rescue
  **writes nothing to the database**: no new `runs` row, no updated `herdr_pane_id`, no cleared
  `ended_at`/`outcome`; the historical row stays immutable and `SCHEMA_VERSION` is unaffected. The
  rescued pane is therefore *ephemeral and unmanaged* — it has no run row, so the daemon does not
  own, watch, or time it out (see `docs/design.md` → Limitations). Its launch is derived from the
  run's persisted `launch_spec_json`, so model/effort/permission mode/env match the original
  execution, with the initial prompt removed and `BOARD_PROMPT` stripped so the card task is never
  re-sent (resuming continues the conversation, it does not re-run the work). A persisted *legacy
  all-in-one* command line (one containing `--`, i.e. with the task embedded positionally) cannot be
  re-threaded onto a resume without re-sending that task, so it is refused (error 3) rather than
  rewritten.

  **Placement workspace.** The rescue places in the run's recorded workspace while it is still
  usable, probing it with the same test placement uses (a live pane must still carry a cwd). When
  the recorded workspace is gone — the user closed it, or it lost its last pane — the rescue
  resolves a replacement from the card's **current** space config
  (`space_kind`/`space_ref`/`space_cwd`), the exact resolution dispatch uses, inside the run's own
  Herdr session: a card with a `new_workspace` space gets a fresh workspace created with its label
  and cwd (the initial tab is adopted as the card tab, same as a first dispatch); a `workspace`-kind
  ref to an open workspace resolves to that one. The rescue never picks a workspace on its own: if
  the recorded workspace is gone and the card's current config cannot supply a replacement (a
  `workspace`-kind ref to the closed workspace, or a `new_workspace` space missing its label/cwd),
  the error names both the dead workspace and the config failure, and nothing is created. A
  replacement created by the rescue is reused on later `run.focus` calls by the same resolution
  (label find-or-create), so repeated presses neither duplicate the workspace nor the pane.

  **Environment of a rescued pane.** Pane-first placement establishes the environment on the
  `pane.split` that creates the pane. A rescued pane receives the persisted run
  environment plus `BOARD_CARD_ID`, `BOARD_SOCKET`, `BOARD_BIN`, `BOARD_RESCUE=1`,
  `BOARD_RESUME_SESSION_ID=<conversation id>`, and `BOARD_RESCUED_RUN_ID=<run id>`.
  **`BOARD_RUN_ID` is deliberately cleared to empty** (the CLI treats empty as unset). It is not a label but the *actor credential*:
  `board comment` authenticates as `agent:$BOARD_RUN_ID`, `board done` forwards it as the run to
  finalize, and the configured-harness wrapper hands it to `run.pane_exited`. A rescued pane belongs
  to no run and the historical row is immutable, so granting it the closed run's id would either be
  rejected anyway (`agent run N is no longer open`) or — for a still-open run whose pane died — let
  an unwatched pane finalize that run while racing the liveness watcher. Withholding it fails closed:
  `board comment` degrades to an ordinary human comment on the card, `board done` answers "no active
  run" (or is rejected on run-id mismatch), and the configured wrapper's `__pane-exited` call fails
  argument parsing and is swallowed. `BOARD_RESCUED_RUN_ID` carries the id for humans and fixtures
  and is consumed by no board command.

  Resume support is an **explicit per-harness capability** (`harness.capabilities.resume`:
  `by_conversation_id` | `unsupported`), never an assumption about flag syntax. `pi`, `claude`,
  `codex`, and `opencode` declare it; a `[harness.NAME]` harness declares it with `resume = true`
  and otherwise fails closed. Note that a recorded `session_id` is **not** evidence that a harness
  can resume it: enqueue mints a uuid and persists it even for a configured harness that never
  receives one, so the capability gate is the only sound signal (verified live in
  `e2e/27-rescue-dead-pane.sh`). Self-minting harnesses (codex and opencode) add one more
  fail-closed rule: a Mint persists `session_id = NULL` until the integration-reported id is
  captured, so a minted run that never reported a session cannot be rescued (`run.focus` refuses
  with error 2 — no recorded conversation id) and the next stage mints fresh instead of
  re-attaching to a conversation the board cannot name.

  Rescue is **idempotent**: before creating anything the daemon scans the placement workspace for a
  pane left
  by an earlier rescue of this exact run, identified by its pane label / agent name
  `card-<id>-r<run>-rescue`. That name depends only on **stable identity** — deliberately not on the
  column name, because renaming a column would otherwise change the marker and resume the same
  conversation a second time. Because no database write is permitted, the name is the only correlator
  available and it is a *diagnostic hint*, not an authoritative record: it is deterministic for panes
  the daemon creates, but a user who renames the pane or its agent can defeat it (for a managed
  harness a second attempt then usually fails closed on `agent_name_taken`, since Herdr agent names
  are exclusive while the pane is open). When the recorded workspace had to be replaced, the
  replacement is found by the card's space config (label find-or-create), so renaming the
  replacement workspace defeats that half of the dedup the same way.

  Matching is on the pane **label**, the one field the daemon both sets (`pane.rename`) and reads
  back (`PaneInfo.label`); the same string is also used as the `agent.start` name purely for Herdr's
  `agent_name_taken` exclusivity backstop. Verified live against Herdr 0.8.0 / protocol 19: `agent.start` leaves a
  board-set label untouched, so labelling once before the launch is sufficient. A matching pane only counts as *live* if its harness is
  still there: a Herdr pane label outlives the process, so for a managed harness the pane must still
  have a registered `agent` (a *presence* test — `PaneInfo.agent` is the agent kind, not the chosen
  name), and in all cases a `done` agent status counts as dead. Otherwise focusing a leftover shell would make `o`
  a permanent no-op after the resumed harness exited. Dead remains carrying this run's exact marker
  are closed before the new pane is split, so repeated presses cannot pile up idle shells that no run
  row could ever reclaim. For a **configured** harness Herdr exposes no `agent` field at all, so a
  leftover shell cannot be distinguished from a live one — see `docs/design.md` → Limitations.

  Rescue takes the same per-card-tab allocation lock as dispatch and registers the exact tab/anchor
  it allocated in the same place, so two concurrent focus requests (or a focus racing a dispatch)
  cannot each split a pane or each create a second `card-<id>` tab.

  Error codes: nothing to focus **and** nothing to resume — no recorded conversation id, or a
  pre-v11 run with no durable launch spec — is error 2 (the same code this dead end reported before
  the rescue existed; the message names the dead pane and points at `run.retry`). A harness that
  does not declare resume support is error 3 and names the harness. Cross-session focus is error 3.
  Herdr/registry unavailable, a run with no recorded workspace, a recorded workspace that is gone
  **and** has no current card space config to supply a replacement (error 4, naming both), tab/pane
  creation failure, and a
  harness that will not start in the new pane are error 4. A failed launch closes the pane it
  created **and**, when placement had to create the `card-<id>` tab, that tab's shell anchor too
  (which removes the empty tab) — and when this very resolution created the workspace, the failure
  also closes that workspace, so a refused or failed rescue leaves nothing behind — a rescue has
  neither a retry nor a run row, so anything orphaned here would be permanent. A `pane.focus` that
  fails *after* a successful launch is logged as a warning and still reported as `rescued`: the pane
  exists and the conversation is resumed, only the focus move was lost. The daemon resolves the run's
  session socket and canonicalizes both it and `origin_socket` before any of this. The CLI resolves
  `origin_socket` from `--origin-socket`, `$HERDR_SOCKET_PATH`, or `$HERDR_SOCK`.

**Internal runner-only method (not public board API):**
`run.pane_exited {card_id,run_id}` is sent only by the hidden `board __pane-exited` configured-harness
wrapper. It accepts the exact matching open queued or started **configured** run (including a callback
that arrives before spawn registration), then records `fail` with summary "configured harness exited
without calling board done", adds "pane exited without board done", leaves the card in its current
column, and does **not** apply `on_fail`. Stale/replaced/already-completed and built-in (pi/claude/codex/opencode)
runs are rejected. This is protected by the local board Unix socket trust boundary, not an unforgeable
token; the wrapper ignores an expected rejection when `run.done` won the race. The generated
script removes itself when it starts; if `pane run` accepts scheduling but the pane never opens
it, a residual configured-script orphan is an explicitly documented limitation.

### harness / spaces
- `harness.capabilities {harness}` → `{harness, models:[{id, efforts:[…]}], model_freeform: bool, default_efforts:[…], permission_modes:[…], resume}`. `default_efforts` is serde-defaulted for backward-compatible clients and applies when model is omitted/free-form; a known model's own efforts remain authoritative. `resume` is `"by_conversation_id"` or `"unsupported"` and answers "can this harness re-attach to a conversation it recorded?" — the question `run.focus` must ask before reopening a run whose pane is gone. It is serde-defaulted to `"unsupported"`, so an older payload fails closed, and there is deliberately no universal-syntax assumption: each adapter declares it.
  - Built-in `pi`: static `models:[]`, `model_freeform:true`, `default_efforts:["off","minimal","low","medium","high","xhigh","max"]`, `permission_modes:[]`. Pi's catalog is user/provider-specific, so the daemon overlays a **live** catalog when it can resolve the pi agent dir (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`): it reads `auth.json` for the authenticated providers, then `models-store.json` and keeps only those providers' models as `provider/model` ids with per-model efforts from each model's `thinkingLevelMap`. Pi's map is tri-state: for standard levels `off` through `high`, an omitted key uses Pi's provider-default mapping, a string is supported with that mapping, and `null` is unsupported; for extended `xhigh`/`max`, only an explicit string is supported (omitted or `null` is unsupported). Efforts remain in canonical ascending order. This reproduces `pi --list-models` (provider-auth scoped) with richer per-model effort data. If the files are missing/unreadable it falls back to shelling out to `pi --list-models`, and finally to the static free-form catalog. `model_freeform` stays `true`, so arbitrary model strings remain valid. Tests leave the agent dir unset, so the catalog stays the static `models:[]`.
  - Built-in `claude` (CLI 2.1.209): models `fable`/`opus`/`sonnet`/`haiku`, each with `low|medium|high|xhigh|max`; the same levels are `default_efforts`; `model_freeform:true`; permissions are `["acceptEdits","auto","bypassPermissions","manual","dontAsk","plan"]`. Both built-ins report `resume:"by_conversation_id"` (`claude --resume <id>`; Pi re-uses `--session-id <id>`).
  - Built-in `codex`: static fallback `models:[]`, `model_freeform:true`, `default_efforts:["off","minimal","low","medium","high","xhigh","max"]`, `permission_modes:["ask-for-approval","approve-for-me","full-access"]`, `resume:"by_conversation_id"` (`codex resume <id>`). At daemon startup `$CODEX_HOME/models_cache.json` (default `~/.codex/models_cache.json`) overlays the fallback with visible model slugs and each model's `supported_reasoning_levels`; missing/malformed caches retain free-form entry, `none` maps to `off`, and unknown protocol levels such as `ultra` are filtered. Model strings remain free-form via `--model`. Board effort maps to `-c model_reasoning_effort=<value>`. Permission presets combine sandbox and approval controls: `ask-for-approval` emits `--sandbox workspace-write --ask-for-approval on-request`; `approve-for-me` emits `--approve-for-me`; `full-access` emits `--dangerously-bypass-approvals-and-sandbox`. Codex mints its own thread/session uuid — there is no `--session-id` for Mint — so the daemon persists `NULL` at enqueue and captures the integration-reported id after launch via `agent.get.agent_session` (`AgentSessionInfo`, `{agent,kind,source,value}` with `kind: "id"`), promoting it atomically onto run+card. See [Dispatch semantics](#dispatch-semantics-column-engine--lives-in-board-core-pure-daemon-executes-effects) for the capture contract and the fail-closed degraded mode.
  - Built-in `opencode`: static fallback `models:[{id:"opencode/nemotron-3-ultra-free", efforts:[]},{id:"opencode/deepseek-v4-flash-free", efforts:["low","high","max"]}]`, `model_freeform:true`, `default_efforts:["off","minimal","low","medium","high","xhigh","max"]`, `permission_modes:["default","auto-approve"]`, `resume:"by_conversation_id"` (`opencode -s <id>`). Models are free-form `provider/model` (via `-m` when no effort is set, else through the `herdr-board` agent config); the daemon overlays the fallback with a **live** catalog whenever `opencode_bin` is configured (`$OPENCODE_BIN`, else `opencode` on `PATH`, resolved at startup): it runs `opencode models --verbose` (argv pinned in `opencode_catalog::models_argv`) and parses the repeated `provider/model` header + JSON-object pairs, mapping each object's `variants` keys onto the board effort ladder in canonical ascending order (opencode's `none` becomes the board's `off`; unknown variant keys such as `thinking` are filtered). A valid model is **listed even when its variants map onto no board effort** — `opencode/nemotron-3-ultra-free` really declares `variants: {}`, so it appears with `efforts:[]` and selecting it offers no effort — while an unknown/free-form model keeps the full ladder; a missing/broken/empty CLI run keeps the static fallback, so the `models` field is always defined. Parsing is bounded (256 entries, 64 KiB per object). The board calls the variant dimension **effort** everywhere in the API/UI/DB. The opencode
root/TUI has **no `--variant` flag** (verified: the spelling exists only on `opencode run`), so an
effort is applied through a process-local `OPENCODE_CONFIG_CONTENT` env defining a stable custom
agent `herdr-board` with exactly `model` + `variant` (board `off` → opencode `none`), selected with
`--agent herdr-board`; `-m` is dropped because the agent owns the model, and an effort without a
model is rejected at enqueue. Without an effort the model stays `-m` and no config is injected.
The two permission modes map to exact verified CLI spellings: `default` emits no flag,
`auto-approve` emits `--auto`; any other value is rejected by engine capability validation before
launch. Like codex, opencode mints its own `ses_…` id — a Mint carries no session flag and the
daemon persists `NULL` at enqueue — and the integration-reported id is captured after launch via
`agent.get.agent_session`, now pinned to the source the current Herdr opencode integration reports,
and promoted atomically onto run+card. See [Dispatch semantics](#dispatch-semantics-column-engine--lives-in-board-core-pure-daemon-executes-effects) for the capture contract and the fail-closed degraded mode.
  - config-defined harnesses report `model_freeform:true` and the declared `models`/`efforts`/`permission_modes`; declared efforts also populate `default_efforts`. `resume` is `"by_conversation_id"` only when `[harness.NAME] resume = true` is declared, otherwise `"unsupported"` — the fail-closed default. Declaring it promises that the harness re-attaches to `$BOARD_RESUME_SESSION_ID` (see `run.focus`). Known model aliases use their declared effort set; omitted or free-form models use `default_efforts` (with a model-union fallback for older payloads that omitted `default_efforts`).
  - error 2 (not found) for an unknown harness, listing the known harnesses.
- `harness.list` (no params) → `{harnesses:[…]}` — every harness the daemon knows about: the built-ins `pi`/`claude`/`codex`/`opencode` in their default order (pi first), then every config-defined `[harness.NAME]` sorted, de-duplicated. This is the single source for BOTH the card `harness` and column `harness_override` selects in the TUI, so every harness menu shares one list in one (default-first) order.
- `space.list {session?}` → `{spaces:[{id, label}]}` — workspaces in the given session (`null` = default), filled from that session's herdr `workspace.list`. Unknown/not-running session → error 4 listing the known sessions.
- `session.list` (no params) → `{sessions:[{name, default: bool, running: bool}], default_label:"default session"}` — the daemon shells out to `herdr session list --json` (session enumeration is not in the herdr socket API; a session only knows itself). `default_label` is the ready display marker for the default session (and for the TUI session selector's unset option), sent so clients never format it themselves. Binary resolved via `$HERDR_BIN_PATH`, else `herdr` on `$PATH`. Error 4 if herdr is unavailable / the CLI fails. That shell-out has a **10-second wall-clock budget** and the child is killed when it expires (error 4, naming the timeout): the session registry sits on the path of every request that resolves a session, and every caller reaches it through the blocking pool, so a hung `herdr` must not pin one of those threads forever. A normal `session list` is sub-100ms; the result is cached for the registry TTL.

### panes

- `pane.set_title {pane_id, title, origin_socket}` → `{renamed:true}` — set the label Herdr renders
  in one pane's border, in the **caller's own** herdr session. `origin_socket` names that session
  exactly as it does for `run.focus` (only the caller knows which Herdr it runs inside); the path is
  canonicalized and then opened through the same gated connect as every other operation, so the
  pinned Herdr 0.8.0 / protocol 19 check runs before the rename. Maps to herdr `pane.rename`, and
  touches no board state — the daemon exists here only because it owns every Herdr call.
  Error 1 for an empty `pane_id`, error 4 for an unavailable socket, a socket that fails the
  protocol gate, or a `pane.rename` herdr refuses (e.g. an unknown pane). A rename that did not
  happen is always one of those errors, never a successful `{renamed:false}`.
  **Whether a failed rename matters is the caller's policy, not the protocol's.** The TUI keeps its
  plugin pane border in sync with the board it shows and drops the result: a cosmetic title must
  never surface an error over the board, and outside a Herdr plugin pane (standalone TUI, tests) it
  never sends the request at all.

## Card statuses & signals

`idle · queued · running · blocked · awaiting · done · failed`

| Status | Meaning |
|---|---|
| `idle` | At rest in a column; no active run. |
| `queued` | Enqueued for dispatch into an auto column. A configured harness may complete this exact run immediately before runner registration; queued built-in (pi/claude/codex/opencode) runs cannot be completed until their managed pane is registered. |
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
| `idle` past `idle_grace_seconds` (no `board done`) | `awaiting` + `idle_expired` + notification. On an already-`awaiting` card it's a no-op (keeps the more specific reason). Herdr may emit `done` then trailing `idle`; that `idle` does not re-arm the grace timer or replace `agent_done`. |
| herdr `unknown`, or any signal on a non-live card | ignored. |
| Herdr `pane_exited` without `board done` | run `fail`, card `failed`, **no** transition (unchanged); watcher identity is `(session socket, pane id)`. |
| configured child returns while its exact run is open (`queued` or `started`) | internal run-id guard records `fail`, card `failed`, **no** transition; callback-before-registration is accepted, while stale/completed and built-in runs are rejected. `board done` likewise requires the exact `BOARD_RUN_ID` during the queued exception, preventing a stale child from completing a replacement. |
| column `timeout_minutes` exceeded | **paused while `awaiting`** (the durable deadline shifts forward by the review span on exit); otherwise run `fail` + `on_fail`. The original deadline survives daemon restart, including already-overdue runs; `NULL` remains unlimited. |
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

- `{"event":"board_changed","reason":"card_moved|card_created|card_updated|card_deleted|card_archived|column_changed|comment_added|run_started|run_ended|run_blocked","board_id"?:N,"card_id"?:N,"column_id"?:N}` — `board_id` scopes the change to a specific board; a cross-board card transfer emits one event per affected board (source + destination). Omitted `board_id` means a coarse, board-agnostic refresh.
- `{"event":"run_ended","card_id":N,"run_id":N,"outcome":"ok|fail|cancelled|lost"}` (also emitted as board_changed; `lost` is legacy — no longer produced, see Card statuses)

## Dispatch semantics (column engine — lives in board-core, pure; daemon executes effects)

1. Card enters an auto column. Under the scheduler→store lock, atomically resolve and snapshot the
   card, column, comments, effective settings, task prompt, system prompt, and target session into the
   queued run. The v7 snapshots are stored byte-for-byte (`outcome=NULL,started_at=NULL`), and the card
   becomes `queued`; later mutations cannot produce stale launch data or a stale `run.session`.
   - `prompt_snapshot` = description plus the last 20 comments (the comments section is omitted when empty);
   - `system_prompt_snapshot` = the effective column prompt plus mandatory board-protocol trailer.
2. Queue key = `(session, space_kind, space_ref)`; one running card per key (FIFO); global semaphore default 3 (config `max_concurrent`). Session is part of the key so the same label/ref in two herdr sessions are distinct spaces.
3. Spawn (daemon, via `Spawner` trait):
   - resolve session: card `session` (null = default) → Herdr socket via the session registry; an unknown/not-running session fails the run with a clear error listing known sessions. The per-session client is used for workspace resolve/create, spawn, kill, and liveness.
    - harness session: resume `card.session_id` unless `column.fresh_session` or none. Pi mint/resume use exact `--session-id`; Pi retry forks old → a newly minted target id. Claude retains mint/`--resume`/`--fork-session`. Codex Mint takes **no session flag** (it mints its own thread id and the enqueued run persists `session_id=NULL` — the board never invents a uuid); codex resume/fork are `resume <id>` / `fork <id>` subcommands appended last to the startup argv. OpenCode Mint also takes **no session flag** (the TUI mints its own `ses_…` id and the enqueued run persists `session_id=NULL`); opencode resume/fork are trailing session flags `-s <id>` / `-s <id> --fork`. Existing cards keep their stored harness/session.
   - **preflight before workspace mutation:** `ping` the selected socket and require exact Herdr 0.8.0 / protocol 19. Only then resolve `workspace` by id/case-insensitive label, or resolve `new_workspace` by label and, if absent, call `workspace.create {label,cwd,focus:false}`. Read the workspace cwd from its pane snapshot; snapshot failure or missing live cwd fails dispatch, never falling back to process cwd or a stale snapshot. When this dispatch itself created the workspace, its exact initial tab/root pane ids travel as a one-shot bootstrap hint: the first card-tab allocation verifies them (tab exists in that workspace, root is the tab's sole pane, root carries no agent), renames the tab to `card-<id>` and the root to `card-<id>-anchor`, and splits the run child from that root — so a daemon-created workspace has no unused initial tab. Any verification mismatch falls back to a fresh `tab.create` and never touches that root; reused/existing/user workspaces never carry a hint.
   - **preflight again at the spawner boundary:** this is the spawner's first protocol call, before placement, managed launch, or the configured runner.
   - build the run-child env `{BOARD_CARD_ID,BOARD_RUN_ID,BOARD_SOCKET,BOARD_BIN}` plus configured-harness prompt env. Current schema v13 runs place each card in a stable short `card-<id>` tab whose root is a shell anchor labeled `card-<id>-anchor`. The anchor receives only stable card identity; every run child is created by `pane.split` from it with the complete run cwd/env. Promotion persists the exact anchor id with the run **except** for managed launches, whose anchor is closed after a successful launch (see below), leaving the tab with exactly the harness pane and a NULL persisted anchor. The daemon reuses only exact tab/anchor identities reconstructed from the newest matching durable panes in the same session/workspace; labels are display metadata, never ownership. A renamed anchor remains selected by identity; a closed anchor is recreated only by splitting a currently live durable board child, otherwise a fresh tab is created. Same-conversation reuse eligibility is checked **before** anchor selection, so an anchorless managed tab still reuses its exact prior harness pane on the next hop. The initial split targets ratio `0.40` and clamps it on narrow terminals so the anchor remains reusable; later splits use layout geometry. Both fresh and recovered placement fail closed unless the live layout can provide a 24x6 anchor and a 12x8 child. Concurrent first allocations for one `(session,workspace,card)` key are serialized; if multiple historical panes are live, newest run id wins. Legacy rows retain the historical `kanban` lookup. Thus cwd/env/placement exist **before** launch; pane-first `agent.start` receives none of them and never receives the anchor pane id.
   - managed Pi/Claude: create a mode-`0600` file containing the snapshotted system prompt; call `agent.start {name,kind,pane_id,args,timeout_ms:30000}` on the newly split child with prompt-free startup args and the harness-specific file flag. A typed `agent_pane_busy` response is treated as a bounded transient on that same child: retry the exact same request on the same pane at most five times, with 100ms backoff doubling per retry (100/200/400/800/1600ms — long enough for a slow login shell to reach its prompt); do not split or allocate another pane. Persistent busy is terminal and follows child-only cleanup, leaving the anchor. This is distinct from `pane_not_found`, which is a placement race: close the child when present, rediscover from `tab.list`, and retry complete placement once. Poll `agent.get {target:pane_id}` for at most 30s until `interactive_ready && !launch_pending`; then call `agent.prompt {target:pane_id,text:prompt_snapshot}`. Remove the prompt file before returning, including error paths.
    - managed self-minting harnesses (codex and opencode): the same `agent.start` readiness contract **without any prompt file** — neither harness has a system-prompt-file equivalent, so startup argv carries neither system nor task text and there is no `--` delimiter. After readiness the daemon runs a **bounded post-launch capture** on the same gated connection: it polls `agent.get` (at most 5 probes, 10s wall-clock cap) for `AgentInfo.agent_session`, and accepts only an `id`-kind reference owned by the expected agent with a non-empty `value` — for codex the source is deliberately unconstrained, while opencode pins the exact source the current Herdr opencode integration reports. The capture is ordered per harness: for **codex** it runs before the prompt (the integration reports its thread id as soon as the CLI is interactive); for **opencode** it runs **after** the prompt — real OpenCode mints its `ses_…` id and reports `agent_session` only once the first `agent.prompt` lands, so a pre-prompt capture would lose the id, while a prompt-less opencode rescue reduces to capture-after-readiness. A missing, wrong-agent, wrong-source, `path`-kind, or blank report degrades to `None` with a warning and the launch **continues**: basic execution works, but the run keeps its enqueue-time `NULL` id, so same-conversation reuse hops and `run.focus` rescue fail closed (no recorded id). A captured id is persisted atomically **with** the run promotion (run + card in one transaction), so a cancel-during-spawn that ends the run discards the capture together with the handle; a capture racing promotion writes through the same single-row UoW and fails closed if the run is no longer open. Then `agent.prompt` delivers the prompt (before the capture for codex, after it for opencode): a Mint receives one delimited block (`## herdr-board system instructions` + `## herdr-board card task`), a resume/fork fresh pane receives the task alone, same-pane reuse the task alone, and a rescue sends nothing. **After a successful managed launch** (fresh or reuse), the daemon closes the tab anchor with `pane.close` — closing a split parent is live-verified safe, and the harness pane keeps its process/env — so the tab converges to exactly one harness pane; the promoted run persists a NULL anchor. If that close fails, the anchor is kept (and persisted) rather than failing the already-successful launch. A failed launch never closes the anchor: it remains for the next allocation, per the child-only cleanup rule.
   - managed pane name is `card-<id>-<column-slug>` (e.g. `card-14-execute`); `agent_name_taken` retries once on the same pane with `card-<id>-<column-slug>-r<run>`.
   - configured harness: `pane.rename` the owned pane, create one mode-`0700` self-removing script whose POSIX-quoted command is the exact configured argv, and invoke exactly the selected Herdr binary (`HERDR_BIN_PATH` when nonempty, otherwise `herdr`) as `pane run <pane_id> <script_path>` with `HERDR_SOCKET_PATH` set to the selected socket. The script runs the child, preserves its status, then calls hidden `board __pane-exited --run-id "$BOARD_RUN_ID"`; the internal run-id guard accepts only the exact open queued/started configured run (including callback-before-registration), rejects stale/completed and built-in runs, and never applies `on_fail`.
   - a disappearing selected/owned child restarts discovery at `tab.list` and retries the complete placement once. Retry/terminal cleanup closes only the board-created run child; the shell anchor and pre-existing panes are never closed. `pane_not_found` means cleanup already won. This placement rediscovery path is separate from the bounded same-child `agent_pane_busy` retry above. A synchronous configured-runner failure also removes its script; after successful scheduling, the script owns self-removal.
   - card status `running`, store exact pane/workspace ids + `session` on the run, emit `run_started`.
4. Finish signals, priority order (the full signal→state mapping is under
   [Card statuses & signals](#card-statuses--signals); the engine is the single decider):
   - `run.done` from the agent (primary; semantics above)
   - Herdr `pane_exited` while running, or the configured wrapper's matching active-run guard after its child returns → outcome `fail`, system comment "pane exited without board done", card status `failed`, **no** transition
   - herdr agent_status `done` with no `run.done` → card `awaiting` (reason `agent_done`), run stays OPEN, notification
   - herdr agent_status `idle` sustained > `idle_grace_seconds` (default 90) with no `run.done` → card `awaiting` (reason `idle_expired`), run stays OPEN, notification
   - `timeout_minutes` (column) exceeded → `pane.close`, outcome `fail`, apply on_fail; **paused while the card is `awaiting`**
   - agent_status `working` → card status `running`, clearing blocked/awaiting (idle tracking is disarmed while awaiting)
   - agent_status `blocked` → card status `blocked`, board change + Herdr notification (run stays active)
5. Every transition posts a `system` comment (e.g. "Plan ok in 4m12s → Execute").
6. Manual-trigger columns on entry: status `idle`, herdr notification if entered via auto-transition.

## Harness adapters (board-core)

New built-in runs are explicit Herdr-managed agents; executable names are not used to infer this.
Their persisted startup argv contains neither system nor card prompt:

- Built-in `pi` (default):
  `pi [--model provider/model] [--thinking off|minimal|low|medium|high|xhigh|max] (--session-id ID | --fork OLD --session-id NEW)`
  - omitted model/thinking means Pi uses its configured defaults;
  - no permission, approval, or `--allowedTools` flag is added; Pi project trust is separate;
  - pane-first managed launch uses `kind:"pi"`, startup args without `pi`, then appends
    `--append-system-prompt <mode-0600-file>`; only after readiness does `agent.prompt` carry the
    unprefixed `prompt_snapshot`.
- Built-in `claude`:
  `claude [--model M] [--effort E] [--permission-mode P] --allowedTools "Bash(board:*)" (--session-id UUID | --resume ID [--fork-session])`
  - pane-first managed launch uses `kind:"claude"`, startup args without `claude`, then appends
    `--append-system-prompt-file <mode-0600-file>`; `agent.prompt` separately carries the card task.
- Built-in `codex`:
  `codex [--model M] [-c model_reasoning_effort=E] [permission preset flags] (resume <id> | fork <id>)`
  - board effort `off` maps to `model_reasoning_effort=none` only while building argv; `minimal`
    through `max` keep their canonical spelling, and the `Effort` enum is unchanged (no `ultra`);
  - `ask-for-approval` emits `--sandbox workspace-write --ask-for-approval on-request`;
    `approve-for-me` emits `--approve-for-me`; `full-access` emits
    `--dangerously-bypass-approvals-and-sandbox`;
  - `resume <id>` / `fork <id>` are **subcommands** closing the startup argv (Mint carries no
    session flag at all and no board-invented uuid);
  - no system-prompt file and no prompt text in argv: the managed `agent.prompt` channels are the
    only prompt transport (Mint: delimited `system + task` block; resume/fork fresh pane: task
    only; same-pane reuse: task only; rescue: nothing).
  - the thread id reported by the codex integration (`AgentInfo.agent_session`, `kind:"id"`) is
    captured after readiness and **before** the prompt, then persisted atomically with the run
    promotion (`NULL` at enqueue for a Mint); a fork's newly captured id supersedes the recorded
    source id.
- Built-in `opencode`:
  `opencode [--agent herdr-board | -m provider/model] [--auto] (-s <id> | -s <id> --fork)`
  - the root/TUI has **no `--variant` flag** (verified against opencode 1.18.15 — the spelling
    exists only on `opencode run`), so with a board effort the launch selects a process-local
    config agent with `--agent herdr-board`: the exact `OPENCODE_CONFIG_CONTENT` JSON
    (`{"agent":{"herdr-board":{"model":…,"variant":…}}}`) rides the launch env, is persisted in
    the launch spec (so resume/rescue keep it), and `-m` is dropped — the agent owns the model,
    and an effort with no model is rejected at enqueue. Board effort `off` maps to the opencode
    variant `none` only while building that config; `minimal` through `max` keep their canonical
    spelling, and the `Effort` enum is unchanged. Without an effort the model stays
    `-m provider/model` and no config env is injected;
  - permission modes are board-facing presets with exact verified CLI spellings: `default` emits no
    flag (the CLI's own manual-approval default), `auto-approve` emits `--auto`; the engine's
    capability validation rejects any other value before launch;
  - `-s <id>` / `-s <id> --fork` are trailing session flags closing the startup argv (a Mint carries
    no session flag at all and no board-invented uuid — the TUI mints its own `ses_…` id);
  - no system-prompt file and no prompt text in argv: the managed `agent.prompt` channels are the
    only prompt transport (Mint: delimited `system + task` block; resume/fork fresh pane: task
    only; same-pane reuse: task only; rescue: nothing);
  - the session id reported by the opencode integration (`AgentInfo.agent_session`, `kind:"id"`,
    source pinned to what the current Herdr opencode integration reports) is captured **after the
    first prompt** — real OpenCode mints `agent_session` only once a prompt lands; a prompt-less
    rescue reduces to capture-after-readiness — and persisted atomically with the run promotion
    (`NULL` at enqueue for a Mint); a fork's newly captured id supersedes the recorded source id.
- Config-defined harnesses (`~/.config/herdr-board/config.toml`) remain unmanaged even if their
  executable is named `pi`, `claude`, or `opencode` — the built-in names are matched first, so a
  config section under a built-in name (e.g. `[harness.opencode]`) is unreachable and must be
  renamed (e.g. `[harness.opencode-run]`):
  ```toml
  [harness.fake]
  argv = ["bash", "/path/to/fake-agent.sh"]   # exact argv; prompt via $BOARD_PROMPT
  resume = false                              # default: cannot resume a conversation
  ```
  `BOARD_PROMPT` and trailer-inclusive `BOARD_SYSTEM_PROMPT` are installed in the pane env.
  `resume = true` opts the harness into the `run.focus` rescue and promises it re-attaches to
  `$BOARD_RESUME_SESSION_ID` (set, together with `BOARD_RESCUE=1`, only on a reopened pane, which
  never receives `BOARD_PROMPT`). Template
  elements support `{model}`, `{effort}`, `{permission_mode}` and are dropped if their value is unset.
  The 0700 script bridge described above preserves multiline/special-character argv boundaries that
  direct `herdr pane run` cannot preserve.
- `permission_mode=bypassPermissions` is refused unless the card explicitly sets it (never via column override).

For every new v7 run, `system_prompt_snapshot` is authoritative for managed and configured launch.
Legacy pre-v7 rows are deliberately not backfilled: NULL built-in rows remain unmanaged and execute
their persisted historical all-in-one argv, avoiding duplicate prompt delivery; NULL configured rows
retain the historical current-column system-prompt reconstruction at spawn. The local test spawner
materializes the historical all-in-one Pi/Claude argv from explicit managed metadata, but the Herdr
path always uses the separated pane-first channels.

Pi lifecycle status comes from Herdr's official Pi integration and the existing event watcher; there
is no Pi-specific watcher. Codex lifecycle status comes from Herdr's official codex integration the
same way, and OpenCode's from Herdr's official opencode integration. Without `herdr integration
install pi` (or `… codex` / `… opencode`), explicit `board done`, spawn
failure, timeout, and pane exit still work, but working/blocked/done detection is
unavailable and the idle→`awaiting` watchdog does not arm while status remains `unknown`
(see [Card statuses & signals](#card-statuses--signals)). For codex the integration also reports the
thread id that enables resume/reuse/rescue; without it the run still executes and completes, but the
conversation cannot be reopened by id. The opencode integration reports the `ses_…` session id the
same way, and its absence fails closed exactly as for codex: basic execution continues with
`session_id = NULL`, while reuse/rescue are refused.
