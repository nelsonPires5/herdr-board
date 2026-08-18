# Visual validation (TUI) — reference

Part of the `development-workflow` skill. Use this stage when the change
touches the TUI's visible behavior: responsive layout, card/status colors,
popup/form/detail interactions, keyboard/mouse behavior, Herdr plugin
integration, ratatui snapshots, or pre-PR visual verification.

Two routes exist; **Route A (through the sandbox) is preferred** whenever
Docker/Colima is available. Route B (host-isolated) exists only as a fallback
when no Docker/Colima is present — it must never become the default.

Read `playbook.md` before executing live Herdr/WezTerm work in Route B: it
contains verified commands, cleanup order, and failure recovery. The
no-WezTerm-CLI capture helper is `pty-capture.py`.

## Non-negotiable safety

1. Read repository `AGENTS.md`, `docs/herdr.md`, and `docs/testing.md`
   completely (repo-root-relative; `../../../AGENTS.md` also resolves from this
   directory). Treat the board docs as the source of truth for operating the
   board; do not duplicate the general CLI/TUI reference here.
2. Verify the installed Herdr with `herdr --version`, `herdr status`,
   `herdr api schema --json`, and relevant `--help`; never guess command
   shapes. In the sandbox this is the pinned, build-time-verified Herdr 0.8.0
   (protocol 19) baked into the image.
3. Mutate only an ephemeral named Herdr session and workspaces created inside
   it. Prefix every mutation log with `HERDR MUTATION:`.
4. **ALWAYS isolate board state — DB, socket, AND daemon.** Route A isolates by
   construction (container-local, inside the sandbox state volume). Route B
   isolates under a short `/tmp` directory via `BOARD_DB`, `BOARD_SOCKET`, and
   `HERDR_BOARD_CONFIG`. No exceptions: trivial prototypes, "quick peeks", and
   one-off TUI runs all use isolated state; the user's real board database and
   daemon are never a valid target. `board tui` with an isolated `BOARD_SOCKET`
   auto-starts its own isolated daemon. When a prototype is runnable/visible,
   open it in a new WezTerm tab so the user can validate interactively before
   approval.
5. Choose the mode before changing files:
   - **Prototype mode:** prototype in a detached temporary worktree under
     `/tmp`; keep the implementation checkout unchanged until approval.
   - **Execution-validation mode:** validate an existing implementation
     worktree; do not create a second implementation worktree or copy
     production changes into another branch.
6. Never dispatch a paid/real agent for visual fixtures. Use `FakeBoardClient`,
   the fake harness, CLI-created manual cards, or direct writes only to the
   isolated fixture database.
7. Capture every PID/resource needed for cleanup. Never use broad `pkill`
   patterns — `pkill -f "board.sock"` matches the invoking shell's own command
   line and kills the session. Nominate the PID with `lsof -t "$TMP/board.sock"`
   (Route B) or stop the sandbox env container with `scripts/sandbox.sh down`
   (Route A), then confirm, then signal that exact PID.

## Workflow

### 1. Establish the baseline

- In Route A, ensure the sandbox is ready: `scripts/sandbox.sh prepare`
  (once per worktree), then `scripts/sandbox.sh selfcheck` to prove isolation.
- Confirm the installed plugin version/root/commit
  (`herdr plugin list --plugin herdr-board --json` inside the sandbox shell /
  env container).
- Read `crates/board-tui/src/{app,view,testkit}.rs` and existing snapshots.
- Run the current snapshot suite — in Route A this is part of
  `scripts/sandbox.sh gates` (`cargo test --workspace --all-features`).
- Capture baseline states at the same terminal dimensions planned for the
  prototype.
- Record Herdr version/protocol, terminal dimensions, theme, and fixture data.

### 2. Prepare the isolated validation source

In **prototype mode**:

- Create a detached worktree under `/tmp` from the current commit.
- Make only disposable prototype changes there; do not create a prototype under
  the repository's `worktree/` directory.

In **execution-validation mode**:

- Use the existing execution worktree as the source and implementation target.
- Do not create another Git worktree for the implementation. Temporary build,
  board, Herdr, and capture artifacts still belong under `/tmp` (Route B) or in
  the sandbox volumes (Route A).

In either mode:

- Build into an isolated `CARGO_TARGET_DIR`. In Route A the sandbox already
  builds into the `/repo/target` volume (which keeps the herdr plugin contract
  `./target/release/board` working, see `docs/sandbox.md`). In Route B build
  into a separate `$TMP`-based target and make the selected source manifest
  resolve that binary without replacing the main build.
- Route A: start the environment container (auto: `scripts/sandbox.sh tui`,
  `shell`, or `board` starts it) with its container-local Herdr server and
  auto-started daemon.
