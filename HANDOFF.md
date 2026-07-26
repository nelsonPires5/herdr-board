# HANDOFF — TUI follow-ups from PR #41 (schema v13) and PR #42 (responsive TUI)

> **START HERE / TRIGGER**
>
> You are picking up work on branch `feat/tui-comment-crud-and-templates` (branched from `main` @ `11d9ab4`).
> Your task is everything in **§3 Scope** of this file: expose comment edit/delete/history in the board
> TUI's card detail, and make templates reachable from the switcher sheet. Read §4 (traps) before
> writing code — several of them cost the previous session hours. Read §5 (definition of done) before
> declaring anything finished.
>
> Before you start, ask the user the questions in **§6**. They are multiple choice; do not guess the
> answers. Then work through §3 in order.
>
> **Delete this file in the same PR that implements the work** — it is a working document, not repo
> documentation.

---

## 1. Where things stand

`main` @ `11d9ab4` contains two recently merged PRs that created this follow-up work:

- **#41 — CLI parity + schema v13.** Added full comment CRUD (`comment.get/update/delete/history`),
  soft deletion, and an append-only audit history, exposed through the protocol, the client trait, and
  the `board` CLI. **None of it is reachable from the TUI** — the TUI still only calls `comment_add`.
- **#42 — mobile-first responsive TUI.** Added `LayoutMode` (Compact `< 60` cols / Regular `60..=119`
  / Wide `>= 120`), a Compact single-column board with a tappable header, per-column vertical
  scrolling with scrollbars, a Compact-only board/column switcher sheet, `sheet_area()` for every
  overlay, and the `crates/board-tui/src/widgets/` layer (`HitMap`/`Zone`/`ButtonBar`/
  `render_sheet_frame`/`windowed_rows`). Also fixed multiline form fields never wrapping and a blank
  screen after returning from `$EDITOR`.

Everything below is scoped to the TUI. No protocol, daemon, schema, or CLI changes should be needed —
if you think one is, stop and raise it rather than widening the change.

## 2. API surface you will be calling (verified against `main` @ `11d9ab4`)

### Comments

Wire protocol — `docs/protocol.md:160-177`, DTOs `crates/board-core/src/protocol.rs:600-649`:

| RPC | Params | Returns |
|---|---|---|
| `comment.add` | `card_id`, `body`, `author?`, `actor_run_id?` | `Comment` |
| `comment.get` | `id` | `CommentRecord { id, card_id, author, body, created_at, deleted_at }` |
| `comment.update` | `id`, `body`, `actor_run_id?` | `CommentRecord` |
| `comment.delete` | `id`, `actor_run_id?` | `{ deleted: true }` (soft delete) |
| `comment.history` | `id` | `Vec<CommentHistory>` |

Client trait methods — `crates/board-core/src/client/traits.rs`:

- `comment_add(card_id, body, author)` — traits.rs:226
- `comment_add_for_run(card_id, body, author, actor_run_id)` — traits.rs:234
- `comment_get(id)` — traits.rs:252
- `comment_update(id, body, actor_run_id)` — traits.rs:258
- `comment_delete(id, actor_run_id)` — traits.rs:273
- `comment_history(id)` — traits.rs:283

**`FakeBoardClient` already implements all five** (`crates/board-core/src/client/fake.rs:250-287`), so
snapshot/reducer tests need no new fake plumbing for comments.

Daemon-enforced invariants you must respect in the UI —
`crates/board-daemon/src/ops/comments.rs`:

- **System comments are immutable** to any actor (`docs/protocol.md:176-177`). Do not offer edit/delete
  on them; the daemon will reject it.
- An **agent** actor may only mutate its own comment from its own still-open run
  (`require_agent_run`, comments.rs:8-49; `comment_for_mutation`, comments.rs:51-63). The TUI acts as a
  human (`author = None` → daemon defaults to `"user"`), so this mostly means: expect rejections when
  editing agent comments and surface the error rather than swallowing it.
- Delete is **soft**: `comment.get` still returns the row with `deleted_at` set. A deleted comment
  cannot be edited or deleted again.
- History is **append-only** (`comment_history` table + `comments_audit_insert` trigger,
  `schema.sql:83-103`). Every insert — including system/daemon comments — gets a first snapshot.
  `comment.history` 404s on an unknown id, which is distinct from an empty list.

### Templates

- RPC `template.apply { name, board_id? }` → `Vec<Column>` (`protocol.rs:474-480`,
  `docs/protocol.md:87`).
