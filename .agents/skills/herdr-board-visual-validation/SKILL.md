---
name: herdr-board-visual-validation
description: Safely validate, visually audit, and prototype herdr-board TUI changes before modifying production code. Use for responsive layout, cards, status colors, popup/form/detail interactions, keyboard/mouse behavior, Herdr plugin integration, ratatui snapshots, disposable live sessions, current-vs-proposed comparisons, or pre-PR verification of herdr-board. Captures via WezTerm screenshots where the WezTerm CLI is reachable, or a PTY + pyte capture on WSL2 and other machines without it.
---

# Herdr Board Visual Validation

Prototype and validate UI changes without touching the user's real Herdr workspaces, board database, installed plugin, or main checkout.

Read [`references/playbook.md`](references/playbook.md) before executing live Herdr/WezTerm work. It contains verified commands, cleanup order, and failure recovery.

## Non-negotiable safety

1. Read repository `AGENTS.md`, `skill/SKILL.md`, `docs/herdr.md`, and `docs/testing.md` completely. Treat `skill/SKILL.md` as the source of truth for operating the board; do not duplicate its general CLI/TUI reference here.
2. Verify the installed Herdr with `herdr --version`, `herdr status`, `herdr api schema --json`, and relevant `--help`; never guess command shapes.
3. Mutate only an ephemeral named Herdr session and workspaces created inside it. Prefix every mutation log with `HERDR MUTATION:`.
4. ALWAYS isolate board state — DB, socket, AND daemon — under a short `/tmp` directory via `BOARD_DB`, `BOARD_SOCKET`, and `HERDR_BOARD_CONFIG`. No exceptions: trivial prototypes, "quick peeks", and one-off TUI runs all use the isolated env; the user's real board database and daemon are never a valid target. `board tui` with an isolated `BOARD_SOCKET` auto-starts its own isolated daemon. Keep this rule identical to the board's Prototyping-column prompt. When a prototype is runnable/visible, open it in a new wezterm tab (`wezterm cli spawn --cwd <src> -- bash -lc '<run cmd>'`; the spawned process inherits the isolated env) so the user can validate interactively before approval.
5. Choose the validation mode before changing files:
   - **Prototype mode:** prototype in a detached temporary worktree under `/tmp`; keep the implementation checkout unchanged until approval.
   - **Execution-validation mode:** validate an existing implementation worktree; do not create a second implementation worktree or copy production changes into another branch.
6. Never dispatch a paid/real agent for visual fixtures. Use `FakeBoardClient`, the fake harness, CLI-created manual cards, or direct writes only to the isolated fixture database.
7. Capture every PID/resource needed for cleanup. Never use broad `pkill` patterns — `pkill -f "board.sock"` matches the invoking shell's own command line and kills the session. Nominate the PID with `lsof -t "$TMP/board.sock"`, confirm it via `/proc/<pid>/cmdline`, then signal that exact PID.

## Workflow

### 1. Establish the baseline

- Require or record the main checkout status; do not overwrite unrelated changes.
- Confirm installed plugin version/root/commit with `herdr plugin list --plugin herdr-board --json`.
- Read `crates/board-tui/src/{app,view,testkit}.rs` and existing snapshots.
- Run the current snapshot suite.
- Capture baseline states at the same terminal dimensions planned for the prototype.
- Record Herdr version/protocol, terminal dimensions, theme, and fixture data.

### 2. Prepare the isolated validation source

In **prototype mode**:

- Create a detached worktree under `/tmp` from the current commit.
- Make only disposable prototype changes there; do not create a prototype under the repository's `worktree/` directory.

In **execution-validation mode**:

- Use the existing execution worktree as the source and implementation target.
- Do not create another Git worktree for the implementation. Temporary build, board, Herdr, and capture artifacts still belong under `/tmp`.

In either mode:

