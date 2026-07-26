---
name: herdr-board
description: >-
  Interact with the herdr-board kanban from inside an agent run or an
  interactive session. Use whenever you need to report progress on a board
  card, close out a run, add or edit a comment, move/cancel/retry a card,
  inspect cards or columns, or create new work on the board. Triggers on
  mentions of the board, cards, columns, kanban, board comment/done/move, or
  $BOARD_CARD_ID.
---

# herdr-board

This is the operational skill for people and dispatched agents using herdr-board. It covers the TUI,
CLI, cards, runs, comments, columns, and Herdr-backed spaces; it does not prescribe how to develop,
prototype, test, or release herdr-board itself.

A **card** is a title/description plus harness/model/effort, optional harness permission, and a target
Herdr session and workspace. New cards default to Pi. **Columns** are pipeline stages: `manual` waits
for a person and `auto` dispatches a visible agent run on entry. Each canonical Git root (or exact
non-Git CWD) has an independent board; the preserved `Global` board remains available. Agents report
through `board`; never edit the database. The daemon owns state and column transitions.

## Inside a run

`$BOARD_CARD_ID`, `$BOARD_RUN_ID`, and `$BOARD_SOCKET` are preset. `board comment` posts as
`agent:$BOARD_RUN_ID` when the run id is present. Do the stage's work, comment the result, then close
the run.

Rules — follow exactly:

- **Always comment before `board done`.** The next stage receives the comments; a bare `done` loses
  your useful result.
- `board done --outcome ok` means the stage goal was met. `--outcome fail` means it was not met;
  `fail` is a semantic verdict, not merely a report of a recovered tool error.
- **Never move, cancel, or retry your own card to advance it.** `board done` applies the column's
  transition. Use cancel/retry only when the operator or task explicitly requires it.
- A card id is optional for top-level `comment` and `done`; both default to `$BOARD_CARD_ID`.

```bash
board comment "Implemented X; added tests, all green. Touched src/foo.rs."
board done --outcome ok --summary "feature X shipped, tests green"
# If the stage goal was not met:
board done --outcome fail --summary "2 integration tests still fail; needs schema work"
```

## TUI

Run `board tui` or invoke the `open-board` Herdr plugin action. Use arrows or `h/j/k/l` to navigate,
`n` for a card, `N` for a column, `m` to move a card, `Enter` for detail, `e`/`E` to edit a
card/column, `a` to archive/restore, `v` for active/all/archived visibility, and `?` for help.
Moving into an `auto` column dispatches a run. `o` focuses the newest same-session run pane; `Enter`
on an `awaiting` card confirms it through the same completion channel as `board done --outcome ok`.

## CLI taxonomy

The nested forms are canonical. A global board selector accepts a stable id or canonical scope path:

```bash
board --board <ID|PATH> card list --json
```

Without `--board`, board-aware commands use the focused Git root, or the canonical CWD outside Git;
`BOARD_SCOPE_PATH` is a deterministic automation override. `card create/list`, `column list`, and
board commands use the selected board. Card-id operations infer the card's own board.

### Boards and templates

```bash
board board list [--json]
board board show [ID|PATH] [--json]
board board open <PATH> [--json]
board board rename [ID|PATH] <NAME> [--json]
board template apply pipeline [--json]
```

`board show` and `board open` return a snapshot with `board`, `columns`, `cards`, and `active_runs`.
The `pipeline` template is atomic and only applies to an empty board containing exactly the seed
`Todo` column; it returns the resulting column array.

### Cards

```bash
board card create --title TITLE [-d DESCRIPTION] [--column COLUMN] \
  [--harness HARNESS] [--model MODEL] [--effort EFFORT] [--permission MODE] \
  [--session SESSION] [--space-kind workspace|new-workspace] \
  [--space-ref REF] [--space-cwd DIR] [--json]
board card edit ID [--title TITLE] [-d DESCRIPTION] [--clear-description] \
  [--harness HARNESS] [--model MODEL|--clear-model] [--effort EFFORT|--clear-effort] \
  [--permission MODE|--clear-permission] [--session SESSION|--clear-session] \
  [--space-ref REF|--clear-space-ref] [--space-cwd DIR|--clear-space-cwd] [--json]
board card show ID [--json]
board card list [--column COLUMN] [--visibility active|all|archived] [--json]
board card move ID COLUMN [--destination-board ID|PATH] [--json]
board card archive ID [--json]
board card restore ID [--json]
board card delete ID [--yes] [--json]
```

`card new` is retained as an alias for `card create`. A new card defaults to Pi; an omitted model or
effort uses the harness default. `new-workspace` requires both `--space-ref` and `--space-cwd`.
Creating directly in an `auto` column dispatches immediately. `card list` defaults to active cards;
`all` includes archived cards and `archived` returns only archived cards. `card show` includes current
comments and run history; soft-deleted comments are omitted.

Omitted edit options are unchanged. Explicit `--clear-*` flags clear nullable values;
`--clear-description` sets the description to an empty string. Harness is required and cannot be
cleared. Model/effort/permission/session/space edits are refused while a card has an open run.
Archiving and deletion also require an idle/terminal card; cancel an open run first. Delete is
permanent and removes card history. It prompts on a TTY and requires `--yes` in non-interactive use.