- Client: `template_apply(name)` and `template_apply_for_board(name, board_id)`
  (`traits.rs:127-142`).
- **Only one template exists: `"pipeline"`**, and it is a hard-coded Rust function, not a stored or
  configurable entity — `crates/board-daemon/src/template.rs:32-116` creates five fixed columns
  (`Plan`/`Execute`/`Review` auto with prompts, `Human Review`/`Done` manual) and wires their
  `on_success`/`on_fail` transitions.
- **Precondition:** the board must have exactly the seed `Todo` column and **no cards**, else
  `InvalidState("template.apply requires an empty board (only the seed Todo column, no cards)")`
  (template.rs:40-53). Unknown name → `BadRequest`.
- **`FakeBoardClient` does NOT implement `template.apply`** — it falls through to
  `bail!("unsupported method")` (`fake.rs:288`). Any test that exercises the template path needs a
  fake arm added first. This is the one place you will likely have to touch `board-core`.

### Current TUI integration points

- Comments render in `crates/board-tui/src/view/detail.rs`: section built at detail.rs:318-364, sized
  by `detail_section_heights()` (detail.rs:97-121) via `comment_wrapped_rows()` (detail.rs:87-95) on
  top of `wrapped_row_count()` (detail.rs:47-80); row-based scroll through
  `Paragraph::scroll((app.detail_comments_scroll, 0))`.
- Add-comment flow: `c` in card detail (`app/detail.rs:56-62`) → `Form::comment(card_id)`
  (`forms/builders.rs:89-99`) → `Submit::Comment` → `Effect::CommentAdd` (`app/mod.rs:152-155`) →
  `client.comment_add(card_id, &body, None)` (`lib.rs:402-408`). Mirror this shape for the new effects.
- Template today: `T` on the board screen, gated by `is_empty_board()` (`app/board.rs:104-109`,
  `app/mod.rs:387-389`), hard-codes `"pipeline"`, handled at `lib.rs:409-415`. There is no picker.
- Switcher sheet: `Screen::Switcher` (`app/mod.rs:45`), `SwitcherState { level, sel, columns_sel,
  entered_at_boards, boards }` (`app/mod.rs:58-72`), rendered by `draw_switcher()`
  (`view/board.rs:249+`); the trailing `⇄ Switch board →` row is at view/board.rs:295-298 and is the
  natural anchor for a new action row.

## 3. Scope

### 3.1 Comment edit / delete / history in card detail

1. Selection: card detail currently scrolls comments as one text block. Introduce a focused-comment
   concept so a specific comment can be acted on (needed for edit/delete/history to mean anything).
   Keep the existing scroll behavior working.
2. Edit: open the existing comment form pre-filled with the focused comment's body; submit calls
   `comment_update(id, body, None)`. Reuse `Form::comment`'s field plumbing rather than a new form kind
   if it fits.
3. Delete: confirm first (there is an existing confirm overlay), then `comment_delete(id, None)`.
   Deleted comments must render visibly deleted rather than vanishing silently — `comment.get` still
   returns them and `deleted_at` is set.
4. History: a sheet listing `comment_history` snapshots for the focused comment, oldest → newest, with
   timestamps. It must scroll and it must be readable at 40 columns (one entry per row with wrapping,
   the same shape Compact help uses — see `draw_help_compact` in `view/overlays.rs`).
5. Every new interactive element must register a `Zone` in the `HitMap` and be reachable by touch, not
   only by keyboard — that is the whole point of the widget layer added in #42. New keys must appear in
   `HELP_KEYS` (`view/mod.rs`).
6. Surface daemon rejections as toasts (system comment immutable, already-deleted, agent-owned), do not
   silently no-op.

### 3.2 Templates from the switcher

1. Add a template action reachable from the switcher sheet (Compact) as well as keeping the existing
   `T` key working on the board screen.
2. Because only `"pipeline"` exists, a full picker is over-engineering unless the user asks for one in
   §6 — but do not hard-code the string in a second place; route both entry points through one effect.
3. Respect the daemon precondition in the UI (`is_empty_board()` already mirrors it) and show why the
   action is unavailable instead of hiding it silently.
4. Add the `template.apply` arm to `FakeBoardClient` so this path is testable.

## 4. Traps — read before coding

These are all verified the hard way; ignoring them costs hours.

1. **Never set `CARGO_TARGET_DIR` when running e2e.** `e2e/scripts/build.sh` expects the binary at the
   repo's default `target/release/board`; a custom target dir silently runs the suite against a stale
   binary. Use a custom target dir only for direct `cargo` invocations.
