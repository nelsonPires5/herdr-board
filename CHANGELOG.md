# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- Schema v8 enforces one open run per card and makes enqueue, promotion, and finalization durable atomic DB units of work. Daemon board-done, cancel, timeout, and pane-exit paths now execute final comments, card transitions, and auto-hop enqueue in that single finalization transaction; failures leave the exact prior durable state, duplicate or stale losers are idempotent, and post-commit effects run in deterministic order.

### Changed
- CLI and TUI board operations now share typed `BoardClient` wrappers for harness, space, session,
  and run actions; raw method/result handling remains confined to the transport primitive, with no
  production client-side SQLite I/O.
- Card and column settings now use shared merged capability validation before persistence or
  change events; effective card/column settings are revalidated at enqueue time, including legacy
  rows. Invalid model/effort/permission combinations are rejected atomically, Pi permission modes
  remain unsupported, and `bypassPermissions` is limited to explicit Claude card settings rather
  than column defaults.
- Live E2E scenario 18 (`18-nullable-clear.sh`) catalogs omitted/null/value persistence, merged
  validation rejection, and provider-free dispatch after clearing overrides without recording
  prompt bodies.
- Nullable `column.update` and `card.update` fields now preserve omitted vs `null` vs value in
  board protocol v1: omission leaves values unchanged, `null` clears them, and a value replaces
  them. Database updates and TUI edits honor the same tri-state semantics.
- Herdr socket requests and subscription acknowledgements now have bounded deadlines, match exact
  response IDs, preserve events interleaved before acknowledgements, and restore blocking stream
  reads after bounded polling/handshakes.
- Board event subscribers now use bounded outbound queues: consecutive coarse refreshes coalesce,
  responses and terminal events retain order, and subscribers that cannot accept a terminal event
  are disconnected to reconnect and refetch rather than growing daemon memory without bound.
- `board daemon --stop` now stops safely: it reports success only after the listener disappears,
  preserves the socket on RPC errors/timeouts, and removes stale sockets only after identity checks.
- The opt-in real-Claude Haiku smoke now stages only completed onboarding/theme, exact workspace
  trust, the installed Herdr hook, credentials, and approved `remote-settings.json` bytes. This
  prevents startup dialogs from consuming `agent.prompt` without copying broad personal Claude
  state; it still permits exactly one no-retry Haiku/low attempt.

## [0.7.0] - 2026-07-22

### Added
- Provider-free fake Pi/Claude fixtures and live E2E scenarios 16 and 17 cover managed
  protocol-17 launch and the configured runner bridge. The full forced-build standard E2E suite
  scenarios 01–17 pass with no model-provider calls, using isolated controlled shell state and
  cleanup scoped to the owning session root.
- A separate fail-closed `real-claude-haiku-smoke.sh` defines an explicitly opted-in contract for
  one authorized Claude Haiku/low attempt against disposable board, Herdr, workspace, and staged
  Claude state; it never runs in the standard suite and has no retry or fallback provider path.

### Changed
- Herdr support is protocol-17-only: install metadata requires 0.7.5, and runtime rejects every
  Herdr version other than 0.7.5 and every protocol other than 17 before discovery or placement.
  There is no protocol-16 compatibility path.
- Managed Pi and Claude dispatch is pane-first. The daemon creates or splits a pane with cwd/env,
  starts the explicit managed kind in that pane, waits for interactive readiness, and only then
  sends the card prompt with `agent.prompt`.
- Managed system instructions use a separate temporary `0600` file, removed after startup; they
  never share startup argv or the post-readiness card-prompt channel.
- Schema v7 snapshots the resolved system prompt when a run is enqueued, so queued/restarted work
  is unaffected by later column edits. Pre-v7 rows remain `NULL` with no backfill and retain their
  persisted legacy all-in-one argv behavior.
- Configured harnesses now run in a board-owned pane through the selected socket's `herdr pane run`
  bridge and a self-removing `0700` argv-safe script. The configured-only callback accepts the exact
  open queued/started run (including callback-before-registration), and an immediate configured
  `board done` may likewise finalize that exact queued run before registration. `RunDoneParams.run_id`
  is optional for manual/TUI compatibility; the CLI forwards `BOARD_RUN_ID` when present, and a
  mismatched id rejects the active run, including the exact queued configured exception. Queued
  built-in Pi/Claude runs are rejected until their managed pane is registered; stale or built-in
  callbacks are rejected and silent exits never transition. Its runner honors nonempty
  `HERDR_BIN_PATH`, else `herdr`. A scheduled configured script can remain as the documented
  residual orphan if its pane never opens it.
