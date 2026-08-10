# herdr-board

![Rust](https://img.shields.io/badge/rust-edition%202021-orange.svg)
![herdr 0.8.0](https://img.shields.io/badge/herdr-0.8.0-8a2be2)
![board protocol v1 · schema v13](https://img.shields.io/badge/board-protocol%20v1%20%C2%B7%20schema%20v13-blue.svg)
![platforms: linux, macOS](https://img.shields.io/badge/platforms-linux%2C%20macOS-informational)

**Turn a kanban card into a real AI coding agent running in a visible Herdr pane.** Cards hold
prompts, columns define pipeline stages, and moving work across the board can plan, execute, review,
and stop at human gates automatically.

<p align="center">
  <img src="docs/assets/readme/board-overview.png" alt="Wide herdr-board view with boxed cards, colored statuses, a single-line header, and a persistent action rail" width="100%">
</p>

<details>
<summary><strong>Interface</strong></summary>

| Guided card creation | Card context and run history | Compact mobile view |
|:--:|:--:|:--:|
| <img src="docs/assets/readme/new-card.png" alt="New card form with agent settings and execution target" width="440"> | <img src="docs/assets/readme/card-detail.png" alt="Card detail sheet with runs and comments" width="440"> | <img src="docs/assets/readme/compact-board.png" alt="Compact mobile layout with touch-friendly controls" width="250"> |

The TUI is mobile-first: a compact, touch-friendly layout that fits phones and small terminals —
the same board, anywhere. Agents run in visible Herdr panes, one stable `card-<id>` tab per card.

<p align="center">
  <img src="docs/assets/readme/agent-panes.png" alt="Visible board agent panes labeled per card" width="100%">
</p>

</details>

## Why herdr-board?

- **Agents stay visible.** New runs open in a stable per-card `card-<id>` Herdr tab. A managed tab converges to exactly its one harness pane — the shell anchor is closed once the launch succeeds — while configured harnesses keep a persistent anchor because their child exits with the run.
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
- **Mobile-first TUI.** The responsive layout keeps the board usable on phones and small terminals,
  with touch-friendly navigation and controls.

## How it works

- A **card** contains a title, base prompt, harness/model/effort/permission settings, and a target
  Herdr session and workspace.
- A **column** can define a system prompt, automatic or manual triggering, timeout, and separate
  success/failure destinations.
- Moving a card into an automatic column queues a run: the daemon resolves the session, opens or
  reuses the workspace, and starts the agent in a visible pane.
- The agent reads card/run environment variables and uses the `board` CLI to comment and report its
  outcome; the daemon then applies the column transition — the next automatic stage or a manual gate.

```text
┌────────┐  ┌────────┐
│  TUI   │  │  CLI   │   interfaces — humans and agents
└───┬────┘  └───┬────┘
    └─────┬─────┘
       ┌──┴─────┐
       │ boardd │   daemon — queue, dispatch, lifecycle
       └───┬────┘
           │
       ┌───┴────┐
       │  herdr │   one visible pane per run
       └────────┘
```

`boardd` runs as a background daemon and orchestrates everything; the TUI and the CLI are the two
interfaces that talk to it. The CLI also exposes `board skill`, which prints the exact contract a
dispatched agent is held to. All board state lives under `~/.local/share/herdr-board/`; Herdr's own
state is never modified.

## Install

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.13.0
```

Open the board:

```bash
herdr plugin action invoke open-board --plugin herdr-board
```

An optional keybinding (`prefix+shift+k`) opens it from anywhere — see
[`docs/install.md`](docs/install.md).

### Supported harnesses

| Harness | Integration (precise status) | GitHub |
|---|---|---|
| **Pi** (default) | `herdr integration install pi` | [earendil-works/pi](https://github.com/earendil-works/pi) |
| **Claude Code** | `herdr integration install claude` | [anthropics/claude-code](https://github.com/anthropics/claude-code) |
| **Codex** | `herdr integration install codex` | [openai/codex](https://github.com/openai/codex) |
| **OpenCode** | `herdr integration install opencode` | [anomalyco/opencode](https://github.com/anomalyco/opencode) |

<details>
<summary><strong>Requirements and details</strong></summary>

- Requires exactly **Herdr 0.8.0 (socket protocol 19)**, Git, and a Rust toolchain with `cargo`;
  Linux and macOS are supported. The daemon rejects any other Herdr version or protocol before
  workspace discovery and pane launch.
- Board protocol **v1**, SQLite schema **v13** (`schema.sql`; upgrades via `board-core::db`).
- The installer copies the `board` CLI to `~/.local/bin` — make sure that directory is on your
  `PATH` (`HERDR_BOARD_CLI_INSTALL_DIR` overrides it before installing).
- Without the harness integration the board still dispatches and accepts `board done`, but runs in
  degraded mode without precise working/blocked/done signals.
- To open the board as a tab instead of an overlay:

  ```bash
  herdr plugin pane open --plugin herdr-board --entrypoint board --placement tab --focus
  ```

- Custom CLI directory, keybinding setup, harness integration, optional agent skill, and named
  sessions: [`docs/install.md`](docs/install.md).

</details>

## Quickstart

1. Open the board: `herdr plugin action invoke open-board --plugin herdr-board` (or your keybinding).
2. On an empty board press `T` to apply the example pipeline, or `N` to create your own columns.
3. Press `n` to create a card. Pi is selected by default; leave the model at `(default)` to use
   Pi's configured default, pick the thinking effort if needed, then select session and workspace.
4. Move the card into an automatic column with `m`, `H` / `L`, or drag-and-drop.
5. Watch the agent appear in its stable `card-<id>` tab. `Enter` opens card detail; the agent
   comments and calls `board done` when its stage finishes.

The same flow from the shell:

```bash
board card create --title "Add retry to the uploader" \
  -d "In src/upload.rs, retry failed PUTs 3x with backoff. Add a unit test." \
  --effort low \
  --space-kind new-workspace --space-ref uploader --space-cwd /path/to/repo
board move <new-card-id> Execute
```

Pi is the default built-in harness; Claude Code, Codex, and OpenCode are available with
`--harness <name>`, each keeping its own model, effort, and permission behavior — the full reference
is [`skill/SKILL.md`](skill/SKILL.md) (also printed by `board skill`).

### Everyday controls

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

## CLI at a glance

`board` is one binary with a nested canonical taxonomy. **[`skill/SKILL.md`](skill/SKILL.md) is the
full reference** — every flag, every return shape, and the agent lifecycle rules. `board skill`
prints those exact bytes, so a dispatched agent can read the contract it is held to.

`--board` and `--json` are **global**: both parse before or after the subcommand.

```bash
board --board <ID|PATH> card list --json
board card list --board <ID|PATH> --json     # identical
```

`--board` takes a stable board id or canonical scope path. Without it, board-aware commands use the
focused Git root (or canonical non-Git CWD; `BOARD_SCOPE_PATH` overrides both). Card-id operations
infer the card's own board; `card create`, `card list`, `column list`, and board commands use the
selected/current board.

| Noun | Verbs |
|---|---|
| `board board` | `list`, `show`, `open`, `rename` |
| `board template` | `apply <NAME>` |
| `board card` | `create` (alias `new`), `edit`, `show`, `list`, `move`, `archive`, `restore`, `delete` |
| `board card comment` | `add`, `show`, `edit`, `delete`, `history` |
| `board card run` | `done`, `confirm`, `cancel`, `retry`, `focus` |
| `board column` | `list`, `create`, `show`, `edit`, `reorder`, `delete` |
| `board harness` | `list`, `models`, `efforts`, `permissions` |
| `board space` / `board session` | `list` |
| `board tui` · `board daemon` · `board version` · `board skill` | see below |

Legacy top-level forms stay supported and re-dispatch into the nested handlers: `board comment`,
`board done`, `board move`, `board cancel`, `board retry`. `card new` and `--to-board` remain
aliases. Cross-board `move` should use `--destination-board`; the old global-`--board` fallback still
works but warns on stderr.

### Daemon

```text
board daemon start [--foreground]     # run boardd in this process
board daemon stop [--json]            # graceful stop -> {"stopped":bool,"was_running":bool}
board daemon status [--json]          # {version, db_path, herdr_connected, active_runs, queued_runs}
```

Bare `board daemon` still runs the daemon, and the historical `--foreground` / `--stop` flags still
work but are hidden from `--help`. `board version [--json]` never starts boardd and reports
`{cli_version, daemon_version}`, with `null`/`unavailable` when boardd is offline.

<details>
<summary><strong>JSON output, exit codes, configuration, and maintenance</strong></summary>

Successful `--json` goes to stdout; JSON errors go to stderr with stdout left empty, in the stable
envelope `{"error":{"code":N,"kind":"...","message":"...","details":...}}` (`kind` and `details` are
additive and may be absent). Errors from the daemon carry its protocol code; errors the CLI itself
raised carry `{"code":64,"kind":"cli"}`.

The exit status carries the same number, so scripts branch on `$?` instead of parsing stderr:

| Code | Meaning |
|---|---|
| `0` | Success |
| `1`–`5` | Daemon protocol code: bad request, not found, invalid state, Herdr unavailable, internal |
| `64` | The CLI itself refused — usage/parse error, declined confirmation, bad enum value, unresolvable column, missing `$BOARD_CARD_ID` (`EX_USAGE`) |
| `70` | Daemon reported a protocol code outside `1..=5`, clamped (`EX_SOFTWARE`) |

- [`docs/configuration.md`](docs/configuration.md) — `config.toml`, `[daemon]` settings,
  config-defined harnesses, and every environment variable;
- [`docs/operations.md`](docs/operations.md) — update, uninstall, and local-development
  source install.

</details>