2. **e2e pane width lives in a five-column window: 60..=65.** `e2e_launch_tui` (`e2e/lib.sh`) pins
   `stty cols` (65 default, `E2E_TUI_COLS` override) — `>= 60` to stay out of Compact, `<= ~65` because
   above that a long span in the board picker corrupts `herdr pane read`. The rationale and the A/B
   evidence are in the helper's comment block. If you add a scenario that greps rendered frames, use
   the helper and grep layout-agnostic text (column *names*, not `┌ Todo · 1 · manual ─┐` chrome).
3. **Never force `stty rows` in a herdr pane.** A pane's row count is Herdr's own fixed bookkeeping;
   forcing rows desyncs the draw from Herdr's row-based replay and `pane read` returns interleaved
   garbage (looks like a rendering bug, is not).
4. **`sheet_area()` derives every mode from `main_area()`** so a sheet border can never land on the
   footer row. Do not "fix" a too-short sheet by growing it past `main_area` — footer collision is a
   bug that #42 specifically removed. Add scrolling instead.
5. **`HitMap` is rebuilt per frame but not re-scoped per screen**, so the compact board header zones
   stay registered under a fullscreen sheet. Every handler for them guards on `app.screen ==
   Screen::Board`, and `tests/mouse.rs` asserts this. **Any new `Zone` you add must either be
   screen-guarded or pushed only by the active screen's own draw call.**
6. **Snapshot tests render at 40x20 / 52x24 / 60x24 / 80x24 / 120x35.** Anything you add must be
   legible at 40 columns; regenerate with `INSTA_UPDATE=new`, review every `.snap.new` diff, and accept
   with `INSTA_UPDATE=always`. Do not accept a diff you cannot explain.
7. **Visual validation uses herdr + wezterm, never tmux** (user's explicit preference). Isolate board
   state under short `/tmp` paths via `BOARD_DB`, `BOARD_SOCKET`, `HERDR_BOARD_CONFIG`, in an ephemeral
   named herdr session — never the user's real board or `default` session. See
   `.agents/skills/herdr-board-visual-validation/`.
8. `crates/board-tui/tests/mouse.rs` contains a dead-zone matrix that clicks the center of every
   registered zone on every screen and size, and fails if a screen's `HitMap` is silently empty. Extend
   it for new zones — that test is the reason touch works.

## 5. Definition of done

All gates green, run from the worktree root:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings   # zero warnings
cargo test --workspace --all-features
python3 scripts/tests/test_docs.py
bash e2e/test-harness.sh
e2e/run-all.sh                                              # 26/26 at time of writing
```

Plus:

- Behavior tests, not only snapshots: reducer tests for the new keys/effects, `tests/mouse.rs` coverage
  for new zones, and a `tests/layout.rs`-style test for any new geometry.
- `docs/design.md` updated where TUI interactions are described (integrate, do not append an
  appendix). `docs/protocol.md` should need no change.
- One `CHANGELOG.md` line at the top of `[Unreleased]`, newest-PR-first, prefixed with the full PR
  link. Write the description first and fill in the number once the PR exists
  (`AGENTS.md:94` documents the convention).
- **`HANDOFF.md` deleted** in the same PR.
- Report honestly: if something is left out or a test is failing for a pre-existing reason, say so with
  the evidence rather than rounding up to green.

## 6. Ask the user before starting

**Q1. Comment selection in card detail:**
a) `j`/`k` move a focused-comment highlight while the section is focused (recommended)
b) A separate "comment list" sheet you open, select in, then act
c) Act only on the newest comment, no selection UI
d) (livre) …

**Q2. Where edit/delete/history hang off:**
a) Keys on the focused comment (`e` edit, `d` delete, `h` history) + tappable row actions
b) A single "actions" sheet per comment (tap the comment → sheet with Edit/Delete/History)
c) Both: keys for desktop, actions sheet in Compact
d) (livre) …

**Q3. Deleted comments:**
a) Show dimmed with a `▣ DELETED` marker, like archived cards (recommended — consistent with the
   existing archived-card treatment)
b) Hide by default, reveal with a filter toggle
c) Hide entirely (history still reachable)
d) (livre) …

**Q4. Template entry point:**
a) An action row in the switcher's Columns level + keep `T` (recommended)
b) A template picker sheet, ready for more templates later
c) Only keep `T`, nothing in the switcher (drops §3.2 to almost nothing)
d) (livre) …