- Enqueue snapshots card/column/comments/settings/task/system/session data atomically under the
  scheduler→store lock. Existing, reused, and newly created workspaces must provide a live snapshot
  cwd; dispatch fails rather than using a requested, stale, or process cwd. Watcher identity includes
  both session socket and pane id. E2E session names are collision-resistant and exact-name
  preflighted; boot, adoption, and teardown are gated by captured Linux `/proc` identity tokens
  (start time, executable, complete
  argv, expected session/name), never PID liveness alone. The token gates primary/secondary
  workspace close, board-daemon signals, and session stop/delete across run-all, standalone, and
  future real-Claude smoke paths; the real-Claude daemon identity is independent. Cleanup failures
  propagate so a successful scenario cannot hide failed cleanup. Board state is isolated under a
  short `/tmp` root and managed-shell cleanup is restricted to its marked owner root.

## [0.6.0] - 2026-07-21

### Added
- New card statuses `awaiting` and `done` (schema v6 adds `cards.awaiting_reason`). `awaiting`
  means the agent finished — or went idle past `idle_grace_seconds` — **without** `board done`:
  the run stays open, the column timeout is paused, and the card waits for human review instead of
  failing. The reason (`agent_done` / `idle_expired`) is shown in the card detail. `done` is
  confirmed completion via `board done ok` when the column has no `on_success` target (with a
  target the card moves as before). The TUI renders `?` (yellow) for `awaiting` and `✓` (green)
  for `done`, and `Enter` on an `awaiting` card's detail confirms completion through the same
  `run.done ok` channel.
- `engine::decide_signal` is now the single, pure decider for agent signals: watchers only
  translate herdr pane statuses and idle expiry into signals, and the daemon applies the engine's
  decision in one place.
- Live e2e scenario `15-awaiting.sh` covers the silent-finish → `awaiting` → confirm → `done`
  flow.

### Changed
- The hermetic TUI demo now carries a real open run for its `awaiting` card, and the fake client
  executes `run.done` through the same pure transition engine used by the daemon.
- Idle past `idle_grace_seconds` no longer fails the run as `lost`; it parks the card in
  `awaiting` (reason `idle_expired`). The `lost` outcome is kept in the schema and wire enums for
  backward compatibility but is no longer produced.
- The board-protocol preamble is injected unconditionally into every run's prompt, instructing
  agents to finish with `board done ok` / `board done fail` and warning that finishing without
  `board done` leaves the card in `awaiting`.
- Docs now stress that `herdr integration install <harness>` is a user prerequisite (the board
  never installs integrations): without it, herdr's `working`/`blocked`/`done` signals don't exist
  and `awaiting` is only reachable via the idle grace path (degraded mode).
- Independent boards per canonical Git root or non-Git CWD, with separate columns, templates, and
  cards. Schema v5 preserves all previous data as `Global`; `b` opens a path-disambiguated board
  picker and pane titles combine scope with `ACTIVE` / `ALL` / `ARCHIVED`.
- Card detail `o` now focuses the newest recorded run pane through daemon-mediated Herdr
  `pane.focus`. Same-session validation prevents pane-id collisions across sessions; success closes
  the overlay and errors remain as toasts.
- Live E2E scenarios cover Git/CWD board identity and real-plugin jump-to-pane behavior.
- `board daemon --stop` gracefully stops the running daemon over its socket (idempotent; clears a
  stale socket if nothing is listening). The plugin `build.sh` calls it before recompiling, so a
  reinstall replaces a stopped process rather than overwriting a binary the old daemon still has
  mapped — the cause of stale-daemon version drift after an update. README `Maintenance` now
  documents the update flow and a full uninstall (stop the daemon first, since Herdr's plugin
  uninstall has no lifecycle hook and leaves the detached daemon running).
- `HarnessMeta` adapter trait in `board-core` is the single daemon-side source of truth for harness
  models/efforts/permissions; built-in `pi`/`claude` and config-defined harnesses all implement it
  and produce the existing `HarnessCapabilities` wire DTO via `from_meta`.
- `harness.list` RPC returns every harness the daemon knows (built-ins `pi`/`claude` in
  default order, then every config-defined `[harness.NAME]` sorted) — the single source for
  both the card `harness` and column `harness_override` TUI selects. A matching
  `board harness list` CLI verb mirrors it.