### Comments

```bash
board card comment add CARD_ID BODY [--json]
board card comment show COMMENT_ID [--json]
board card comment edit COMMENT_ID BODY [--json]
board card comment delete COMMENT_ID [--yes] [--json]
board card comment history COMMENT_ID [--json]
```

Add returns the compact current comment (`id`, `card_id`, `author`, `body`, `created_at`);
show/edit return the current record with those fields plus `deleted_at`. Delete is a soft delete and
returns `{"deleted":true}`. History returns immutable snapshots from creation through the latest
edit; deletion marks the final snapshot. Card detail and run prompts show only non-deleted comments.

When `$BOARD_RUN_ID` is set, add creates `author=agent:<run-id>`, and that open durable run may
edit or delete only comments owned by its exact `agent:<run-id>` author. Calls without a run id are
human operations and may edit/delete non-system comments. System comments are immutable; deleted
comments are immutable. The legacy `board comment [CARD_ID] BODY` form remains available and defaults
the card id to `$BOARD_CARD_ID`.

### Runs

```bash
board card run done [CARD_ID] --outcome ok|fail [--summary SUMMARY] [--json]
board card run confirm [CARD_ID] [--summary SUMMARY] [--json]
board card run cancel CARD_ID [--json]
board card run retry CARD_ID [--json]
board card run focus CARD_ID [--origin-socket SOCKET] [--json]
```

`card run confirm` is the `done --outcome ok` channel for an `awaiting` card. `card run focus`
returns `{run_id, pane_id}` and requires the newest run pane to belong to the current Herdr session;
without `--origin-socket` it uses `HERDR_SOCKET_PATH` or `HERDR_SOCK`. Done/cancel/retry return
`{run, card}` in JSON. Retry creates a new run while preserving history.

### Columns and discovery

```bash
board column list [--json]
board column create --name NAME [--prompt TEXT] [--trigger manual|auto] \
  [--on-success COLUMN] [--on-fail COLUMN] [--fresh-session|--reuse-session] \
  [--harness HARNESS] [--model MODEL] [--effort EFFORT] [--permission MODE] \
  [--timeout MINUTES] [--position ZERO_BASED_POSITION] [--json]
board column show COLUMN [--json]
board column edit COLUMN [--name NAME] [--prompt TEXT|--clear-prompt] \
  [--trigger manual|auto] [--on-success COLUMN|--clear-on-success] \
  [--on-fail COLUMN|--clear-on-fail] [--fresh-session|--reuse-session] \
  [--harness HARNESS|--clear-harness] [--model MODEL|--clear-model] \
  [--effort EFFORT|--clear-effort] [--permission MODE|--clear-permission] \
  [--timeout MINUTES|--clear-timeout] [--json]
board column reorder COLUMN ZERO_BASED_POSITION [--json]
board column delete COLUMN [--move-cards-to COLUMN] [--yes] [--json]
board harness list|models|efforts|permissions [--json]
board space list [--session SESSION] [--json]
board session list [--json]
```

Column references accept an id or case-insensitive name. List and reorder return ordered arrays;
create/show/edit return a column. `--fresh-session` and `--reuse-session` are mutually exclusive.
A column containing cards needs `--move-cards-to`; any open card run blocks deletion. Column
settings are validated as a complete merged configuration, and effective card+column settings are
checked again before dispatch. Pi has no permission modes; `bypassPermissions` is never a column
override.

## Status, version, skill, JSON, and aliases

- `board daemon status [--json]` is the only supported daemon status command and the operational
  probe: `{version, db_path, herdr_connected, active_runs, queued_runs}`.
- `board version --json` never starts boardd and reports `{cli_version, daemon_version}`. The daemon
  value is `null`/`unavailable` when boardd is offline; use daemon status for liveness and run counts.
- `board skill` prints this exact checked-in `skill/SKILL.md` file, byte-for-byte, with no JSON wrapper.
- Successful `--json` output goes to stdout. JSON errors go to stderr, leave stdout empty, and use
  the stable envelope `{"error":{"code":N,"kind":"...","message":"...","details":...}}`;
  `kind` and `details` are additive and may be absent. Codes are 1 bad request, 2 not found/CLI
  error, 3 invalid state, 4 Herdr unavailable, and 5 internal error.
- Legacy top-level run/action forms remain supported: `board comment`, `board done`, `board move`,
  `board cancel`, and `board retry`. `card new` and `--to-board` are also retained aliases. Prefer
  the nested forms above for new automation.

When creating work, create the card first, then move it into an `auto` column:

```bash
board card create --title "Add retry to the uploader" \
  -d "Retry failed PUTs 3x with backoff and add a unit test." \
  --effort low --space-kind new-workspace --space-ref uploader \
  --space-cwd /path/to/repo
board card move <new-card-id> Execute
```

The agent will comment and call `board done`; the daemon applies the column transition until a manual
gate is reached. Never bypass that transition with a self-directed `move`.
