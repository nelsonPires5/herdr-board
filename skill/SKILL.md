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
Herdr session and workspace. New cards default to Pi; the built-in catalog is `pi`, `claude`, `codex`
(in that order), then any config-defined `[harness.NAME]`. **Columns** are pipeline stages: `manual` waits
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
`n` for a card, `N` for a column, `m` to move a card, `M` to reorder the focused column, `O` to
reorder the selected card within its column (`j`/`k` stage, `Enter` commits, `Esc` cancels),
`Enter` for detail, `e`/`E` to edit a card/column, `a` to archive/restore, `v` for
active/all/archived visibility, and `?` for help.
Moving into an `auto` column dispatches a run. `o` focuses the newest same-session run pane; `Enter`
on an `awaiting` card confirms it through the same completion channel as `board done --outcome ok`.
Below 60 columns the TUI switches to a single-column Compact layout with a tappable
`‹ [ ⇄ <column> n/N] ›` header and a board/column switcher sheet (open it with `b`); every layout
mode scrolls the focused column's cards independently, with a scrollbar shown whenever a column
overflows.

## CLI taxonomy

The nested forms are canonical. `--board` (a stable id or canonical scope path) and `--json` are both
**global** flags: either position parses identically, before or after the subcommand path.

```bash
board --board <ID|PATH> card list --json
board card list --board <ID|PATH> --json     # identical
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
board card move ID COLUMN [--position ZERO_BASED_POSITION] [--destination-board ID|PATH] [--json]
board card duplicate ID [--json]
board card archive ID [--json]
board card restore ID [--json]
board card delete ID [--yes] [--json]
```

`card duplicate` copies a card into a fresh idle card inserted directly below it: title gains
` (copy)`, and description, harness, model, effort, permission mode, session, and space
configuration are copied verbatim — but status is `idle` with no runs, comments, conversation id,
or archive flag. The original is never modified, and the copy is never dispatched, even in an
auto column; run it (or move it) explicitly like any idle card. In the TUI, `C` duplicates the
focused card from the board or card detail.

`card new` is retained as an alias for `card create`. A new card defaults to Pi; an omitted model or
effort uses the harness default. `--harness codex` selects the built-in Codex adapter: models are
free-form (`--model` any string), every effort level is accepted (`off|minimal|low|medium|high|xhigh|max`;
`off` is delivered to codex as `model_reasoning_effort=none`), and `--permission` takes
`ask-for-approval|approve-for-me|full-access`. These are board-facing presets: ask on request in a
workspace-write sandbox, automatic review in a workspace-write sandbox, or bypass approvals and
sandbox respectively. Codex mints its own conversation thread id —
the board never invents one — and captures the integration-reported id after launch, so later stages
can `resume`/`fork` the same conversation; a minted run whose id was never reported cannot be
reopened by id (`card run focus` refuses; use `card run retry` for a new run).
`new-workspace` requires both `--space-ref` and `--space-cwd`. For an existing `workspace`, set
`--space-cwd` when its live panes intentionally use different directories; without it, dispatch
requires all non-empty live pane cwd values to agree and fails closed when they differ.
Creating directly in an `auto` column dispatches immediately. `card list` defaults to active cards;
`all` includes archived cards and `archived` returns only archived cards. `card show` includes current
comments and run history; soft-deleted comments are omitted.

A cross-board move goes on `--destination-board`. The old fallback — a global `--board` naming a
*different* board than the card's — still works but prints a deprecation warning to stderr; do not
write new automation against it.

Omitted edit options are unchanged. Explicit `--clear-*` flags clear nullable values;
`--clear-description` sets the description to an empty string. Harness is required and cannot be
cleared (`--clear-harness` parses but is refused). Model/effort/permission/session/space edits are
refused while a card has an open run.
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
show/edit return the current record with those fields plus `deleted_at`; `card show` prints each
comment in that same one-line shape. Delete is a soft delete and
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
board card run focus CARD_ID RUN_ID [--origin-socket SOCKET] [--json]
```

`card run confirm` is the `done --outcome ok` channel for an `awaiting` card. `card run focus`
takes an explicit `RUN_ID` (there is no implicit "latest run"; read it from `card show --json`
`runs[]`), returns
`{action, recorded_pane_id?, run_id, card_id, column_id, harness, session, session_id, pane_id}`,
and requires that run's pane to belong to the current Herdr session; `session` is the herdr session
name while `session_id` is the harness conversation id. Without `--origin-socket` it uses
`HERDR_SOCKET_PATH` or `HERDR_SOCK`.

`action` tells you what happened: `focused_recorded_pane` (the run's pane was alive),
`focused_rescued_pane` (its pane is gone but an earlier reopen's pane is still alive), or `rescued`
(its pane is gone, so the harness conversation was **resumed in a new pane** in the card's tab). A
rescue never re-sends the card task and never writes to the database, so the reopened pane is
ephemeral: it has no run row and is not watched or timed out. It gets `BOARD_CARD_ID`/`BOARD_SOCKET`
but deliberately **no** `BOARD_RUN_ID`, so from inside it `board comment` still records on the card
(as a human comment) while `board done` does not apply — the run stays closed. Closing the pane is up
to you. Resuming requires an explicit per-harness capability (`pi`, `claude`, and
`codex` have it; a `[harness.NAME]` harness needs `resume = true`) and a recorded conversation id;
without either, focus is refused explicitly — use `card run retry` for a new run instead.

Done/cancel/retry return `{run, card}` in JSON. Retry creates a new run while preserving history.

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
board harness list [--json]
board harness models [HARNESS] [--json]
board harness efforts [HARNESS] --model MODEL [--json]
board harness permissions [HARNESS] [--json]
board space list [--session SESSION] [--json]
board session list [--json]
```

