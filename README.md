# herdr-board

Kanban board that sits **above** herdr spaces: cards are prompts, columns are pipeline stages, and moving a card dispatches a real AI coding agent into a visible herdr pane.

```
Todo ──► Plan ──► Execute ──► Review ──► Human Review ──► Done
        (auto)    (auto)      (auto)     (manual gate)
        └─────────── example pipeline — columns are fully user-defined ──────────┘
```

- A **card** = title + description (the prompt) + harness/model/effort/permission-mode + target space.
- A **column** can carry a *system prompt* (prepended to the card prompt) and *auto-transition* rules (on success move card to next column, which may auto-trigger the next agent run). Columns are created/renamed/configured freely in the TUI (mouse or keyboard); a new board starts with only `Todo`.
- Agents report back by **commenting on the card** via the `board` CLI, and the daemon moves the card along the pipeline until a human-gated column stops it.

## Components (one binary)

The single `board` binary is TUI, daemon, and CLI (subcommands). Crates: `board-core` (state/protocol/engine), `board-daemon` (orchestration), `board-herdr` (herdr socket client), `board-tui` (ratatui view), `board-cli` (the `board` binary).

| Role | What |
|---|---|
| `board daemon` (boardd) | Owns SQLite state, the run queue, and orchestration. Talks to herdr's socket to create workspaces/worktrees, spawn agent panes, and watch status events. |
| `board tui` | The kanban board, run **inside a herdr overlay pane** as a plugin (`herdr-plugin.toml`). Talks to the daemon; auto-starts it if absent. |
| `board <verb>` (CLI) | Same binary; the verbs agents call from inside a run (`comment`, `done`, `move`, …). |

## Quick start

```bash
# 1. Build the release binary.
./scripts/build.sh                       # -> target/release/board

# 2. Install: link the plugin + copy the agent skill (prints the exact commands).
./scripts/install.sh                     # dry run: prints mutating steps
./scripts/install.sh --yes               # actually links plugin + copies skill

# 3. Bind a key to summon the board (add to ~/.config/herdr/config.toml):
#    [[keys.command]]
#    key = "prefix+shift+k"
#    type = "shell"
#    command = "herdr plugin action invoke open-board --plugin herdr-board"

# 4. Recommended — precise agent status (idle/working/blocked) + session refs:
herdr integration install claude
```

If `overlay` placement is unavailable in your herdr, open the board on demand with:
`herdr plugin pane open --plugin herdr-board --entrypoint board --placement overlay --focus`.

**Named sessions**: herdr keeps a plugin registry *per session* (keybindings/config are global, plugins are not). In each named session run once, from inside it:
`herdr plugin link /path/to/herdr-board`. Also note the boardd daemon binds the herdr
session it starts under — for a fully separate session, run a second stack with
`BOARD_SOCKET`/`BOARD_DB` overrides. Don't bind keys herdr already uses by default
(`prefix+k` is `focus_pane_up`; check `herdr --default-config`).

The **agent skill** (`skill/SKILL.md`, copied to `~/.claude/skills/herdr-board/` by `install.sh`) teaches Claude Code sessions how to drive the board from inside a run.

## Keybindings (the `?` overlay)

| Key | Action | | Key | Action |
|---|---|---|---|---|
| `←/→ h/l` | focus column | | `Enter` | card detail |
| `↑/↓ k/j` | focus card | | `T` | apply template (empty board only) |
| `n` | new card | | `r` | refresh board |
| `N` | new column | | `?` | this help |
| `e` | edit card | | `q / Esc` | back / quit |
| `E` | edit focused column | | **card detail** | |
| `d` | delete card | | `c` | add comment |
| `D` | delete column (move cards / refuse if running) | | `o` | jump to pane (stub) |
| `m` | move card (column picker) | | `x` | cancel run |
| `H / L` | shove card left / right | | `r` | retry run |
| **forms** | | | **mouse** | |
| `Tab` / `Shift+Tab` | next / previous field | | `click` | focus card/column |
| `←/→ Space` | cycle a picker field | | `dbl-click` | open card detail |
| `Ctrl+E` | edit textarea in `$EDITOR` | | `drag` | move card / reorder column |
| `Enter` / `Esc` | submit / cancel | | `wheel` | scroll cards |

## CLI synopsis

```
board tui | daemon [--foreground] | status [--json]
board card new --title T [-d D] [--column C] [--harness H] [--model M] [--effort E] \
   [--permission P] [--space-kind workspace|cwd|worktree] [--space-ref R] [--worktree-base B]
board card show <ID> | card list [--column C] | column list
board comment [CARD_ID] <BODY>            # CARD_ID defaults to $BOARD_CARD_ID
board done [CARD_ID] --outcome ok|fail [--summary S]
board move <CARD_ID> <COLUMN> | cancel <CARD_ID> | retry <CARD_ID>
board harness models [HARNESS] | efforts [HARNESS] --model M | permissions [HARNESS]
board space list                          # HARNESS defaults to "claude"
```

`--json` is accepted everywhere. Agent lifecycle rules and examples live in `skill/SKILL.md`.

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

- `scripts/build.sh` — release build of the `board` binary (plugin `[[build]]` step; idempotent).
- `scripts/install.sh` — build + link plugin + copy skill (mutating steps guarded behind `--yes`).
- `scripts/open-board.sh` — the `open-board` action launcher (open-or-focus, toggle off on repeat).
- `scripts/e2e.sh` — end-to-end test against a live herdr session (see the file header).
- `scripts/board-rpc.py` — raw boardd protocol client (e.g. `column.create`, which has no CLI verb).

## Architecture

- [`docs/design.md`](docs/design.md) — architecture, data model, column config, full data flow.
- [`docs/protocol.md`](docs/protocol.md) — the boardd socket protocol contract (single source of truth).
- [`docs/research.md`](docs/research.md) — herdr API capability map, prior art, verified harness flags.
- [`docs/implementation.md`](docs/implementation.md) — build phases.
- [`schema.sql`](schema.sql) — SQLite schema.

## Status

**v1.** Rust (ratatui + rusqlite + tokio), one `board` binary. Harness: **Claude Code** builtin (`claude`) plus config-defined harnesses, behind a `HarnessAdapter` so codex/gemini/opencode plug in later. Execution: **visible herdr panes**. UI: **overlay TUI** summoned by a herdr keybinding (`?` = help). DB is extension-owned (`~/.local/share/herdr-board/`); herdr's own state is untouched. Core is fully tested.
