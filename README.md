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

- **Agents stay visible.** New runs open in a stable per-card `card-<id>` Herdr tab, one split child
  per stage/retry, so each card's panes stay together while they work.
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
herdr plugin install nelsonPires5/herdr-board --ref v0.10.0
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

For a custom CLI install directory, a Herdr keybinding, the harness integration, the optional
agent skill, and named-session notes, see [`docs/install.md`](docs/install.md).

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

Runtime launch ownership stays in `board-daemon`: it owns Herdr pane placement and process handles,
and runs an always-on per-session supervisor that reconnects conservatively after outages.

All board state lives under `~/.local/share/herdr-board/`; Herdr's own state is never modified.

## A closer look

| Guided card creation | Card context and run history |
|:--:|:--:|
| <img src="docs/assets/readme/new-card.png" alt="New card form with harness, model, effort, permission, session, and workspace fields" width="820"> | <img src="docs/assets/readme/card-detail.png" alt="Card detail popup showing status, description, comments, and run history" width="820"> |

### Agents run in visible Herdr panes

The daemon creates one stable `card-<id>` tab per new card, reserves its root as a labeled
`card-<id>-anchor` shell, and splits every run into a child from that anchor. Ownership is proven
from durable pane identity, never from a matching label, so a user tab is never adopted. The full
placement and recovery rules are in [`docs/design.md`](docs/design.md).

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

### JSON and exit codes

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

## Configuration and maintenance

- [`docs/configuration.md`](docs/configuration.md) — `config.toml`, `[daemon]` settings,
  config-defined harnesses, and every environment variable;
- [`docs/operations.md`](docs/operations.md) — update, uninstall, and local-development
  source install.

## Architecture and development

| Role | Responsibility |
|---|---|
| `board daemon` | Owns SQLite state, run queue, orchestration, workspace resolution, pane spawning, and status watching. |
| `board tui` | Ratatui board opened inside a Herdr overlay/tab; talks to and auto-starts the daemon. |
| `board <verb>` | CLI used by humans and dispatched agents (`comment`, `done`, `move`, and others). |

Five workspace crates — `board-core` (models, protocol, database, engine, prompts, config, harness
adapters), `board-daemon` (orchestration and dispatch), `board-herdr` (Herdr socket client),
`board-tui` (Ratatui application), and `board-cli` (the `board` binary). The CLI and TUI share the
typed `board_core::client::BoardClient`; only boardd touches SQLite.

- **[`docs/README.md`](docs/README.md) — the documentation index** (design, protocol, herdr facts,
  implementation, testing, releasing) and the single maintained copy of the
  [test gates](docs/README.md#test-gates-single-source);
- [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`AGENTS.md`](AGENTS.md) — read before contributing;
- [`schema.sql`](schema.sql) — SQLite migration source of truth;
- `scripts/` — `build.sh` (release build used by plugin installation), `install-cli.sh` (managed
  CLI copy), `install.sh` (local-development setup), `open-board.sh` (open-or-focus plugin
  action), and `board-rpc.py` (raw daemon protocol client);
- `e2e/` — scenarios 01–29 against disposable Herdr sessions/workspaces; checked-in fake Pi,
  Claude, and configured harnesses keep the standard suite provider-free. `e2e/test-harness.sh`
  performs static ownership/safety checks without starting Herdr; the live suite is a separate
  gate. The catalog is [`e2e/README.md`](e2e/README.md).

## Status

**v1 board protocol / schema v13 / Herdr 0.7.5 (protocol 17).** Rust with Ratatui, Rusqlite, and
Tokio. See [`docs/README.md`](docs/README.md) for the full version and source-ownership matrix.

## License

MIT — see the `license` field in [`Cargo.toml`](Cargo.toml).
