# herdr-board

![Rust](https://img.shields.io/badge/rust-edition%202021-orange.svg)
![herdr 0.7+](https://img.shields.io/badge/herdr-0.7%2B-8a2be2)
![platforms: linux, macOS](https://img.shields.io/badge/platforms-linux%2C%20macOS-informational)
![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)

**A kanban board that sits above herdr spaces: cards are prompts, columns are pipeline stages, and
moving a card dispatches a real AI coding agent into a visible herdr pane.** The board runs as an
overlay TUI you summon with a keypress; the daemon behind it queues runs, spawns agents, watches
their status, and walks each card down the pipeline until a human-gated column stops it.

```
Todo ──► Plan ──► Execute ──► Review ──► Human Review ──► Done
        (auto)    (auto)      (auto)     (manual gate)
        └─────────── example pipeline — columns are fully user-defined ──────────┘
```

- A **card** = title + description (the prompt) + harness/model/effort/permission-mode + a target
  herdr session and space.
- A **column** can carry a *system prompt* (prepended to the card prompt) and *auto-transition*
  rules (on success move to the next column, which may auto-trigger the next run). Columns are
  created/renamed/configured freely in the TUI (mouse or keyboard); a new board starts with only
  `Todo`.
- Agents report back by **commenting on the card** via the `board` CLI, and the daemon moves the
  card along the pipeline until a human-gated column stops it.

## Features

- **One binary, three roles.** The single `board` binary is the TUI, the daemon (boardd), and the
  CLI (subcommands the agents call). No separate services to install.
- **Runs land in visible herdr panes.** Agents run where you can watch them, in the workspace's
  `kanban` tab, tiled roughly square.
- **Responsive, status-rich board.** Visible columns always divide the full viewport; cards retain
  readable status colors, harness/model metadata, and a clear selected state. Archive finished cards
  without losing history, then cycle `ACTIVE` / `ALL` / `ARCHIVED` in the Herdr pane title. The
  board footer stays minimal (`? help`); forms and pickers size to their content.
- **Pipelines, not just a queue.** Per-column system prompts and success/fail transitions turn a
  board into a Plan → Execute → Review flow with human gates where you want them.
- **Session- and space-aware.** A single daemon drives every herdr session; a card resolves to its
  session's socket at dispatch and runs in an existing workspace or one the daemon opens for it.
- **Pluggable harnesses.** Claude Code is built in; config-defined harnesses (and codex/gemini/
  opencode later) plug in behind a `HarnessAdapter`.
- **Agent-legible.** Ships a Claude Code skill so a dispatched agent knows exactly how to comment,
  close its run, and (from an interactive session) queue new work.
- **Extension-owned state.** All state lives under `~/.local/share/herdr-board/`; herdr's own state
  is never touched.

## Components (one binary)

The single `board` binary is TUI, daemon, and CLI (subcommands). Crates: `board-core`
(state/protocol/engine), `board-daemon` (orchestration), `board-herdr` (herdr socket client),
`board-tui` (ratatui view), `board-cli` (the `board` binary).

| Role | What |
|---|---|
| `board daemon` (boardd) | Owns SQLite state, the run queue, and orchestration. Talks to herdr's socket to resolve/create workspaces, spawn agent panes, and watch status events. |
| `board tui` | The kanban board, run **inside a herdr overlay pane** as a plugin (`herdr-plugin.toml`). Talks to the daemon; auto-starts it if absent. |
| `board <verb>` (CLI) | Same binary; the verbs agents call from inside a run (`comment`, `done`, `move`, …). |

## Install

herdr-board is a herdr plugin distributed from source (topic: `herdr-plugin`). It requires
herdr 0.7+, Git, and a Rust toolchain with `cargo`, and supports Linux and macOS. Ensure
`~/.local/bin` is on your `PATH` (for example, add `export PATH="$HOME/.local/bin:$PATH"`
to your shell profile).

```bash
herdr plugin install nelsonPires5/herdr-board
```

herdr first shows an interactive trust preview of the plugin's build commands. After approval it
checks out the source, builds the release binary, registers the plugin, and copies the CLI to
`~/.local/bin/board` as a regular executable (not a symlink into Herdr's managed checkout). For a
noninteractive install after reviewing the manifest and scripts:

```bash
herdr plugin install nelsonPires5/herdr-board --yes
```

Set `HERDR_BOARD_CLI_INSTALL_DIR` to an absolute user bin directory before installing to override
`~/.local/bin`; the installed command is `<that-directory>/board`. The installer records the
installed binary's SHA-256 checksum in `<that-directory>/.herdr-board-cli-managed`. Updates require
that `board` remain a regular, non-symlink file with matching contents, and refuse to overwrite a
pre-existing or subsequently replaced command.

Open the board overlay with:

```bash
herdr plugin action invoke open-board --plugin herdr-board
```

If `overlay` placement is unavailable in your herdr, open it as a tab instead:

```bash
herdr plugin pane open --plugin herdr-board --entrypoint board --placement tab --focus
```

The `board` command is also now available directly for CLI operations.

Plugin installation deliberately does **not** edit `~/.config/herdr/config.toml` or copy the Claude
skill. To add a keybinding, put a command such as this in that config (do not reuse a herdr default;
`prefix+k` is `focus_pane_up`, so check `herdr --default-config`):

```toml
[[keys.command]]
key = "prefix+shift+k"
type = "shell"
command = "herdr plugin action invoke open-board --plugin herdr-board"
```

The repository's optional **agent skill** (`skill/SKILL.md`) teaches Claude Code sessions how to
comment and call `board done`, but is only copied automatically by the local-development installer
below.

For more precise Claude agent status (idle/working/blocked) and session refs, optionally install
herdr's integration; the board otherwise still works:

```bash
herdr integration install claude
```

**Named sessions**: herdr keeps a plugin registry *per session* (keybindings/config are global,
plugins are not). Run the GitHub install command once from each named session where you want the
plugin registered. A single boardd runs one board across **every** herdr session: a card carries a
`session` (the default session when unset), and the daemon resolves it to that session's socket at
dispatch (via `herdr session list`) to create/resolve the workspace and spawn the pane there. Use a
second stack with `BOARD_SOCKET`/`BOARD_DB` overrides only for a fully separate *board*.

### Uninstall

Herdr's plugin uninstall cannot remove the CLI copied outside its managed checkout. Remove the CLI
only if it is still the managed binary, then unregister the plugin:

```bash
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

If you used `HERDR_BOARD_CLI_INSTALL_DIR`, use the **same directory** for every plugin update and
for cleanup. Changing or omitting it later can leave a second installed copy behind. Uninstall the
plugin from each named session where it was registered.

### Local development / source install

For a checkout you plan to edit, clone the repository and use `scripts/install.sh` instead. It
builds and prints its proposed plugin link, skill copy, PATH symlink, and keybinding changes by
default; `--yes` applies those broader development-only changes. It is intentionally not called by
the GitHub plugin install flow.

```bash
git clone https://github.com/nelsonPires5/herdr-board
cd herdr-board
./scripts/install.sh                         # dry run
./scripts/install.sh --yes                   # apply, default key: prefix+shift+k
./scripts/install.sh --yes --key prefix+shift+b
```

## Quickstart

1. Run `herdr plugin action invoke open-board --plugin herdr-board` to open the overlay (or press
   the optional keybinding if you configured one).
2. On the empty board press `T` to apply the example pipeline (or `N` to add your own columns), then
   `n` to create a card — the guided form picks harness/model/effort/permission, session, and space.
3. Move the card into an `auto` column (`m` for the column picker, or drag it) — the move dispatches
   the agent into a herdr pane in the workspace's `kanban` tab.
4. Watch the run land; the agent comments and calls `board done`, and the daemon advances the card
   until it reaches a manual gate. Follow along with `Enter` (card detail) or `board card show <id>`.

Same flow from the shell:

```bash
board card new --title "Add retry to the uploader" \
  -d "In src/upload.rs, retry failed PUTs 3x with backoff. Add a unit test." \
  --harness claude --effort high \
  --space-kind new-workspace --space-ref uploader --space-cwd /path/to/repo
board move <new-card-id> Execute        # Execute is an auto column -> run starts
```

## Keybindings (the `?` overlay)

| Key | Action | | Key | Action |
|---|---|---|---|---|
| `←/→ h/l` | focus column | | `Enter` | card detail |
| `↑/↓ k/j` | focus card | | `T` | apply template (empty board only) |
| `n` | new card | | `r` | refresh board |
| `N` | new column | | `?` | this help |
| `e` | edit card | | `q / Esc` | back / quit |
| `E` | edit focused column | | **card detail** | |
| `a` | archive / restore card | | `a` | archive / restore card |
| `v` | active / all / archived view | | `e` | edit card |
| `d` | delete card | | `c` | add comment |
| `D` | delete column (move cards / refuse if running) | | | |
| `m` | move card (column picker) | | `f` / click title | popup / fullscreen |
| `H / L` | shove card left / right | | `Tab` | focus comments / runs |
| **forms** | | | `↑/↓ k/j` | scroll focused detail section |
| `Tab` / `Shift+Tab` | next / previous field | | `o` | jump to pane (stub) |
| `←/→ Space` | cycle a picker field | | `x` / `r` | cancel / retry run |
| `Ctrl+E` | edit textarea in `$EDITOR` | | **mouse** | |
| `Enter` / `Esc` | submit / cancel | | `click` / `dbl-click` | focus / open card detail |
| | | | `drag` / `wheel` | move card or column / scroll |

## CLI reference

```
board tui | daemon [--foreground] | status [--json]
board card new --title T [-d D] [--column C] [--harness H] [--model M] [--effort E] \
   [--permission P] [--session S] [--space-kind workspace|new-workspace] \
   [--space-ref R] [--space-cwd DIR]      # space-cwd required for new-workspace
board card show <ID> | card list [--column C] | card archive|restore <ID> | column list
board comment [CARD_ID] <BODY>            # CARD_ID defaults to $BOARD_CARD_ID
board done [CARD_ID] --outcome ok|fail [--summary S]
board move <CARD_ID> <COLUMN> | cancel <CARD_ID> | retry <CARD_ID>
board harness models [HARNESS] | efforts [HARNESS] --model M | permissions [HARNESS]
board space list [--session S] | session list    # HARNESS defaults to "claude"
```

`--json` is accepted everywhere. In the TUI, `d` permanently deletes a card after confirmation;
`D` deletes a column (asking where to move its cards, and refusing while a card is active).
Archiving is the safer reversible action: `a` in the TUI or `board card archive <ID>`; restore with
`a` from an archived view or `board card restore <ID>`. The active filter is shown as
`Board [ACTIVE|ALL|ARCHIVED]`. In card detail, blue selects comments/runs and directional arrows
appear only when history is hidden (counts are omitted). Agent lifecycle rules and examples live in
`skill/SKILL.md`.

## Configuration

**`~/.config/herdr-board/config.toml`** (override path via `HERDR_BOARD_CONFIG`):

```toml
[daemon]
spawner = "herdr"          # herdr = agent panes (default); local = plain child processes
max_concurrent = 3         # global cap on concurrent runs
idle_grace_seconds = 90    # idle-without-`board done` before a run is marked `lost`

[harness.myharness]        # config-defined harness; prompt via $BOARD_PROMPT
argv = ["mytool", "--model", "{model}"]   # {model}/{effort}/{permission_mode} placeholders
```

**Environment variables**

| Var | Purpose |
|---|---|
| `BOARD_DB` | SQLite path (daemon). Default `~/.local/share/herdr-board/board.db`. |
| `BOARD_SOCKET` | Daemon socket. Default `~/.local/share/herdr-board/boardd.sock`. |
| `HERDR_BOARD_CONFIG` | Config file path override. |
| `BOARD_SPAWNER` | `herdr` or `local` (overrides `[daemon] spawner`). |
| `BOARD_CARD_ID` / `BOARD_RUN_ID` | Injected into agent runs; `board comment`/`done` default to them. |
| `BOARD_PROMPT` / `BOARD_SYSTEM_PROMPT` | Prompt delivery for custom harnesses. |
| `BOARD_TIMEOUT_UNIT_SECS` / `BOARD_LOCAL_POLL_MS` / `BOARD_TICK_MS` | Test-tuning knobs. |

## Scripts

- `scripts/build.sh` — release build of the `board` binary (first plugin `[[build]]` step; idempotent).
- `scripts/install-cli.sh` — copy the built CLI to `~/.local/bin/board` (second plugin `[[build]]`
  step; override the directory with `HERDR_BOARD_CLI_INSTALL_DIR`).
- `scripts/install.sh` — local-development build + link + skill/keybinding setup (mutations guarded
  behind `--yes`; not called by GitHub plugin installation).
- `scripts/open-board.sh` — the `open-board` action launcher (open-or-focus, toggle off on repeat).
- `e2e/` — live end-to-end scenario suite against a REAL herdr; run `e2e/run-all.sh`
  (`scripts/e2e.sh` is a compat wrapper). See [`e2e/README.md`](e2e/README.md) and
  [`docs/testing.md`](docs/testing.md).
- `scripts/board-rpc.py` — raw boardd protocol client (e.g. `column.create`, which has no CLI verb).

## Architecture

- [`docs/`](docs/README.md) — the documentation index (start here).
- [`docs/design.md`](docs/design.md) — architecture, data model, column config, full data flow.
- [`docs/protocol.md`](docs/protocol.md) — the boardd socket protocol contract (single source of truth).
- [`docs/research.md`](docs/research.md) — herdr API capability map, prior art, verified harness flags.
- [`docs/releasing.md`](docs/releasing.md) — release policy and the Prepare Release → CI-gated publish flow.
- [`docs/implementation.md`](docs/implementation.md) — crate layout and build phases.
- [`docs/testing.md`](docs/testing.md) — the testing pyramid and the live e2e scenario suite.
- [`schema.sql`](schema.sql) — SQLite schema.

## Development

```bash
cargo test --workspace --all-features      # unit + integration tests (no live herdr needed)
cargo clippy --all-targets -- -D warnings   # no warnings
cargo fmt --all --check                     # formatted
./e2e/run-all.sh                            # live e2e scenario suite vs a REAL herdr (disposable workspaces)
```

Testing pyramid and how to add a test: [`docs/testing.md`](docs/testing.md). Contributing guide:
[`CONTRIBUTING.md`](CONTRIBUTING.md). Cross-agent contributor notes: [`AGENTS.md`](AGENTS.md).

## Status

**v1.** Rust (ratatui + rusqlite + tokio), one `board` binary. Harness: **Claude Code** builtin
(`claude`) plus config-defined harnesses, behind a `HarnessAdapter` so codex/gemini/opencode plug in
later. Execution: **visible herdr panes**. UI: **overlay TUI** summoned by a herdr keybinding
(`?` = help). DB is extension-owned (`~/.local/share/herdr-board/`); herdr's own state is untouched.
Core is fully tested.

## License

MIT (see `license` in `Cargo.toml`). No `LICENSE` file ships yet — add one to make the terms
explicit for users and the marketplace.
