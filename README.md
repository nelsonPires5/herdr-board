# herdr-board

![Rust](https://img.shields.io/badge/rust-edition%202021-orange.svg)
![herdr 0.7.5](https://img.shields.io/badge/herdr-0.7.5-8a2be2)
![platforms: linux, macOS](https://img.shields.io/badge/platforms-linux%2C%20macOS-informational)
![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)

**Turn a kanban card into a real AI coding agent running in a visible Herdr pane.** Cards hold
prompts, columns define pipeline stages, and moving work across the board can plan, execute, review,
and stop at human gates automatically.

<p align="center">
  <img src="docs/assets/readme/board-overview.png" alt="herdr-board showing a six-column pipeline with idle, running, queued, blocked, and failed cards" width="100%">
</p>

```text
Todo ──► Plan ──► Execute ──► Review ──► Human Review ──► Done
        (auto)    (auto)      (auto)     (manual gate)
```

Columns are fully user-defined. The pipeline above is an optional template; a new board starts with
only `Todo`.

## Why herdr-board?

- **Agents stay visible.** New runs open in a stable per-card `card-<id>` Herdr tab with a reserved
  shell anchor and one split child per stage/retry, so each card's panes stay together while they
  work; legacy runs retain the historical `kanban` tab.
- **Pipelines, not just a queue.** Each column can prepend a system prompt and route successful or
  failed runs to another stage.
- **Human gates where they matter.** Automatic stages keep moving; manual columns stop the pipeline
  for approval.
- **One board per project.** The focused pane's Git root selects an independent pipeline; non-Git
  directories use their canonical CWD. The preserved `Global` board remains available with `b`.
- **One binary.** `board` provides the TUI, daemon, and CLI used by both humans and agents.
- **Session- and workspace-aware.** One daemon can dispatch cards across every Herdr session and
  existing or newly created workspaces.
- **History stays with the card.** Comments, runs, outcomes, retries, and archived cards remain
  available for inspection.

## Install

Requires exactly **Herdr 0.7.5 (protocol 17)**, Git, and a Rust toolchain with `cargo`. Linux
and macOS are supported. Ensure `~/.local/bin` is on your `PATH`. The board protocol is v1 and the
current SQLite schema is v13; `schema.sql` defines fresh databases and the daemon applies tested
upgrades through `board-core::db`.

The daemon checks the selected Herdr socket before workspace discovery and pane launch. It rejects
any Herdr version other than 0.7.5 and any protocol other than 17; protocol 16 is not supported.

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.8.0
```

Precise live lifecycle status also requires Herdr's integration for the harness you dispatch (for
example, `herdr integration install pi`). Without that integration the board still dispatches and
accepts `board done`, but runs in degraded mode without precise working/blocked/done signals.

Open the board:

```bash
herdr plugin action invoke open-board --plugin herdr-board
```

To open the board as a tab instead of an overlay:

```bash
herdr plugin pane open --plugin herdr-board --entrypoint board --placement tab --focus
```

<details>
<summary><strong>Installation details and custom CLI directory</strong></summary>

Herdr first shows an interactive trust preview of the plugin's build commands. After approval it
checks out the source, builds the release binary, registers the plugin, and copies the CLI to
`~/.local/bin/board` as a regular executable. After reviewing the manifest and scripts, a
noninteractive install is available:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.8.0 --yes
```

Set `HERDR_BOARD_CLI_INSTALL_DIR` to an absolute user bin directory before installing to override
`~/.local/bin`; the installed command is `<that-directory>/board`. The installer records the
binary's SHA-256 checksum in `<that-directory>/.herdr-board-cli-managed`. Updates only overwrite a
regular, non-symlink `board` whose contents still match that marker.

</details>

## Quickstart

1. Open the board with the plugin action or an optional keybinding. Herdr's focused pane selects
   its Git-root/CWD board; press `b` to switch boards or open the preserved `Global` board.
2. On an empty board press `T` to apply the example pipeline, or `N` to create your own columns.
3. Press `n` to create a card. Pi is selected by default. Leave model at `(default)` to use Pi's
   configured default, choose thinking effort if needed, then select the session and workspace.
   Permission appears only for harnesses that support it (Pi does not).
4. Move the card into an automatic column with `m`, `H` / `L`, or drag-and-drop.
5. Watch the agent appear in its stable `card-<id>` workspace tab. Follow progress with `Enter`
   for card detail; the agent comments and calls `board done` when its stage finishes.

The same flow from the shell:

```bash
board card create --title "Add retry to the uploader" \
  -d "In src/upload.rs, retry failed PUTs 3x with backoff. Add a unit test." \
  --effort low \
  --space-kind new-workspace --space-ref uploader --space-cwd /path/to/repo
board move <new-card-id> Execute
```

`pi` is the default built-in harness. An omitted model lets Pi use its current configured default;
an explicit model uses Pi's `provider/model` form. Board effort maps to Pi `--thinking`. Pi has no
board permission mode and rejects `--permission`. Claude remains available explicitly with
`--harness claude` and keeps its model/effort/permission behavior.

## How it works

- A **card** contains a title, base prompt, harness/model/effort/permission settings, and a target
  Herdr session and workspace.
- A **column** can define a system prompt, automatic or manual triggering, timeout, and separate
  success/failure destinations.
- Moving a card into an automatic column queues a run. The daemon resolves the target session,
  opens or reuses the workspace, and starts the agent in a visible pane.
- The agent receives card/run environment variables and uses the `board` CLI to comment and report
  its outcome.
- The daemon applies the column transition. It either dispatches the next automatic stage or stops
  at a manual gate.

The CLI and TUI share typed `board-core::client::BoardClient` wrappers; only boardd owns SQLite.
`RootConfig` parses the complete TOML document once, including typed `[daemon]` settings, then applies
process-environment overrides. Runtime launch ownership stays in `board-daemon`: it consumes the
versioned neutral launch spec, owns Herdr pane placement and process handles, and runs an always-on,
per-session supervisor that reconnects conservatively after outages. Board snapshots include an
additive active-run summary, so the TUI timer follows the open run's `started_at` rather than card
activity timestamps.

All board state lives under `~/.local/share/herdr-board/`; Herdr's own state is never modified.

## A closer look

| Guided card creation | Card context and run history |
|:--:|:--:|
| <img src="docs/assets/readme/new-card.png" alt="New card form with harness, model, effort, permission, session, and workspace fields" width="820"> | <img src="docs/assets/readme/card-detail.png" alt="Card detail popup showing status, description, comments, and run history" width="820"> |

### Agents run in visible Herdr panes

The daemon creates one stable `card-<id>` tab per new card, reserves its root as a labeled
`card-<id>-anchor` shell, and splits every agent/configured run into a child from that anchor. It
never adopts a user tab solely because its label matches; after exact tab proof, a missing anchor
is recovered only from a durable board pane, otherwise a fresh tab is created. Legacy runs retain
`kanban`.

<p align="center">
  <img src="docs/assets/readme/agent-panes.png" alt="Herdr workspace with board agents in separate per-card tabs" width="100%">
</p>

## Everyday controls

| Key | Action | Key | Action |
|---|---|---|---|
| `←/→` or `h/l` | Focus column | `↑/↓` or `k/j` | Focus card |
| `b` | Switch project/Global board | `n` | New card |
| `N` | New column | `Enter` | Card detail |
| `m` | Move card picker | `o` (detail) | Jump to selected run's pane |
| `H / L` | Move card left/right | `a` | Archive/restore card |
| `e` | Edit card | `E` | Edit column |
| `v` | Active/all/archived view | `?` | Full help overlay |
| `q / Esc` | Back/quit | mouse drag | Move card/reorder column |

<details>
<summary><strong>Full keyboard and mouse reference</strong></summary>

| Key | Action | | Key | Action |
|---|---|---|---|---|
| `←/→ h/l` | focus column | | `Enter` | card detail |
| `↑/↓ k/j` | focus card | | `T` | apply template (empty board only) |
| `b` | switch board | | `r` | refresh board |
| `n` | new card | | `?` | help |
| `N` | new column | | `q / Esc` | back / quit |
| `e` | edit card | | **card detail** | |
| `E` | edit focused column | | `e` | edit card | |
| `a` | archive / restore card | | `a` | archive / restore card |
| `v` | active / all / archived view | | `f` / click title | popup / fullscreen |
| `d` | delete card | | `c` | add comment |
| `D` | delete column | | `Tab` | focus comments / runs |
| `m` | move card picker | | `↑/↓ k/j` | select comment / run (section follows) |
| `H / L` | move card left / right | | `o` | jump to the selected run's same-session pane |
| | | | `Enter` | confirm done (`awaiting` card) |
| **forms** | | | `x` / `r` | cancel / retry run |
| `Tab` / `Shift+Tab` | next / previous field | | `Tab`, `↑/↓` | choose/scroll detail history |
| `←/→ Space` | cycle a picker field | | **mouse** | |
| `Ctrl+E` | edit textarea in `$EDITOR` | | click / double-click | focus / open card detail |
| `Enter` / `Esc` | submit / cancel | | drag / wheel | move or scroll |

</details>

## Integration and optional setup

<details>
<summary><strong>Add a Herdr keybinding</strong></summary>

Plugin installation deliberately does not edit `~/.config/herdr/config.toml`. Add a command such as
this yourself (do not reuse a Herdr default; `prefix+k` is `focus_pane_up`, so check
`herdr --default-config`):

```toml
[[keys.command]]
key = "prefix+shift+k"
type = "shell"
command = "herdr plugin action invoke open-board --plugin herdr-board"
```

</details>

<details>
<summary><strong>Install the harness integration and optional agent skill</strong></summary>

For precise Pi status (`idle`, `working`, `blocked`, `done`) and session references, install Herdr's
Pi integration. Installation changes your personal Pi extension config, so herdr-board never does it
automatically — it is a user prerequisite. Without it (degraded mode), spawn, explicit `board done`,
timeout, and pane-exit handling still work, but Herdr's `working`/`blocked`/`done` signals do not
exist and a card can only reach `awaiting` (pending review) via the idle grace path.

```bash
herdr integration install pi
```

Claude users can similarly run `herdr integration install claude`. The repository's optional
[`skill/SKILL.md`](skill/SKILL.md) teaches interactive or dispatched agents to comment, call
`board done`, and queue work. GitHub plugin installation does not copy the skill; the
local-development installer below can do so.

</details>

<details>
<summary><strong>Use named Herdr sessions</strong></summary>

Herdr keeps a plugin registry per session, while keybindings/configuration are global. Run the
GitHub install command once from every named session where the plugin should be registered.

A single board daemon serves every scoped board across every Herdr session. Each card carries a
`session` (the default session when unset), and dispatch resolves that session's socket through
`herdr session list`. Use `BOARD_SOCKET` and `BOARD_DB` overrides only when you want a completely
separate board stack.

</details>

## CLI reference

<details>
<summary><strong>Canonical commands, selectors, and JSON</strong></summary>

`board` is one binary with a nested canonical taxonomy. Put the global selector before any command:

```bash
board --board <ID|PATH> card list --json
```

`--board` accepts a stable board id or canonical scope path. Without it, board-aware commands use the
focused Git root (or canonical non-Git CWD; `BOARD_SCOPE_PATH` overrides both). Card-id operations
(`show`, `edit`, `delete`, comments, and runs) infer the card's own board; `card create`, `card list`,
`column list`, and board commands use the selected/current board.

### Boards, templates, and cards

```text
board board list [--json]
board board show [ID|PATH] [--json]
board board open <PATH> [--json]
board board rename [ID|PATH] <NAME> [--json]
board template apply pipeline [--json]

board card create --title T [-d DESC] [--column C] [--harness H] [--model M]
  [--effort E] [--permission P] [--session S]
  [--space-kind workspace|new-workspace] [--space-ref R] [--space-cwd DIR] [--json]
board card edit <ID> [--title T] [-d DESC] [--clear-description]
  [--harness H] [--model M|--clear-model] [--effort E|--clear-effort]
  [--permission P|--clear-permission] [--session S|--clear-session]
  [--space-ref R|--clear-space-ref] [--space-cwd DIR|--clear-space-cwd] [--json]
board card show <ID> [--json]
board card list [--column C] [--visibility active|all|archived] [--json]
board card move <ID> <COLUMN> [--destination-board ID|PATH] [--json]
board card archive|restore <ID> [--json]
board card delete <ID> [--yes] [--json]
```

`card create` is the canonical spelling; `card new` remains an alias. A new card defaults to Pi and
an omitted model/effort uses the harness default. `new-workspace` requires both `--space-ref` and
`--space-cwd`; creating directly in an `auto` column dispatches a run. `card list` defaults to active
cards; `all` includes archived cards and `archived` returns only archived cards. JSON list commands
return arrays; `board show`/`open` return a snapshot `{board, columns, cards, active_runs}`; card
show returns `{card, comments, runs}`. Card mutations return a card, except delete returns
`{"deleted":true}`.

Edits leave omitted fields unchanged. Use the explicit `--clear-*` flags to clear nullable values
(`--clear-description` sets the description to an empty string). Harness cannot be cleared. Delete
is permanent and removes history; it requires `--yes` when stdin is not a TTY, otherwise it prompts
and cancels unless the answer is `y`/`yes`. Cards with an open run must be cancelled before edit of
run settings, archive, or delete.

### Comments and runs

```text
board card comment add <CARD_ID> <BODY> [--json]
board card comment show <COMMENT_ID> [--json]
board card comment edit <COMMENT_ID> <BODY> [--json]
board card comment delete <COMMENT_ID> [--yes] [--json]
board card comment history <COMMENT_ID> [--json]

board card run done [CARD_ID] --outcome ok|fail [--summary S] [--json]
board card run confirm [CARD_ID] [--summary S] [--json]
board card run cancel <CARD_ID> [--json]
board card run retry <CARD_ID> [--json]
board card run focus <CARD_ID> <RUN_ID> [--origin-socket SOCKET] [--json]
```

Comment add returns the compact current comment (`id`, `card_id`, `author`, `body`,
`created_at`); show/edit return the current record with those fields plus `deleted_at`; delete
returns `{deleted:true}`; history is an array of immutable snapshots. Ordinary card detail hides soft-deleted comments. A comment created
inside a durable run is owned by `agent:<BOARD_RUN_ID>`; with `BOARD_RUN_ID` set, that run may
edit/delete only its own comment while the run is open. Human calls without it may edit/delete
non-system comments. System comments and deleted comments are immutable. `card run confirm` is the
`done --outcome ok` channel for awaiting review.
Focus returns `{action, recorded_pane_id?, run_id, card_id, column_id, harness, session, session_id,
pane_id}` and requires the target run to belong to the current Herdr session;
without `--origin-socket`, it uses `HERDR_SOCKET_PATH` or `HERDR_SOCK`. If that run's pane was
closed, focus **reopens** it: the harness conversation is resumed in a new pane in the card's tab
(`action` = `rescued`, or `focused_rescued_pane` when an earlier reopen's pane is still alive), the
card task is not re-sent, and nothing is written to the database — so that pane is ephemeral and not
tracked as a run. A harness that cannot resume, or a run with no recorded conversation id, is
refused explicitly; use `card run retry` to start a new run instead. A reopened pane can still
`board comment` on the card (as a human comment), but `board done` does not apply to it — the run it
continues is already closed and stays that way.

### Columns and discovery

```text
board column list [--json]
board column create --name NAME [--prompt TEXT] [--trigger manual|auto]
  [--on-success COLUMN] [--on-fail COLUMN] [--fresh-session|--reuse-session]
  [--harness H] [--model M] [--effort E] [--permission P]
  [--timeout MINUTES] [--position N] [--json]
board column show <ID|NAME> [--json]
board column edit <ID|NAME> [--name NAME] [--prompt TEXT|--clear-prompt]
  [--trigger manual|auto] [--on-success COLUMN|--clear-on-success]
  [--on-fail COLUMN|--clear-on-fail] [--fresh-session|--reuse-session]
  [--harness H|--clear-harness] [--model M|--clear-model]
  [--effort E|--clear-effort] [--permission P|--clear-permission]
  [--timeout MINUTES|--clear-timeout] [--json]
board column reorder <ID|NAME> <ZERO-BASED-POSITION> [--json]
board column delete <ID|NAME> [--move-cards-to COLUMN] [--yes] [--json]
board harness list|models|efforts|permissions [--json]
board space list [--session S] [--json]
board session list [--json]
```

Column list/reorder JSON is an ordered array; create/show/edit JSON is a column. Column transition
references are names (case-insensitive) or ids. `--fresh-session` and `--reuse-session` are
mutually exclusive. A column with cards requires `--move-cards-to`; any open card run blocks column
deletion. `template apply pipeline` is atomic and only works on a board containing exactly its seed
`Todo` column and no cards; it returns the resulting column array.

### Daemon, version, skill, and compatibility aliases

```text
board daemon [--foreground] [--stop]
board daemon status [--json]
board version [--json]
board skill
```

`daemon status` is the operational probe and returns `{version, db_path, herdr_connected, active_runs,
queued_runs}`. `version` never starts boardd: it reports `{cli_version, daemon_version}`, where the
daemon value is `null`/`unavailable` when boardd is offline. `skill` prints the exact checked-in
`skill/SKILL.md` bytes, not a JSON envelope.

The older top-level action forms remain supported: `board comment [CARD_ID] BODY` (card id defaults
to `$BOARD_CARD_ID`), `board done`, `board move`, `board cancel`, and `board retry`. Only
`board daemon status [--json]` is supported for daemon status. The nested forms above are canonical;
`card new` and `--to-board` remain accepted aliases.

Every command accepts `--json` where shown. Successful JSON is written to stdout. JSON errors are
written to stderr with no stdout output and keep the stable envelope
`{"error":{"code":N,"kind":"...","message":"...","details":...}}`; additive `kind` and
`details` may be omitted. Codes are 1 bad request, 2 not found/CLI error, 3 invalid state, 4 Herdr
unavailable, and 5 internal error.

The pane title combines scope and filter, for example `Board [my-repo · ACTIVE]`. In card detail,
`o` focuses the latest recorded run pane only when it belongs to the current Herdr session; errors
leave the overlay open. Agent lifecycle rules and examples live
in [`skill/SKILL.md`](skill/SKILL.md).

</details>

## Configuration

<details>
<summary><strong>Configure the daemon and custom harnesses</strong></summary>

Configuration lives at `~/.config/herdr-board/config.toml`; override it with
`HERDR_BOARD_CONFIG`.

```toml
max_concurrent = 3         # global cap on concurrent runs
idle_grace_seconds = 90    # idle without board done before the card is parked in `awaiting` for review

[daemon]
spawner = "herdr"          # herdr = agent panes (default); local = child processes
timeout_unit_secs = 60      # seconds per column timeout_minutes unit
tick_ms = 1000              # timeout/idle watcher interval
local_poll_ms = 2000        # local-spawner liveness interval

[harness.myharness]
argv = ["mytool", "--model", "{model}"]
resume = false             # can this harness resume a recorded conversation? default false
```

Custom harness prompts are delivered through `$BOARD_PROMPT`. The placeholders `{model}`, `{effort}`,
and `{permission_mode}` are available in `argv`. Optional keys `models`, `efforts`, and
`permission_modes` declare the harness's capability catalog.

`resume` declares whether this harness can re-attach to a conversation it recorded earlier, which is
what lets `board card run focus` **reopen a run whose pane was closed** (see
[`docs/protocol.md`](docs/protocol.md) → `run.focus`). It **defaults to `false`**: there is no
universal CLI syntax for resuming, so herdr-board never guesses one — the built-ins `pi`
(`--session-id <id>`) and `claude` (`--resume <id>`) declare it themselves. Setting `resume = true`
promises that your `argv` re-attaches to the conversation named by `$BOARD_RESUME_SESSION_ID`, which
the daemon sets on the reopened pane along with `BOARD_RESCUE=1` (the run's argv is persisted fully
materialized, so there is no placeholder left to substitute). Without it, focusing such a run is
refused explicitly rather than starting a fresh conversation that would re-run the task.

The daemon parses the complete document once, including `[daemon]`, into typed settings. A missing
file or omitted section uses the defaults shown above. An existing file with malformed TOML or an
invalid typed value (including an unknown `spawner`) is an error: the daemon does not silently fall
back to defaults. Environment overrides are applied after parsing and take precedence; malformed
override values also prevent daemon startup.

### Environment variables

| Variable | Purpose |
|---|---|
| `BOARD_DB` | SQLite path. Default: `~/.local/share/herdr-board/board.db`. |
| `BOARD_SOCKET` | Daemon socket. Default: `~/.local/share/herdr-board/boardd.sock`. |
| `HERDR_BOARD_CONFIG` | Configuration path override. |
| `BOARD_SCOPE_PATH` | Canonicalizable scope override for CLI/TUI automation. |
| `BOARD_SPAWNER` | `herdr` or `local`; overrides `[daemon] spawner`. |
| `BOARD_CARD_ID` / `BOARD_RUN_ID` | Injected into runs; `comment`/`done` use them by default. |
| `BOARD_PROMPT` / `BOARD_SYSTEM_PROMPT` | Prompt delivery for custom harnesses. |
| `BOARD_RESCUE` / `BOARD_RESUME_SESSION_ID` / `BOARD_RESCUED_RUN_ID` | Set on a *reopened* run pane only: marks it as an ephemeral rescue (not a tracked run), names the conversation to resume, and labels which run it continues. A reopened pane gets `BOARD_CARD_ID`/`BOARD_SOCKET`/`BOARD_BIN` but **never** `BOARD_RUN_ID` — that is the actor credential for `comment`/`done`, and a rescued pane must not be able to write to the finished run. |
| `BOARD_TIMEOUT_UNIT_SECS` / `BOARD_LOCAL_POLL_MS` / `BOARD_TICK_MS` | Test-tuning knobs. |

</details>

## Maintenance

<details>
<summary><strong>Update</strong></summary>

Re-run the install command to update — Herdr has no separate update command, so reinstall over the
existing plugin:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.8.0 --yes
```

The build step requests a graceful stop (`board daemon --stop`) before recompiling, so the new
binary replaces a stopped process instead of overwriting one the old daemon still has mapped in
memory. The command succeeds only after the daemon listener disappears. Stop failures and timeouts
are non-zero and preserve the socket; stale-socket cleanup is only performed after a fresh failed
connect and an identity check. The next `board` command auto-starts a fresh daemon from the new
binary.

Run the install once from each named Herdr session where the plugin is registered.

If you are updating from a version older than the `--stop` flag and a stale daemon is still
serving the old code, use your platform's process manager to stop that specific board process
(after verifying its PID and command) before reinstalling. Do not remove the socket or use a broad
process-name kill.

</details>

<details>
<summary><strong>Uninstall</strong></summary>

Herdr's plugin uninstall has no lifecycle hook and does not stop the board daemon — boardd is a
detached process Herdr does not track, so uninstalling the plugin leaves it running (and, after a
reinstall, serving stale code). Stop it first, then remove the CLI Herdr can't manage (only when
its checksum still matches the managed marker), then unregister the plugin:

```bash
if ! board daemon --stop; then
  echo "board daemon did not stop safely; socket preserved" >&2
  exit 1
fi
(
  if [ "${HERDR_BOARD_CLI_INSTALL_DIR+x}" = x ]; then
    install_dir="$HERDR_BOARD_CLI_INSTALL_DIR"
  else
    install_dir="${HOME:?HOME must be set}/.local/bin"
  fi
  case "$install_dir" in /*) ;; *) echo "Install directory must be absolute" >&2; exit 1;; esac

  board="$install_dir/board"
  marker="$install_dir/.herdr-board-cli-managed"
  prefix="herdr-board install-cli.sh managed board sha256:"
  if [ -f "$board" ] && [ ! -L "$board" ] && [ -f "$marker" ] && [ ! -L "$marker" ]; then
    checksum=""
    if command -v sha256sum >/dev/null 2>&1; then
      checksum_output="$(sha256sum <"$board")" && checksum="${checksum_output%% *}"
    elif command -v shasum >/dev/null 2>&1; then
      checksum_output="$(shasum -a 256 <"$board")" && checksum="${checksum_output%% *}"
    fi
    if [[ "$checksum" =~ ^[0-9a-f]{64}$ ]] && printf '%s\n' "$prefix$checksum" | cmp -s - "$marker"; then
      rm -- "$board" "$marker"
    else
      echo "board CLI was changed or is unrecognized; retaining $board and $marker" >&2
    fi
  else
    echo "board CLI was changed or is unrecognized; retaining $board and $marker" >&2
  fi
)
herdr plugin uninstall herdr-board
```

If `HERDR_BOARD_CLI_INSTALL_DIR` was used, use the same directory for every update and cleanup.
Uninstall the plugin from each named session where it was registered.

To remove all board data (cards, columns, runs), delete the data directory — `BOARD_DB`'s default
(`~/Library/Application Support/herdr-board` on macOS, `~/.local/share/herdr-board` on Linux).
This is optional and never needed for a normal reinstall.

</details>

<details>
<summary><strong>Local development / source install</strong></summary>

For a checkout you plan to edit, use `scripts/install.sh`. It prints proposed plugin links, skill
copies, PATH symlinks, and keybinding changes by default; `--yes` applies them.

```bash
git clone https://github.com/nelsonPires5/herdr-board
cd herdr-board
./scripts/install.sh                         # dry run
./scripts/install.sh --yes                   # default key: prefix+shift+k
./scripts/install.sh --yes --key prefix+shift+b
```

This broader development installer is intentionally separate from the GitHub plugin install flow.

</details>

## Architecture and development

<details>
<summary><strong>Components, documentation, scripts, and test commands</strong></summary>

### Components

| Role | Responsibility |
|---|---|
| `board daemon` | Owns SQLite state, run queue, orchestration, workspace resolution, pane spawning, and status watching. |
| `board tui` | Ratatui board opened inside a Herdr overlay/tab; talks to and auto-starts the daemon. |
| `board <verb>` | CLI used by humans and dispatched agents (`comment`, `done`, `move`, and others). |

Workspace crates:

- `board-core`: models, protocol, database, engine, prompts, config, and harness adapters;
- `board-daemon`: orchestration and dispatch;
- `board-herdr`: Herdr socket client;
- `board-tui`: Ratatui application;
- `board-cli`: the `board` binary.

The CLI and TUI share `board_core::client::BoardClient`: typed wrappers own method names,
wire parameters, and response decoding for board, harness, space, session, and run actions.
The Unix-socket transport retains only the raw request primitive; production clients do not access SQLite.

### Documentation

- [`docs/README.md`](docs/README.md) — documentation index;
- [`docs/design.md`](docs/design.md) — architecture and full data flow;
- [`docs/protocol.md`](docs/protocol.md) — daemon socket protocol;
- [`docs/herdr.md`](docs/herdr.md) — verified Herdr commands and API facts;
- [`docs/research.md`](docs/research.md) — capability map, prior art, and harness flags;
- [`docs/implementation.md`](docs/implementation.md) — crate layout and build phases;
- [`docs/testing.md`](docs/testing.md) — testing pyramid and live scenarios;
- [`docs/releasing.md`](docs/releasing.md) — release policy;
- [`schema.sql`](schema.sql) — SQLite migration source of truth.

### Scripts

- `scripts/build.sh` — release build used by plugin installation;
- `scripts/install-cli.sh` — managed CLI copy;
- `scripts/install.sh` — local-development setup;
- `scripts/open-board.sh` — open-or-focus plugin action;
- `scripts/board-rpc.py` — raw daemon protocol client;
- `e2e/` — scenarios 01–27 against disposable Herdr sessions/workspaces; checked-in fake Pi,
  Claude, and configured harnesses keep the standard suite provider-free. `e2e/test-harness.sh`
  performs static ownership/safety checks without starting Herdr; the live suite is a separate gate.

### Test gates

```bash
cargo test --workspace --all-features
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
./e2e/run-all.sh
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`AGENTS.md`](AGENTS.md) before contributing.

</details>

## Status

**v1 board protocol / schema v13.** Rust with Ratatui, Rusqlite, and Tokio. Pi is the default
built-in harness and Claude Code remains explicitly selectable; config-defined harnesses are also
supported. Execution happens in visible Herdr panes, and extension-owned state remains separate from
Herdr's state. See [`docs/README.md`](docs/README.md) for version, source-ownership, and test-gate
links.

## License

MIT — see the `license` field in [`Cargo.toml`](Cargo.toml).