- The `pi` harness now reports a **live** model catalog (real `provider/model` ids with per-model
  efforts from each model's `thinkingLevelMap`) instead of always `models:[]`. The daemon reads
  `$PI_CODING_AGENT_DIR`/`~/.pi/agent` (`auth.json` + `models-store.json`), filters to authenticated
  providers, and falls back to `pi --list-models` then the static catalog. `model_freeform` stays
  `true`.
- Scope-sensitive CLI commands use Git root/CWD (overridable with `BOARD_SCOPE_PATH`), while
  card-id operations and `move` infer the card's own board. Legacy protocol requests without
  `board_id` continue to target `Global`.

### Fixed
- Run finalization now holds an in-memory per-card claim from the atomic run close through its
  transition and optional auto-enqueue. Concurrent retry/enqueue and conflicting card or column
  mutations are rejected until the final status is committed.

## [0.5.0] - 2026-07-18

### Added
- Pi Coding Agent is a first-class built-in harness with runtime-default/free-form models,
  `off|minimal|low|medium|high|xhigh|max` thinking, exact mint/resume IDs, retry forks to a new
  persisted session, and the mandatory board completion protocol.
- Deterministic Pi/Herdr lifecycle tests cover working, blocked, idle-lost, pane exit, and spawn
  failure. Standard live E2E dispatches a checked-in fake `pi` through real Herdr at zero model
  cost; a separate fail-closed real-Pi poem smoke supports isolated visual validation.

### Changed
- Pi is now the default for newly created cards, TUI forms, and harness CLI queries. Existing
  stored Claude cards are preserved and Claude remains explicitly selectable with unchanged argv
  and permissions.
- The TUI preserves an omitted `(default)` model, supports a custom Pi `provider/model`, exposes
  Pi thinking levels, and hides/rejects permission mode for Pi.

## [0.4.0] - 2026-07-17

### Added
- Cards can be archived/restored without losing comments or run history. The TUI uses `a` to
  toggle archive state and `v` to cycle `ACTIVE` / `ALL` / `ARCHIVED`; the current filter appears
  in the Herdr pane title, the board footer stays minimal (`? help`), and archived cards are dimmed
  and marked `▣ ARCHIVED`. The CLI exposes `board card archive|restore <ID>`.
- Card detail now opens as a contextual popup with a clickable/`f` fullscreen toggle, `e` editing
  that returns to detail, and independent keyboard/mouse scrolling for comments and run history.
  The focused history uses a blue divider; histories open at their latest item and show only
  directional arrows (no counts) when content is hidden.

### Changed
- Reorganized the README around installation and first use, with real TUI screenshots and
  collapsible advanced reference sections.
- The board now distributes visible columns across the full viewport, uses higher-contrast
  status-rich cards, and shows card counts in column headers.
- Detail sections and status metadata have clearer visual hierarchy; forms, pickers, and help size
  to their content instead of occupying fixed percentages of large terminals.

## [0.3.0] - 2026-07-17

### Added
- macOS platform support in `herdr-plugin.toml` (`platforms = ["linux", "macos"]`), enabling
  `herdr plugin install` on macOS. The uninstall snippet in README now uses `sha256sum` with a
  `shasum -a 256` fallback for stock macOS compatibility.

### Changed
- `scripts/install-cli.sh` now uses portable checksum selection (`sha256sum` / `shasum -a 256`)
  and avoids GNU-only `ln -T` / `mv -T`, preserving managed checksum and collision safety.

### Fixed
- Flaky Stage1→Stage2 pane placement race: when a chained auto-column Stage1
  finishes and its agent pane closes, the Stage2 placement could pick up the
  now-closing pane, focus it, and fail `agent.start` with `pane_not_found`.
  The placement now retries once on `pane_not_found`, rediscovering the
  `kanban` tab; and existing-but-empty tabs land unsplit instead of querying
  `pane.layout(null)` which may return a different tab's layout.

## [0.2.0] - 2026-07-16

### Changed
- GitHub plugin installation now builds herdr-board and copies the `board` CLI to `~/.local/bin/board` as part of the trusted plugin build, with an install-directory override for custom setups. A per-directory marker records the installed binary's SHA-256 checksum; managed updates validate matching regular-file contents and refuse to overwrite an unrelated or subsequently replaced `board` command.

## [0.1.1] - 2026-07-15

### Added
- Documented the release contract in [`docs/releasing.md`](docs/releasing.md): Prepare Release bump choice, bot-opened PRs, explicit CI dispatch, CI-green `main` publishing, artifacts, reruns, and tag immutability.

### Changed
- The release helper now verifies synchronized release files and uses atomic, rerunnable writes after partial failure.
- Release publication is gated on a version bump in the green `main` CI commit, with draft/asset recovery and immutable tags.
- CI is split into `fmt`, `clippy`, and `test` jobs, with clippy warnings annotated on pull requests.
- The end-to-end suite runs against an ephemeral herdr session per invocation and supports `--keep` for review.
- `scripts/install.sh --yes` now applies the `open-board` keybinding during install.

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

[Unreleased]: https://github.com/nelsonPires5/herdr-board/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.7.0
[0.6.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.6.0
[0.5.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.5.0
[0.4.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.4.0
[0.3.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.3.0
[0.2.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.2.0
[0.1.1]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.1.1
[0.1.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.1.0
