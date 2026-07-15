# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-15

First release: a kanban board that sits above herdr spaces. Cards are prompts, columns are
pipeline stages, and moving a card into an `auto` column dispatches a real AI coding agent into
a visible herdr pane. Ships as a single `board` binary (TUI + daemon + CLI) and a herdr plugin.

### Added
- **Kanban TUI overlay.** A ratatui board summoned in a herdr overlay pane (`herdr-plugin.toml`),
  keyboard- and mouse-driven: focus/scroll cards and columns, drag to move a card or reorder a
  column, `Enter` for card detail, `?` for the help overlay. Auto-starts the daemon if absent.
- **boardd daemon.** Owns the SQLite state, the run queue, and orchestration: resolves/creates
  herdr workspaces, spawns agent panes, watches herdr status events, and applies each column's
  transition when a run ends. Single-instance (exclusive `flock` on `<db>.lock`); auto-started
  detached by any client on connection failure.
- **`board` CLI.** The same binary exposes the verbs agents call from inside a run —
  `comment`, `done`, `move`, `cancel`, `retry` — plus `card`/`column`/`space`/`session`/`status`
  queries. `--json` accepted everywhere; `CARD_ID` defaults to `$BOARD_CARD_ID`.
- **Claude Code harness.** Built-in `claude` adapter (session mint/resume/fork, model, effort,
  permission-mode) behind a `HarnessAdapter` trait, plus config-defined harnesses driven by
  `$BOARD_PROMPT`/`$BOARD_SYSTEM_PROMPT` so codex/gemini/opencode can plug in later.
- **Column pipeline engine.** Columns carry an optional system prompt (prepended to the card
  prompt) and `on_success`/`on_fail` auto-transition targets; `manual` columns act as human gates.
  A new board seeds a single `Todo` column; `T` applies an example pipeline on an empty board.
- **Session-aware cards and a workspace space model (schema v2).** A card carries a herdr
  `session` (the daemon's default when unset) and a space kind: `workspace` (run in an already-open
  workspace) or `new-workspace` (the daemon opens one on first dispatch). Per-session herdr clients,
  watchers, and workspace auto-create; the daemon resolves a card's session to its socket at
  dispatch via `herdr session list`.
- **kanban-tab grid placement.** Agent panes are placed in the workspace's `kanban` tab
  (find-or-create), tiling roughly square (split `Right` when the largest pane is ≥ 2× as wide as
  tall, else `Down`). Agent names are `card-<id>-<column-slug>`, with a run-scoped fallback on a
  name collision.
- **Capability introspection.** `board harness models|efforts|permissions` and
  `board space list` / `board session list` expose the harness catalog and live herdr state; the
  card form uses them for guided selectors.
- **Guided card form + lowercase `r` refresh** in the TUI: picker fields for
  harness/model/effort/permission/session/space, `Ctrl+E` to edit a textarea in `$EDITOR`.
- **Agent skill** (`skill/SKILL.md`, installed to `~/.claude/skills/herdr-board/`): teaches Claude
  Code sessions how to drive the board from inside a run.
- **Packaging.** `herdr-plugin.toml` manifest, and `scripts/` for build, install (guarded behind
  `--yes`), the open-or-focus launcher, a raw protocol client, and a live-herdr e2e smoke test.

[Unreleased]: https://github.com/nelsonPires5/herdr-board/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.1.0