- Start an ephemeral named Herdr session (Route A: inside the env container;
  Route B: host) with isolated board env.
- Link the selected source plugin only inside that session.
- Create a disposable workspace and open the plugin through its real
  action/placement.
- Attach the disposable session in a temporary WezTerm tab after unsetting
  nested-Herdr environment variables. Without a WezTerm CLI, skip the tab and
  drive the binary under a PTY instead (`pty-capture.py`); the same variables
  must still be unset.

Use the exact sequence in `playbook.md` (Route B) or the sandbox equivalents
above (Route A).

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

Prefer CLI creation. In Route A use `scripts/sandbox.sh board …` (e.g.
`board board create`, `board card create --board <id> …`, `board card list
--json`) — the board DB lives in the sandbox state volume. Direct SQLite
writes are permitted only against the isolated fixture DB (Route A: the state
volume's DB, via `docker exec`; Route B: the `/tmp` fixture DB) and only for
display states unavailable through public commands.

### 4. Capture comparable evidence

- **Route A sandbox TUI:** `scripts/sandbox.sh tui` in a new WezTerm tab
  (`wezterm cli spawn --cwd <repo> -- bash -lc 'scripts/sandbox.sh tui'`),
  which gives a real attached terminal with working resize and clean exit
  (`q`). Capture with `wezterm cli get-text --escapes` (and `screencapture`
  PNGs on macOS after permission) — the pane is host-side, so captures work
  unchanged.
- **No WezTerm CLI** (WSL2 with WezTerm on the Windows host): drive the binary
  under a PTY at an exact size and read the final screen and per-cell
  attributes with `pyte`, via `pty-capture.py`. Do not try the host
  `wezterm.exe`; it cannot reach the Windows-namespace mux socket.
- **Route B fallback:** `playbook.md` sections 4–5.

Use identical viewport dimensions and fixture content for baseline and
proposal. Create a local side-by-side HTML page with clearly labeled
current/proposed images. Keep each feedback round focused: layout, cards,
detail, overlays, then polish.

> **Palette fidelity (Route A):** the sandbox renders under a neutral
> 256-color model (`TERM=xterm-256color`) and the container's palette — it
> proves emitted attributes, geometry, and interactions, not the user's real
> palette or font. When the question is whether something *looks* right under
> the user's terminal, hand the attached sandbox TUI to the human instead of
> claiming visual approval from the capture. (This mirrors the existing PTY
> caveat and applies to Route B's PTY route as well.)

Do not infer contrast from text snapshots alone; inspect attributed ANSI, PTY
cell attributes, or a real screenshot under the user's terminal palette.

### 5. Iterate without promoting

- In prototype mode, apply feedback only in the disposable prototype worktree.
- In execution-validation mode, apply fixes directly to the existing execution
  worktree and repeat isolated validation; never create a second
  implementation worktree.
- Add reducer/layout tests for behavior, not only screenshots.
- Run focused tests and clippy after each interaction change (Route A:
  `scripts/sandbox.sh gates <filter>` for e2e subsets, or the full `gates`
  before handoff).
- Rebuild and restart the disposable plugin pane/TUI so screenshots use the
  new binary.
- Preserve the approved final prototype diff until promotion.

### 6. Promote after explicit approval

Promotion applies only to **prototype mode**. The target is the designated
implementation checkout or execution worktree, not necessarily the main
checkout. Execution-validation mode already validates its implementation
target and must not create or port into another worktree.

1. Add/apply behavior tests to the implementation target first and run them
   red.
2. Port the approved source changes from the disposable prototype.
3. Update/add deterministic snapshots, including wide/narrow and overflow
   states.
4. Update README, design docs, and `CHANGELOG.md` in the same change.
5. Run all repository gates and live e2e — in Route A,
   `scripts/sandbox.sh gates` (fmt → clippy → `cargo test --workspace
   --all-features` → python tier → `e2e/test-harness.sh` → `run-all.sh
   --require-all`).
6. Review the final diff for accidental prototype paths, fixture data, or
   generated artifacts.

### 7. Clean up and prove cleanup

- Close the temporary WezTerm pane/tab, or in the PTY route remove the `pyte`
  venv and confirm no PTY child survives.
- Close disposable workspaces.
- Route A: `scripts/sandbox.sh down` stops and removes the environment
  container (its volumes are per-worktree and disposable; `reset` drops them).
- Route B: stop the isolated board daemon by its captured PID/socket owner;
  stop and delete the named Herdr session.
- Verify no `hb-visual-*`, `hb-prototype-*`, or `hb-e2e-*` session remains.
- In prototype mode, remove the disposable linked worktree with
  `git worktree remove --force` only after its approved diff is promoted or
  intentionally discarded. Never remove the user's execution worktree during
  validation cleanup.
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