`HARNESS` is a positional and defaults to `pi`; `harness efforts` additionally **requires**
`--model`. Column references accept an id or case-insensitive name. List and reorder return ordered
arrays; create/show/edit return a column. `--fresh-session` and `--reuse-session` conflict at
**parse time** — passing both is a usage error (exit 64), not a runtime rejection.
A column containing cards needs `--move-cards-to`; any open card run blocks deletion. Column
settings are validated as a complete merged configuration, and effective card+column settings are
checked again before dispatch. Pi has no permission modes; `bypassPermissions` is never a column
override.

### TUI, daemon, version, skill

```bash
board tui
board daemon start [--foreground]
board daemon stop [--json]
board daemon status [--json]
board version [--json]
board skill
```

- `board tui` opens the kanban TUI, auto-starting boardd. `daemon start` runs boardd in this
  process; `--foreground` additionally logs to stderr and stays attached. Bare `board daemon` (no
  subcommand) is unchanged, and the historical `board daemon --foreground` / `board daemon --stop`
  flags still work but are hidden from `--help`.
- `daemon status` is the operational probe: `{version, db_path, herdr_connected, active_runs,
  queued_runs}`. `daemon stop --json` reports `{"stopped": bool, "was_running": bool}`; it is
  fail-closed and errors rather than removing a socket it cannot prove is stale.
- `board version --json` never starts boardd and reports `{cli_version, daemon_version}`. The daemon
  value is `null`/`unavailable` when boardd is offline; use daemon status for liveness and run counts.
- `board skill` prints this exact checked-in `skill/SKILL.md` file, byte-for-byte, with no JSON wrapper.

### JSON and errors

Successful `--json` output goes to stdout. JSON errors go to stderr, leave stdout empty, and use the
stable envelope `{"error":{"code":N,"kind":"...","message":"...","details":...}}`; `kind` and
`details` are additive and may be absent. An error the **daemon** raised carries its protocol code —
1 bad request, 2 not found, 3 invalid state, 4 Herdr unavailable, 5 internal. An error the **CLI**
itself raised carries `{"code":64,"kind":"cli"}`.

Bad enum values are one shape everywhere: `invalid <kind> '<value>' (expected: a, b, c)`.

### Exit codes

Scripted agents should branch on `$?`, not on stderr text.

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1`–`5` | The daemon's protocol code, passed straight through (see above). |
| `64` | The CLI itself refused: a clap usage/parse error, a declined confirmation prompt, a bad enum value, a column name that resolves to nothing client-side, or a missing `$BOARD_CARD_ID`. `EX_USAGE`. |
| `70` | The daemon reported a protocol code outside `1..=5`. Clamped, because an exit status is taken mod 256. `EX_SOFTWARE`. |

```bash
board done --outcome ok || case $? in
  2) echo "no open run for this card" ;;
  4) echo "herdr is unavailable; retry later" ;;
  64) echo "my own invocation was wrong" ;;
esac
```

### Aliases

Legacy top-level run/action forms remain supported and re-dispatch into the nested handlers:
`board comment`, `board done`, `board move`, `board cancel`, and `board retry`. `card new` and
`--to-board` are also retained aliases. Prefer the nested forms above for new automation.

## Creating work

Create the card first, then move it into an `auto` column:

```bash
board card create --title "Add retry to the uploader" \
  -d "Retry failed PUTs 3x with backoff and add a unit test." \
  --effort low --space-kind new-workspace --space-ref uploader \
  --space-cwd /path/to/repo
board card move <new-card-id> Execute
```

The agent will comment and call `board done`; the daemon applies the column transition until a manual
gate is reached. Never bypass that transition with a self-directed `move`.