- Build into a separate `CARGO_TARGET_DIR`; make the selected source manifest resolve that binary without replacing the main build.
- Start an ephemeral named Herdr server with isolated board env.
- Link the selected source plugin only inside that session.
- Create a disposable workspace and open the plugin through its real action/placement.
- Attach the disposable session in a temporary WezTerm tab after unsetting nested-Herdr environment variables. Without a WezTerm CLI, skip the tab and drive the binary under a PTY instead (playbook section 5b); the same variables must still be unset.

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

Pick the capture route with `command -v wezterm` — never with `TERM_PROGRAM`, which stays `WezTerm` in WSL2 even when no CLI exists:

- **WezTerm CLI available** (macOS, native Linux): playbook sections 4–5. Save plain terminal text and attributed ANSI (`wezterm cli get-text --escapes`); on macOS also capture PNGs with `screencapture` after permission is granted.
- **No WezTerm CLI** (WSL2 with WezTerm on the Windows host): playbook section 5b. Drive the release binary under a PTY at an exact size and read the final screen and per-cell attributes with `pyte`, via `references/pty-capture.py`. Do not try the host `wezterm.exe`; it cannot reach the Windows-namespace mux socket.

In both routes:

- Use identical viewport dimensions and fixture content for baseline and proposal.
- Create a local side-by-side HTML page with clearly labeled current/proposed images.
- Keep each feedback round focused: layout, cards, detail, overlays, then polish.

Do not infer contrast from text snapshots alone; inspect attributed ANSI, PTY cell attributes, or a real screenshot under the user's terminal palette.

The PTY route renders under a synthetic 256-color model: it proves emitted attributes and geometry, not the user's real palette or font. When the question is whether something *looks* right, hand the interactive board to the user instead of claiming visual approval.

### 5. Iterate without promoting

- In prototype mode, apply feedback only in the disposable prototype worktree.
- In execution-validation mode, apply fixes directly to the existing execution worktree and repeat isolated validation; never create a second implementation worktree.
- Add reducer/layout tests for behavior, not only screenshots.
- Run focused tests and clippy after each interaction change.
- Rebuild and restart the disposable plugin pane so screenshots use the new binary.
- Preserve the approved final prototype diff until promotion.

### 6. Promote after explicit approval

Promotion applies only to **prototype mode**. The target is the designated implementation checkout or execution worktree, not necessarily the main checkout. Execution-validation mode already validates its implementation target and must not create or port into another worktree.

1. Add/apply behavior tests to the implementation target first and run them red.
2. Port the approved source changes from the disposable prototype.
3. Update/add deterministic snapshots, including wide/narrow and overflow states.
4. Update README, design docs, and `CHANGELOG.md` in the same change.
5. Run all repository gates and live e2e.
6. Review the final diff for accidental prototype paths, fixture data, or generated artifacts.

Required gates:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace --all-features
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
e2e/run-all.sh
```

Use `~/.cargo/bin/cargo` or prepend it to `PATH` if non-login shells cannot find Cargo.

### 7. Clean up and prove cleanup

- Close the temporary WezTerm pane/tab, or in the PTY route remove the `pyte` venv and confirm no PTY child survives.
- Close disposable workspaces.
- Stop the isolated board daemon by its captured PID/socket owner.
- Stop and delete the named Herdr session.
- Verify no `hb-visual-*`, `hb-prototype-*`, or `hb-e2e-*` session remains.
- In prototype mode, remove the disposable linked worktree with `git worktree remove --force` only after its approved diff is promoted or intentionally discarded. Never remove the user's execution worktree during validation cleanup.
- Recheck the original checkout and implementation-target `git status`.

## Handoff/report format

Return:

1. baseline and proposed behavior;
2. artifact/screenshot paths;
3. approved decisions and unresolved questions;
4. files changed;
5. exact test/gate results;
6. live Herdr/e2e result and cleanup proof;
7. whether changes are merely prototyped, promoted, committed, or installed.
