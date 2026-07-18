---
name: herdr-board
description: >-
  Interact with the herdr-board kanban from inside an agent run or an
  interactive session. Use whenever you need to report progress on a board
  card, close out a run, add a comment, move/cancel/retry a card, inspect
  cards or columns, or create new work on the board. Triggers on mentions of
  the board, cards, columns, kanban, "board comment/done/move", or $BOARD_CARD_ID.
---

# herdr-board

herdr-board is a kanban board for AI coding agents. A **card** is a prompt (title +
description) plus harness/model/effort, an optional harness-specific permission, and a target herdr space. New cards default to Pi; runs remain harness-neutral. **Columns**
are pipeline stages; a column can be `manual` or `auto`. Moving a card into an `auto`
column dispatches an agent run into a visible herdr pane. Agents report back to the board
via the `board` CLI — never by editing the DB. The daemon (`boardd`) owns all state and
handles column transitions; you never move a card out of your own stage.

## Inside a run (you are the dispatched agent)

`$BOARD_CARD_ID`, `$BOARD_RUN_ID`, and `$BOARD_SOCKET` are preset. `board comment` posts as
`agent:$BOARD_RUN_ID`. Your job: do the stage's work, comment your results, then close the run.

Rules — follow exactly:
- ALWAYS `board comment` your results/findings BEFORE calling `board done`. The transition
  comment and next stage depend on what you left behind; a bare `done` loses your work.
- `board done --outcome ok` = this stage's goal was met. `--outcome fail` = the stage goal
  was NOT met (e.g. tests still red, plan not approvable). `fail` is a semantic verdict, not
  a crash report — a tool error you recovered from is still `ok` if the goal is met.
- NEVER `board move`/`cancel`/`retry` your own card to advance yourself. `board done` applies
  the column's transition (ok → on_success column, fail → on_fail). Moving yourself corrupts
  the pipeline.
- `CARD_ID` is optional on `comment`/`done` — it defaults to `$BOARD_CARD_ID`.

```bash
board comment "Implemented X; added 3 tests, all green. Touched src/foo.rs, src/bar.rs."
board done --outcome ok --summary "feature X shipped, tests green"
# stage goal not met:
board done --outcome fail --summary "2 integration tests still failing; needs a schema change"
```

## CLI reference

```
board card show <ID> [--json]            # card + comments + run history
board card list [--column C] [--json]    # cards, optionally one column
board card archive <ID> [--json]          # reversible; preserves comments/runs
board card restore <ID> [--json]
board column list [--json]               # columns in order
board comment [CARD_ID] <BODY> [--json]  # CARD_ID defaults to $BOARD_CARD_ID
board done [CARD_ID] --outcome ok|fail [--summary S] [--json]
board move <CARD_ID> <COLUMN> [--json]   # COLUMN = name (case-insensitive) or id
board cancel <CARD_ID> [--json]          # kill the run; card -> failed, no transition
board retry <CARD_ID> [--json]           # re-run in current column (forks session)
board status [--json]                    # daemon status
board session list [--json]              # herdr sessions (name, running, default)
board space list [--session S] [--json]  # workspaces in a session (default if unset)
board card new --title T [-d DESC] [--column C] [--harness H] [--model M] \
   [--effort E] [--permission P] [--session S] \
   [--space-kind workspace|new-workspace] [--space-ref R] [--space-cwd DIR] [--json]
```

A card picks a **herdr session** (`--session`, the daemon's default session when
unset) and a **space** within it: `workspace` runs in an already-open workspace
(`--space-ref` = its id or label), while `new-workspace` makes the daemon open a
workspace on first dispatch (`--space-ref` = label, `--space-cwd` = its working
dir — both required). There is no worktree space kind; run per-branch isolation
from the agent prompt instead.

Archiving is a human/interactive-session action. It is refused for cards with an active or queued
run; cancel first if appropriate. Archived cards must be restored before move/retry.

Examples:
```bash
board card show 42 --json
board card list --column Execute
board comment 42 "Blocked: need the API key in the workspace env."
```

## Creating work on the board (interactive sessions)

To queue work, create a card then move it into an `auto` column — the move is the trigger
that dispatches the agent. Creating a card directly into an `auto` column dispatches at once.
`manual` columns (e.g. a human-review gate) just hold the card until a person acts.

```bash
# Create in the default Todo column, then dispatch by moving into an auto column:
board card new --title "Add retry to the uploader" \
  -d "In src/upload.rs, retry failed PUTs 3x with backoff. Add a unit test." \
  --effort low \
  --space-kind new-workspace --space-ref uploader --space-cwd /path/to/repo
board move <new-card-id> Execute        # Execute is an auto column -> run starts
```

Omitting `--harness` selects Pi; omitting `--model` lets Pi use its configured default. Pi effort
maps to thinking and Pi has no `--permission` mode. Use `--harness claude` explicitly for Claude.

Watch the run land in a herdr pane; the agent will comment and `board done`, and the daemon
moves the card along until it reaches a manual gate. Use `board card show <id>` to follow along.
