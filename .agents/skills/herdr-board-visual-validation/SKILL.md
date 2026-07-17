---
name: herdr-board-visual-validation
description: Safely validate, visually audit, and prototype herdr-board TUI changes before modifying production code. Use for responsive layout, cards, status colors, popup/form/detail interactions, keyboard/mouse behavior, Herdr plugin integration, WezTerm screenshots, ratatui snapshots, disposable live sessions, current-vs-proposed comparisons, or pre-PR verification of herdr-board.
---

# Herdr Board Visual Validation

Prototype and validate UI changes without touching the user's real Herdr workspaces, board database, installed plugin, or main checkout.

Read [`references/playbook.md`](references/playbook.md) before executing live Herdr/WezTerm work. It contains verified commands, cleanup order, and failure recovery.

## Non-negotiable safety

1. Read repository `AGENTS.md`, `docs/herdr.md`, and `docs/testing.md` completely.
2. Verify the installed Herdr with `herdr --version`, `herdr status`, `herdr api schema --json`, and relevant `--help`; never guess command shapes.
3. Mutate only an ephemeral named Herdr session and workspaces created inside it. Prefix every mutation log with `HERDR MUTATION:`.
4. Isolate board state under a short `/tmp` directory via `BOARD_DB`, `BOARD_SOCKET`, and `HERDR_BOARD_CONFIG`.
5. Prototype in a detached temporary worktree. Keep the main checkout unchanged until the user approves the comparison.
6. Never dispatch a paid/real agent for visual fixtures. Use `FakeBoardClient`, the fake harness, CLI-created manual cards, or direct writes only to the isolated fixture database.
7. Capture every PID/resource needed for cleanup. Never use broad `pkill` patterns.

## Workflow

### 1. Establish the baseline

- Require or record the main checkout status; do not overwrite unrelated changes.
- Confirm installed plugin version/root/commit with `herdr plugin list --plugin herdr-board --json`.
- Read `crates/board-tui/src/{app,view,testkit}.rs` and existing snapshots.
- Run the current snapshot suite.
- Capture baseline states at the same terminal dimensions planned for the prototype.
- Record Herdr version/protocol, terminal dimensions, theme, and fixture data.

### 2. Create an isolated prototype

- Create a detached worktree under `/tmp` from the current commit.
- Build into a separate `CARGO_TARGET_DIR`; make the worktree manifest resolve that binary without replacing the main build.
- Start an ephemeral named Herdr server with isolated board env.
- Link the worktree plugin only inside that session.
- Create a disposable workspace and open the plugin through its real action/placement.
- Attach the disposable session in a temporary WezTerm tab after unsetting nested-Herdr environment variables.

Use the exact sequence in the playbook.

### 3. Build visual fixtures

Exercise at least:

- empty board;
- several columns at narrow and wide widths;
- long titles;
- idle/running/queued/blocked/failed cards;
- selected card contrast;
- new/edit card form;
- picker and confirmation;
- help;
- card detail popup and fullscreen;
- short and overflowing comments/runs;
- keyboard and mouse behavior.

Prefer CLI creation. Direct SQLite writes are permitted only against the isolated fixture DB and only for display states unavailable through public commands.

### 4. Capture comparable evidence

- Use identical viewport dimensions and fixture content for baseline and proposal.
- Save plain terminal text and attributed ANSI (`wezterm cli get-text --escapes`).
- On macOS, capture PNGs with `screencapture` after permission is granted.
- Create a local side-by-side HTML page with clearly labeled current/proposed images.
- Keep each feedback round focused: layout, cards, detail, overlays, then polish.

Do not infer contrast from text snapshots alone; inspect attributed ANSI or a real screenshot under the user's terminal palette.

### 5. Iterate without promoting

- Apply feedback only in the prototype worktree.
- Add reducer/layout tests for behavior, not only screenshots.
- Run focused tests and clippy after each interaction change.
- Rebuild and restart the disposable plugin pane so screenshots use the new binary.
- Preserve the approved final prototype diff until promotion.

### 6. Promote after explicit approval

1. Add/apply behavior tests to the main checkout first and run them red.
2. Port the approved source changes.
3. Update/add deterministic snapshots, including wide/narrow and overflow states.
4. Update README, design docs, and `CHANGELOG.md` in the same change.
5. Run all repository gates and live e2e.
6. Review the final diff for accidental prototype paths, fixture data, or generated artifacts.

Required gates:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace --all-features
e2e/run-all.sh
```

Use `~/.cargo/bin/cargo` or prepend it to `PATH` if non-login shells cannot find Cargo.

### 7. Clean up and prove cleanup

- Close the temporary WezTerm pane/tab.
- Close disposable workspaces.
- Stop the isolated board daemon by its captured PID/socket owner.
- Stop and delete the named Herdr session.
- Verify no `hb-visual-*`, `hb-prototype-*`, or `hb-e2e-*` session remains.
- Remove the linked worktree with `git worktree remove --force` only after its approved diff is promoted or intentionally discarded.
- Recheck main `git status`.

## Handoff/report format

Return:

1. baseline and proposed behavior;
2. artifact/screenshot paths;
3. approved decisions and unresolved questions;
4. files changed;
5. exact test/gate results;
6. live Herdr/e2e result and cleanup proof;
7. whether changes are merely prototyped, promoted, committed, or installed.
