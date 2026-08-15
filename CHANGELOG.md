# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- [#90](https://github.com/nelsonPires5/herdr-board/pull/90) feat: projects group boards by folder, with separate project/board selectors in the TUI and CLI.
- [#90](https://github.com/nelsonPires5/herdr-board/pull/90) feat: move cards across projects with `card move --to-project/--to-board` without changing your selection.
- [#90](https://github.com/nelsonPires5/herdr-board/pull/90) feat: create projects and boards from the TUI with forms that select what you create.

### Changed

- [#90](https://github.com/nelsonPires5/herdr-board/pull/90) feat: each project's first board is `main`; existing boards migrate into their project as `main` (schema v14).
- [#90](https://github.com/nelsonPires5/herdr-board/pull/90) feat: the selected project and per-project board persist, and only opening, creating, or selecting updates recency.

### Fixed

- [#89](https://github.com/nelsonPires5/herdr-board/pull/89) fix: the promotion PR is now merged by a maintainer so the Release workflow can tag the green main commit.

## [0.14.0] - 2026-08-13

### Added

- [#82](https://github.com/nelsonPires5/herdr-board/pull/82) feat: duplicate a card from the TUI (`C`) or CLI, creating an idle copy right below it with all settings and no dispatch.
- [#80](https://github.com/nelsonPires5/herdr-board/pull/80) feat: reorder a card within its column via TUI (`O` mini-mode or same-column drag) and `board card move --position`.
- [#83](https://github.com/nelsonPires5/herdr-board/pull/83) feat: reopening a run whose workspace was closed recreates the workspace from the card's space config and resumes there.

### Changed

- [#73](https://github.com/nelsonPires5/herdr-board/pull/73) feat: the daemon stamps resolved labels (`default session`, `default effort`, …) onto card payloads; TUI and CLI render them verbatim.

### Fixed

- [#81](https://github.com/nelsonPires5/herdr-board/pull/81) fix: `f` types normally in form text fields; the popup/fullscreen toggle now works only on picker fields.

## [0.13.0] - 2026-08-10

### Added

- [#65](https://github.com/nelsonPires5/herdr-board/pull/65) feat: add OpenCode as a managed harness.
- [#63](https://github.com/nelsonPires5/herdr-board/pull/63) feat: add Codex as a managed harness.

### Changed

- [#71](https://github.com/nelsonPires5/herdr-board/pull/71) chore(ci): drop the signed-commits rule on main so automated promotion PRs merge without manual signature rewrites.
- [#62](https://github.com/nelsonPires5/herdr-board/pull/62) chore(ci): `dev` is now the long-lived integration branch; `main` stays production-only (automated promotion + release tagging).

### Fixed

- [#67](https://github.com/nelsonPires5/herdr-board/pull/67) fix: repair the Prepare Release workflow so release pull requests open correctly.
- [#64](https://github.com/nelsonPires5/herdr-board/pull/64) fix: retry `agent.start` on a fresh owned pane with backoff so slow login shells stop failing managed runs.

## [0.12.0] - 2026-08-08

- [#59](https://github.com/nelsonPires5/herdr-board/pull/59) Redesign the board TUI mobile-first: boxed cards with one semantic glyph on each dedicated status row, direct visibility filters, persistent chrome around content overlays, transparent white `[ NAME ]` controls, icon detail toggles, no Board-card Edit/Delete controls (keyboard `e`/`d` preserved), drag-only card movement, and a reduced bottom action row.
- [#59](https://github.com/nelsonPires5/herdr-board/pull/59) Keep Compact detail actions fully named across wrapped rows, reserve Runs action rows before history content, and contextualize filter-empty copy.
- [#59](https://github.com/nelsonPires5/herdr-board/pull/59) Tighten the responsive TUI chrome: one-line desktop headers, three-row Compact navigation, `[ X ]` close chips, field-visible multiline editors, and no persistent footer hint row.

## [0.11.1] - 2026-08-08

- [#57](https://github.com/nelsonPires5/herdr-board/pull/57) Let managed card tabs adopt new-workspace roots and run without visible anchors.

- [#56](https://github.com/nelsonPires5/herdr-board/pull/56) Reuse the managed agent pane on same-conversation resume hops while fresh columns still mint and manual landings stay open.

## [0.11.0] - 2026-08-04

- [#54](https://github.com/nelsonPires5/herdr-board/pull/54) Upgrade Herdr compatibility to 0.8.0/socket protocol 19 and centralize the gated client/daemon integration with matching E2E preflight.

## [0.10.0] - 2026-07-30

- [#50](https://github.com/nelsonPires5/herdr-board/pull/50) fix(core,tui): honor Pi `thinkingLevelMap` missing/string/null semantics in model effort menus.
- [#51](https://github.com/nelsonPires5/herdr-board/pull/51) feat(daemon,herdr): add private daily redacted NDJSON diagnostics with 30-day retention.
- [#52](https://github.com/nelsonPires5/herdr-board/pull/52) chore(ci): run the complete provider-free live E2E suite with pinned Herdr and always-uploaded 30-day evidence.

## [0.9.1] - 2026-07-27

- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) feat(cli)!: `board` exit codes are meaningful instead of always `1` — an RPC error exits with the daemon's protocol code for `1..=5` so a script can branch on `$?`, a protocol code outside that range exits `70` (`EX_SOFTWARE`, because an exit status is taken modulo 256 and `256` would read as success) while the `--json` envelope still carries the exact code, and an error the CLI itself raised (usage/parse, a declined confirmation, a bad enum value, a column reference that resolves to nothing client-side, a missing `$BOARD_CARD_ID`) exits `64` (`EX_USAGE`).
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(cli)!: the `--json` error envelope for CLI-local errors reports `"code": 64` instead of `"code": 2`, so a CLI refusal can no longer be mistaken for the daemon's "not found"; `kind` stays `"cli"` and RPC envelopes are unchanged.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(cli): `--json` is a true global flag accepted before or after the subcommand path, and is decided by parsing rather than an argv scan — a positional whose *value* is the literal `--json` no longer flips error rendering.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) feat(cli): add `board daemon start [--foreground]`, `board daemon stop`, and `board daemon status` as real subcommands, with `daemon stop --json` emitting `{"stopped": bool, "was_running": bool}` (human text unchanged); bare `board daemon` and the historical `--foreground`/`--stop` flags keep working but are hidden from `--help`.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) feat(cli): `--destination-board` is the documented way to cross boards on `move`; the old fallback to the global `--board` still works but now prints a deprecation warning to stderr.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(cli): human list output is space-aligned through one shared table helper (some commands emitted raw tabs, others computed their own widths), `card show` renders comments in the same shape as `comment show`, and enum parse errors are one form everywhere — `invalid <kind> '<value>' (expected: a, b, c)` — with `effort` and `trigger` gaining the expected list they never printed. `--json` output is byte-identical to before.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(cli): `--fresh-session` and `--reuse-session` conflict at parse time (and say so in `--help`) instead of being rejected at runtime; the legacy top-level verbs `done`, `move`, `cancel`, `retry`, and `comment` are unchanged for callers but now re-dispatch into the nested handlers, so there is one implementation per operation.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) feat(daemon,core,tui): new `pane.set_title {pane_id, title, origin_socket}` → `{renamed:true}` renames one pane in the **caller's own** herdr session through the same gated connect as every other operation, so the TUI keeps its plugin-pane border in sync with the board it shows without any client opening a Herdr socket itself; a rename that did not happen is an error, never a silent success.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(daemon): a request handler task that panics or is cancelled now answers with error code `5` instead of killing the connection and every other in-flight request on it, and the `herdr session list --json` shell-out is capped at 10s with the child killed on expiry (it was unbounded, on the path of every request that resolves a session).
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(tui): destructive actions in card detail are gated and confirmed — `r` (retry) now confirms because it relaunches a real agent, `x` (cancel) only confirms when there is actually an open run and otherwise toasts, and deleting a column that holds cards confirms after the "move them where?" picker instead of deleting on selection (only the empty-column path had ever confirmed, leaving the riskier one unguarded).
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(tui): `?` is bound globally and returns to the screen it was opened from (it previously worked on two screens and dumped you back on the board), the help sheet scrolls with `j`/`k` in every layout (the keybinding table had outgrown a fixed sheet and was silently truncating at 80x24), and `q` closes the Compact switcher.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(tui,core): an unknown harness whose `harness.capabilities` fetch has not landed hides the permission selector instead of guessing a six-item list that included a value the daemon rejects — the rule is now `permission_modes.is_empty()` against the effective capabilities, never a harness-name comparison; the archive refusal also reads "card has an open run; cancel it before archiving", sourced from `board-core` rather than restated in the TUI.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(core,daemon): a duplicate name is error `1` with an actionable message (`column "Todo" already exists on this board; pick another name`) instead of error `5` leaking `sqlite: UNIQUE constraint failed: columns.board_id, columns.name` — code `5` told a scripting agent the daemon had broken and invited a retry that could never succeed; covers `column.create`, `column.update`, `board.rename`, `template.apply`, and `board.open`, whose `INSERT OR IGNORE` had been swallowing the conflict and reporting `sqlite: Query returned no rows`. Only `SQLITE_CONSTRAINT_UNIQUE` is translated, so a genuine internal failure is still `5`.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) fix(tui): the help sheet's key column no longer runs into its own description (`q / Esc / anyhelp: close`) — the column pads but never clipped, so an over-long label overflowed it; keys are now truncated to the column and a test pins every label's width, which the existing description-width test never covered.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) chore(ci): run `e2e/test-harness.sh` — the provider-free static safety gate that four documents already called required, and that CI had never executed — and split the gates into `docs`, `scripts`, `e2e-safety` and `test` jobs by what each protects; only `test` needs a Rust toolchain, so a stale doc or a regressed safety token reports in seconds instead of queueing behind a compile, and one failure no longer hides the other two answers. `test_docs.py` now also asserts every `scripts/tests/test_*.py` is matched by some job's pattern, so the split cannot strand a module in no job.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) refactor(daemon,tui,cli,core): reorganize by responsibility — `board-daemon` gains `herdr_conn.rs` (the centralized gated connect), `rescue.rs`, `recovery.rs`, `logging.rs`, `ops/{errors,panes}.rs`, `spawner/{error.rs,placement/,herdr/}` and `dispatch/{launch_plan,ownership}.rs`; `board-tui` splits `lib.rs` and `app/mod.rs` into `origin.rs`, `runtime.rs`, `driver/` and `app/{state,effect,nav,drag}.rs`; `board-cli` splits `args.rs` into an `args/` module and adds `render.rs` (the single output path) plus `context.rs`; `board-core` adds `engine/columns.rs` and shares `Patch::from_flags`/`from_option`, `protocol::parse_timestamp`, `capability::default_capabilities`, `engine::resolve_column`, `engine::run_elapsed`, `Comment::is_system`, `paths::session_name_from_socket`, and public `Db::require_card`/`require_column`. `SpaceKind::parse_str` also accepts the hyphenated `new-workspace` so the CLI's flag spelling resolves through board-core; the wire vocabulary and the emitted form are unchanged.
- [#47](https://github.com/nelsonPires5/herdr-board/pull/47) test: `board-daemon/src/testkit.rs` replaces 13 hand-rolled `Daemon` constructions and 7 hand-rolled fake-Herdr socket servers with one builder each plus shared `assert_no_effects`/`assert_no_events`/`fault_db`; `board-tui` test helpers consolidate into `src/testkit.rs`; the CLI and core suites split by concern (`integration/{cards,comments,columns,runs,meta}.rs` plus a new `exit_codes.rs`; `tests/db/{migrations,atomic}.rs`); and a new `ops/tests/parity.rs` asserts, from the dispatch tables themselves, that `FakeBoardClient` implements every method boardd routes or names it in a `KNOWN_UNIMPLEMENTED` allowlist — the whole board-tui tier runs against that fake, so a drifted method was a hole no TUI suite could see.

## [0.9.0] - 2026-07-27

- [#45](https://github.com/nelsonPires5/herdr-board/pull/45) feat(core,daemon,cli,tui)!: `run.focus` now requires an explicit `run_id` (breaking) and returns the focused run's full identity — card, column, harness, herdr session name and harness conversation id as separate documented fields, plus pane — resolved through a new ownership-validating `Db::run_for_card`, so a specific historical run can be focused and a foreign or pane-less run fails with a clear non-destructive error instead of silently landing on the latest run.
- [#45](https://github.com/nelsonPires5/herdr-board/pull/45) feat(tui,daemon,herdr): pick a specific historical run in card detail's Runs section — a bright-blue `▸` selected-run cursor moved with `↑`/`↓` and `k`/`j` (separate from the scroll offset, which follows it, defaulting to the newest run and surviving a detail refresh; the same shared marker the comments list uses, so both lists now mark their cursor in blue), run rows kept minimal — run number, harness, status and how long the run took or has been running — and `o` jumping to that exact run (a mouse-wheel scroll of either detail section now drags that section's cursor into view instead of stranding the marker off screen, and `o` before the detail loads says so rather than doing nothing); the daemon additionally verifies the recorded pane still exists (new `pane.get` wrapper) so a closed pane fails with a distinct non-destructive error rendered as a non-fatal toast instead of an opaque `pane.focus` failure, and the now-callerless `Db::latest_run_with_pane` is gone.
- [#45](https://github.com/nelsonPires5/herdr-board/pull/45) feat(core,daemon,cli,tui): reopen a run whose pane was closed — `run.focus` now resumes the run's harness conversation in a new pane in the card's tab instead of dead-ending, reported through a new `action` field (`focused_recorded_pane` / `focused_rescued_pane` / `rescued`) plus the dead `recorded_pane_id`; the launch is derived from the run's persisted launch spec (same model/effort/env) with the card task never re-sent, resume is an explicit per-harness capability (`pi`/`claude` built in, `[harness.NAME] resume = true` to opt in, fail-closed otherwise), a second `o` reuses the reopened pane instead of creating a second one, and the rescue writes **nothing** to the database — so the historical run row stays immutable and the reopened pane is deliberately ephemeral and unwatched, receiving `BOARD_CARD_ID`/`BOARD_SOCKET`/`BOARD_BIN` but never the `BOARD_RUN_ID` actor credential that would let it rewrite the finished run.
- [#43](https://github.com/nelsonPires5/herdr-board/pull/43) feat(tui): manage comments from card detail — a focused-comment marker moved with `j`/`k` (the viewport follows it), `e` edit / `d` delete (behind the confirm overlay) / `h` audit-trail sheet on the focused comment, all three also reachable from a tappable `[ Edit ]` `[ Delete ]` `[ History ]` bar with every comment row registered as a hit zone; system comments render their edit/delete dimmed and explain the refusal instead of issuing a call the database rejects, and soft-deleted comments drop out of the list as `card.get` already filters them. Also adds an `⊞ Apply template` row to the Compact switcher routed through the same single-source helper as the board's `T` key, and a `template.apply` arm to `FakeBoardClient` so that path is testable.
- [#42](https://github.com/nelsonPires5/herdr-board/pull/42) feat(tui): mobile-responsive Compact layout (terminal width `< 60` cols) — a single full-width column with a tappable `‹  [ ⇄ <column>  n/N ]  ›` header, a Compact-only board/column switcher sheet, per-column card scrolling with a scrollbar in every layout mode (mouse wheel now scrolls the hovered column instead of reordering the focused card), wrapped card titles and help/picker text, and a new widget/HitMap layer adding visible `[ Save ] [ Cancel ]` buttons and sheet close buttons for touch; also fixes the edit form's description field never wrapping (it hard-truncated to one line), a blank screen after returning from `$EDITOR`, and the e2e suite's daemon-readiness poll still calling the renamed `board status` (which failed every scenario).
- [#41](https://github.com/nelsonPires5/herdr-board/pull/41) Add canonical CLI parity plus schema-v13 comment CRUD, soft deletion, and immutable audit history.
- [#40](https://github.com/nelsonPires5/herdr-board/pull/40) Reserve a durably owned shell anchor in each board-managed card tab; every stage/retry launches only from a split child, with safe anchor recovery and child-only cleanup.
- [#39](https://github.com/nelsonPires5/herdr-board/pull/39) Per-card Herdr tabs use stable `card-<id>` labels, exact board-owned tab IDs, safe restart reconstruction, serialized first allocation, and closed-tab recreation; legacy `kanban` rows remain unchanged.
- [#38](https://github.com/nelsonPires5/herdr-board/pull/38) docs(visual-validation): require ALWAYS-isolated board state — DB, socket, AND daemon — under a short `/tmp` dir (no exceptions for trivial or "quick peek" runs; `board tui` auto-starts its own isolated daemon) and open runnable prototypes in a visible wezterm tab for interactive validation, matching the Prototyping-column prompt.
- [#37](https://github.com/nelsonPires5/herdr-board/pull/37) feat(core,daemon,tui): move a card to a column of another board — `card.move` gains optional `board_id`, an atomic `transfer_card` recompacts both columns, `board_changed` carries `board_id` (one event per affected board), a cross-board blocking sanity check (merged capabilities + session resolve + read-only workspace preflight for auto columns) aborts incompatible moves, and `m` is a hybrid picker (`m` = active-board columns, `b` = other board).
- [#36](https://github.com/nelsonPires5/herdr-board/pull/36) feat(tui): column form hides the `system_prompt` field when the trigger is `manual` (no run launched) and reveals it for `auto`; the field is hidden, not omitted, so submit still sends a `Patch` that preserves the stored value.
- [#35](https://github.com/nelsonPires5/herdr-board/pull/35) Retry transient `agent_pane_busy` on the same owned pane with bounded backoff and safe cleanup.
- [#34](https://github.com/nelsonPires5/herdr-board/pull/34) Separate the operational board skill from isolated visual-development validation and avoid duplicate implementation worktrees.
- [#33](https://github.com/nelsonPires5/herdr-board/pull/33) feat(tui): card detail word-wraps the description and each comment body at the panel border (popup and fullscreen) instead of truncating to one line; comments now scroll by wrapped row.

- [#31](https://github.com/nelsonPires5/herdr-board/pull/31) Pin Herdr plugin installs to the latest released tag (`--ref v0.8.0`) and require one-line, PR-linked `CHANGELOG.md` entries.
- [#32](https://github.com/nelsonPires5/herdr-board/pull/32) feat(tui): `M` (Shift+m) mini-mode to move the focused column (`←`/`→` stage, `Enter` commits one `column.reorder`, `Esc` cancels).

## [0.8.0] - 2026-07-23

- Internal Rust refactor organizes production modules and tests by responsibility: public API behavior
  uses crate integration-test targets, while private invariants remain adjacent to their
  implementations.
- Schema v11 snapshots each run's materialized harness argv, configured environment, managed prompt channels, and selected Herdr session at enqueue time. Later card, column, or harness edits cannot alter those persisted inputs; daemon run/socket metadata is attached at dispatch, and older database rows retain their versioned compatibility behavior.
- T20/R11 cleanup removes the unused public Herdr worktree calls and DTOs without changing the pinned Herdr API schema fixture. Documentation now cross-links the v11/v1/protocol-17 ownership matrix and scenarios 01–21, including typed client/config boundaries, daemon launch ownership, active-run timers, supervisor recovery, and the provider-free safe harness. Auto-start is characterized as one exact child process-group owner rather than an undocumented double-fork/`setsid` lifecycle.
- Board snapshots now carry an additive v1 `active_runs` summary (`card_id`, `started_at`) scoped to the requested board. The TUI uses the open run start for timers, so comments and other card updates do not reset elapsed time; live scenario 21 verifies the real event-refresh path without a provider.
- Schema v10 uses partial queued/active indexes and direct scheduler queries, avoiding full run-history scans. Independent spaces can begin launch concurrently while typed per-space FIFO, a serialized dispatch-claim pass, and the global cap prevent duplicate or oversubscribed launches.
- Daemon restart recovery now performs conservative Herdr reconciliation independent of initial connection success. An always-on per-socket supervisor connects when Herdr appears, isolates subscription changes and reconnect backoff to the affected socket, subscribes before snapshot reconciliation, and periodically repairs missed-event gaps. Only confirmed missing panes fail; unresolved sessions and probe failures remain open.
- Schema v9 preserves timeout deadlines and awaiting pauses across daemon restarts; pause/resume is atomic and restart never resets a run's budget. Upgrades derive legacy running deadlines once from each original start and pause awaiting runs at their last durable card update.
- Schema v8 enforces one open run per card and makes enqueue, promotion, and finalization durable atomic DB units of work. Daemon board-done, cancel, timeout, and pane-exit paths now execute final comments, card transitions, auto-hop enqueue, and source/target position compaction in that single finalization transaction; failures leave the exact prior durable state, duplicate or stale losers are idempotent, and post-commit effects run in deterministic order. Upgrades from pre-v8 databases fail closed without changing schema shape or version when legacy data contains multiple open runs for one card; duplicates must be resolved before retrying.

### Changed
- The standard provider-free E2E harness now runs on Linux and macOS. Portable filesystem helpers replace GNU-only `stat`/`readlink` assumptions; process identity uses Linux `/proc` or Darwin `libproc` plus `KERN_PROCARGS2`, with versioned HMAC-signed direct-child capabilities, complete argv verification, and a non-exported signing key delivered only over an inherited file descriptor. Each environment-scrubbed scenario gets its own marker-gated Herdr session, isolated board stack, and private artifact directory; `--keep` retains its disposable Herdr session/workspace while still cleaning daemon and temporary roots, `--require-all` converts skips to failures, and `bash e2e/test-harness.sh` provides a separate no-Herdr safety gate. `run-all.sh` also resolves Bash ≥4 before narrowing `PATH`, and macOS canonical `/private/tmp` paths are compared without weakening bounded-root cleanup. The independent real-Claude smoke remains Linux-only.
- Run lifecycle is consolidated into a canonical three-step core DB API: `enqueue_run_uow` →
  optional `promote_run_uow` → `finalize_run_uow`. No legacy methods remain. Queued cancel
  and spawn failure are themselves finalized through `finalize_run_uow` and are atomic with
  zero external effect before commit; all process, socket, notification, and event effects
  execute in a fixed deterministic order only after the transaction commits.
- Runtime launch ownership now resides entirely in `board-daemon`; `board-core` retains only the
  versioned, runtime-neutral execution specification. Existing argv/environment materialization,
  placement, liveness, and cleanup behavior is unchanged.
- Column create/edit forms now load harness metadata immediately when opened, without fetching
  card-only sessions or workspaces; existing field values and focus survive the refresh.
- Daemon startup now parses a typed `RootConfig` once; malformed existing TOML or daemon values are fatal instead of silently falling back to defaults, and environment overrides are applied afterward with precedence.
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
  reads after bounded polling/handshakes. Event polling also carries partial newline-delimited
  events across timeout returns.
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

[Unreleased]: https://github.com/nelsonPires5/herdr-board/compare/v0.14.0...HEAD
[0.14.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.14.0
[0.13.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.13.0
[0.12.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.12.0
[0.11.1]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.11.1
[0.11.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.11.0
[0.10.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.10.0
[0.9.1]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.9.1
[0.9.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.9.0
[0.8.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.8.0
[0.7.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.7.0
[0.6.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.6.0
[0.5.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.5.0
[0.4.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.4.0
[0.3.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.3.0
[0.2.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.2.0
[0.1.1]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.1.1
[0.1.0]: https://github.com/nelsonPires5/herdr-board/releases/tag/v0.1.0
