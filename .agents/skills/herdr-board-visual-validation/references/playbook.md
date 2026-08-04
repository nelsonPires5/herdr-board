# Reproducible Herdr Board Prototype Playbook

## Contents

1. Preflight
2. Isolated validation source and build
3. Disposable Herdr/board stack
4. Temporary WezTerm client
5. Fixtures and captures
5b. WSL2 / no-WezTerm alternative to sections 4–5 (PTY + pyte)
6. Rebuild/restart loop
7. Promotion
8. Cleanup and recovery
9. Common failures

Commands below were exercised with Herdr 0.8.0 / protocol 19. This repository
requires that exact pair; re-verify the installed CLI and session socket before use.

## 1. Preflight

```bash
REPO="$(git rev-parse --show-toplevel)"
cd "$REPO"
git status --short
test "$(herdr --version)" = "herdr 0.8.0"
herdr status
herdr api schema --json | jq -e 'select(.protocol == 19) | {protocol,schema_version}'
herdr plugin list --plugin herdr-board --json | jq '.result.plugins[0] | {version,plugin_root,source}'
~/.cargo/bin/cargo test -p board-tui --features fake-client --test snapshots
```

Read `AGENTS.md`, `docs/herdr.md`, and `docs/testing.md`. Do not continue if proposed commands would target a user workspace/session.

Record terminal state:

```bash
env | grep -E '^(TERM|COLORTERM|TERM_PROGRAM|WEZTERM_)' | sort
herdr api snapshot | jq '.result.snapshot | {version,protocol,workspaces,tabs,layouts}'
```

The snapshot command is read-only against the current session. Do not mutate that session.

## 2. Isolated validation source and build

Choose one mode. Prototype mode creates a disposable detached worktree. Execution-validation mode reads changes from the user's existing execution worktree and mirrors them into a disposable non-Git plugin/build directory; fixes remain in the execution worktree, and no second implementation worktree is created.

```bash
VALIDATION_MODE="${VALIDATION_MODE:-prototype}" # prototype | execution
RUN_ID="$$"
HERDR_BIN="$(command -v herdr)"
# Reuse the standard E2E identity/name helpers without starting its stack.
# shellcheck disable=SC1091
. "$REPO/e2e/lib.sh"
SESSION="$(e2e_session_name 'hb-visual-')"
e2e_session_name_absent "$SESSION"
TMP="$(mktemp -d /tmp/hb-visual.XXXXXX)"
WT="/tmp/herdr-board-visual-$RUN_ID"
TARGET="/tmp/herdr-board-target-$RUN_ID"
STATE="/tmp/hb-visual-$RUN_ID.env"

case "$VALIDATION_MODE" in
  prototype)
    git worktree add --detach "$WT" HEAD
    ;;
  execution)
    : "${EXECUTION_WORKTREE:?set EXECUTION_WORKTREE to the existing implementation worktree}"
    mkdir -p "$WT"
    rsync -a --delete --exclude .git --exclude target \
      "$EXECUTION_WORKTREE/" "$WT/"
    ;;
  *) echo "unknown VALIDATION_MODE: $VALIDATION_MODE" >&2; exit 2 ;;
esac

CARGO_TARGET_DIR="$TARGET" ~/.cargo/bin/cargo build \
  --manifest-path "$WT/Cargo.toml" --release -p board-cli
ln -s "$TARGET" "$WT/target"

cat >"$STATE" <<EOF
REPO=$REPO
HERDR_BIN=$HERDR_BIN
SESSION=$SESSION
TMP=$TMP
WT=$WT
TARGET=$TARGET
VALIDATION_MODE=$VALIDATION_MODE
EXECUTION_WORKTREE=${EXECUTION_WORKTREE:-}
EOF
```

The symlink satisfies `herdr-plugin.toml`'s relative `./target/release/board` without overwriting the original or execution checkout's binary.

For a baseline/proposal comparison, capture the baseline before editing. In prototype mode, edit the disposable worktree. In execution-validation mode, edit only `$EXECUTION_WORKTREE`, refresh the disposable mirror with the same `rsync` command, then rebuild and recapture with identical fixtures and dimensions.

## 3. Disposable Herdr/board stack

Start a named server whose children inherit isolated board paths:

```bash
printf 'HERDR MUTATION: start disposable session %s\n' "$SESSION"
env \
  BOARD_DB="$TMP/board.db" \
  BOARD_SOCKET="$TMP/board.sock" \
  HERDR_BOARD_CONFIG="$TMP/config.toml" \
  herdr --session "$SESSION" server >"$TMP/herdr-server.log" 2>&1 &
SERVER_PID=$!
printf 'SERVER_PID=%s\n' "$SERVER_PID" >>"$STATE"

for _ in $(seq 1 50); do
  herdr session list --json 2>/dev/null | jq -e --arg s "$SESSION" \
    '.sessions[] | select(.name==$s and .running==true)' >/dev/null && break
  sleep .1
done
herdr session list --json | jq -e --arg s "$SESSION" \
  '.sessions[] | select(.name==$s and .running==true)' >/dev/null

SERVER_IDENTITY=""
for _ in $(seq 1 20); do
  SERVER_IDENTITY="$(e2e_process_identity_capture \
    "$SERVER_PID" "$SESSION" "$SESSION" "$HERDR_BIN" 2>/dev/null || true)"
  [ -n "$SERVER_IDENTITY" ] && break
  sleep .05
done
if [ -z "$SERVER_IDENTITY" ]; then
  # Capture failed, but this remains the direct child just spawned by this shell.
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  echo "refusing prototype stack without a Herdr process-identity token" >&2
  exit 1
fi
printf 'SERVER_IDENTITY=%q\n' "$SERVER_IDENTITY" >>"$STATE"
```

Link and open only inside that session:

```bash
printf 'HERDR MUTATION: link prototype plugin\n'
herdr --session "$SESSION" plugin link "$WT"

printf 'HERDR MUTATION: create disposable workspace\n'
WS_JSON="$(herdr --session "$SESSION" workspace create \
  --cwd "$REPO" --label hb-visual \
  --env BOARD_DB="$TMP/board.db" \
  --env BOARD_SOCKET="$TMP/board.sock" \
  --env HERDR_BOARD_CONFIG="$TMP/config.toml" \
  --focus)"
WS="$(printf '%s' "$WS_JSON" | jq -r '.result.workspace.workspace_id')"
printf 'WS=%s\n' "$WS" >>"$STATE"

printf 'HERDR MUTATION: invoke real plugin action (opens protocol-19 plugin pane)\n'
herdr --session "$SESSION" plugin action invoke open-board --plugin herdr-board

BOARD_PID="$(lsof -t "$TMP/board.sock" 2>/dev/null | head -1 || true)"
[ -n "$BOARD_PID" ] || { echo "board daemon pid not found" >&2; exit 1; }
BOARD_IDENTITY="$(e2e_process_identity_capture \
  "$BOARD_PID" daemon daemon "$WT/target/release/board")" \
  || { echo "refusing unowned board daemon" >&2; exit 1; }
printf 'BOARD_PID=%q\nBOARD_IDENTITY=%q\n' \
  "$BOARD_PID" "$BOARD_IDENTITY" >>"$STATE"
```

`lsof` only discovers the candidate PID; the full `/proc` token authorizes any later signal.

Verify placement and process paths:

```bash
herdr --session "$SESSION" api snapshot | jq \
  '.result.snapshot | {focused_pane_id,workspaces,tabs,panes,layouts}'
herdr --session "$SESSION" plugin log list --plugin herdr-board --limit 10
```

## 4. Temporary WezTerm client

Run the detection in section 5b first. Sections 4 and 5 require a reachable
WezTerm CLI on the same OS as the terminal (macOS, native Linux). If
`command -v wezterm` fails, skip to section 5b instead.

A coding agent running inside Herdr inherits `HERDR_ENV` and IDs. Unset them to avoid nested-Herdr rejection. Also unset `WEZTERM_UNIX_SOCKET`; permission changes may restart WezTerm and make the inherited socket stale.

```bash
WINDOW_ID="$(env -u WEZTERM_UNIX_SOCKET wezterm cli list --format json |
  jq -r 'map(select(.is_active))[0].window_id')"
CMD="exec env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID \
-u HERDR_WORKSPACE_ID -u HERDR_SOCKET_PATH herdr --session '$SESSION'"

PANE="$(env -u WEZTERM_UNIX_SOCKET wezterm cli spawn \
  --window-id "$WINDOW_ID" --cwd "$REPO" -- bash -lc "$CMD")"
printf 'PANE=%s\n' "$PANE" >>"$STATE"
env -u WEZTERM_UNIX_SOCKET wezterm cli set-tab-title \
  --pane-id "$PANE" 'HB visual audit'
env -u WEZTERM_UNIX_SOCKET wezterm cli activate-pane --pane-id "$PANE"
```

Capture terminal text:

```bash
env -u WEZTERM_UNIX_SOCKET wezterm cli get-text --pane-id "$PANE" \
  >"$TMP/visible.txt"
env -u WEZTERM_UNIX_SOCKET wezterm cli get-text --pane-id "$PANE" --escapes \
  >"$TMP/visible.ansi"
```

Send TUI input:

```bash
env -u WEZTERM_UNIX_SOCKET wezterm cli send-text \
  --pane-id "$PANE" --no-paste 'T'
env -u WEZTERM_UNIX_SOCKET wezterm cli send-text \
  --pane-id "$PANE" --no-paste $'j\r'
```

## 5. Fixtures and captures

Define an isolated CLI helper in each shell invocation:

```bash
board() {
  env BOARD_DB="$TMP/board.db" BOARD_SOCKET="$TMP/board.sock" \
    HERDR_BOARD_CONFIG="$TMP/config.toml" \
    "$TARGET/release/board" "$@"
}
board status --json
```

Create cards in a manual column. Avoid moving into auto columns unless using the repository's fake harness/e2e stack.

To display otherwise unreachable statuses, direct SQLite writes are acceptable only for this isolated `$TMP/board.db`:

```bash
python3 - "$TMP/board.db" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1])
for card_id, status in [(1,'idle'),(2,'running'),(3,'queued'),(4,'blocked'),(5,'failed')]:
    con.execute("UPDATE cards SET status=?, updated_at=datetime('now','-125 seconds') WHERE id=?",
                (status, card_id))
con.commit()
PY
```

Refresh with `r`. Build overflow fixtures with comments/runs in the isolated stack; verify the newest item is at the bottom and scroll arrows change at top/middle/bottom.

Capture macOS screen after Screen Recording permission is granted:

```bash
mkdir -p "$TMP/shots"
/usr/sbin/screencapture -x "$TMP/shots/state.png"
```

Do not hardcode crop coordinates across machines. Keep full-screen evidence or calculate the active window bounds with an approved automation method.

For deterministic text evidence:

```bash
INSTA_UPDATE=new ~/.cargo/bin/cargo test \
  -p board-tui --features fake-client --test snapshots
```

Review `.snap.new` before accepting with `INSTA_UPDATE=always`.

## 5b. WSL2 / no-WezTerm alternative to sections 4–5 (PTY + pyte)

Use this section instead of sections 4 and 5's WezTerm client and
`get-text --escapes` captures when the WezTerm CLI is unreachable — the normal
case in WSL2 with WezTerm running on the Windows host. Sections 1–3, 6, 7 and 8
still apply unchanged, minus the WezTerm pane they create and kill.

### 5b.1 Detection

```bash
if command -v wezterm >/dev/null 2>&1; then
  echo "wezterm CLI available: use sections 4-5"
else
  echo "no wezterm CLI: use the PTY flow below"
fi
```

`command -v wezterm` is the only signal. Verified facts behind that rule:

- `TERM_PROGRAM=WezTerm` is still exported inside WSL2, so it does **not** mean
  the CLI works. Never branch on `TERM_PROGRAM`.
- `WEZTERM_UNIX_SOCKET` is unset in WSL2.
- The host binary at `/mnt/c/Program Files/WezTerm/wezterm.exe` exists and is
  **unusable**: it exits with
  `failed to connect to Socket("gui-sock-<pid>"); terminating`, because the GUI
  mux socket lives in the Windows namespace. Do not attempt the `.exe`.

### 5b.2 One-time dependency

`pyte` decodes the attributed output. System-wide `pip install` is blocked here
by PEP 668 (externally-managed environment), so use a venv:

```bash
python3 -m venv /tmp/hb-venv
/tmp/hb-venv/bin/pip install -q pyte
PY_BIN=/tmp/hb-venv/bin/python3
```

### 5b.3 Capture with the PTY harness

[`pty-capture.py`](pty-capture.py) forks the real release binary under a PTY at
an exact size, drives keys, and renders the final screen with `pyte`.

```bash
CAP="$REPO/.agents/skills/herdr-board-visual-validation/references/pty-capture.py"
mkdir -p "$TMP/shots"
BOARD_BIN="$TARGET/release/board" "$PY_BIN" "$CAP" "$TMP" 120 40 "$TMP/shots/wide"
BOARD_BIN="$TARGET/release/board" "$PY_BIN" "$CAP" "$TMP" 80 24 "$TMP/shots/narrow"
# keyboard/mouse sequences: TAB ENTER ESC UP DOWN LEFT RIGHT, WHEELUP:n, WHEELDOWN:n
BOARD_BIN="$TARGET/release/board" "$PY_BIN" "$CAP" "$TMP" 120 40 \
  "$TMP/shots/detail-scrolled" ENTER DOWN DOWN WHEELDOWN:3
```

Each run writes `<out>.txt` (final screen grid), `<out>.attrs` (per-cell
attributes) and `<out>.ansi` (raw stream). Non-negotiables the harness already
encodes — keep them if you write your own driver:

- Isolated stack only: `BOARD_DB`, `BOARD_SOCKET`, `HERDR_BOARD_CONFIG` under
  the short `/tmp/hb-*.XXXXXX` `$TMP` (AF_UNIX path length matters). Set
  `TERM=xterm-256color`, `COLORTERM=truecolor`. Unset `HERDR_ENV`,
  `HERDR_PANE_ID`, `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID`, `HERDR_SOCKET_PATH`
  so a nested-Herdr agent session does not leak in.
- Set the size with
  `fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))`.
  ratatui sizes from the PTY; `LINES`/`COLUMNS` alone is not sufficient.
- Drain on a `select` loop with a settle budget — ~2.5s after startup, ~0.8s
  after each key. The TUI needs time to redraw. Override with
  `STARTUP_SETTLE` / `KEY_SETTLE`.
- Point the wheel inside the target section with `WHEEL_COL` / `WHEEL_ROW`
  (SGR: `\x1b[<64;COL;ROWM` = wheel up, `65` = wheel down).

### 5b.4 Assert on the final screen, never on the raw stream

The `.ansi` stream is **cumulative across frames**: grepping it for a marker
finds stale frames from earlier redraws. `pyte.Screen(cols, rows)` replays the
stream and gives the end state, which is the only correct basis for
"exactly one marker is rendered".

```bash
grep -c 'RUNNING' "$TMP/shots/wide.txt"                 # correct: final screen
grep -c 'RUNNING' "$TMP/shots/wide.ansi"                # WRONG: counts stale frames
grep -P '\tRUNNING?' "$TMP/shots/wide.attrs" | head     # per-cell attributes
```

`<out>.attrs` rows come from `screen.buffer[y][x]`: `.fg`, `.bg`, `.bold`,
`.reverse`. `.fg` is a hex string such as `cdcd00`. These are the real emitted
attributes and replace `wezterm cli get-text --escapes` from section 4.

### 5b.5 Fixtures: inject run history into the isolated DB

Section 5's `board` helper still applies, with two CLI shapes worth recording:
`board column create --name <N>` and `board card create --title <T>` take
**named flags, not positionals**.

Varied run rows are only practical through direct writes to the isolated
`$TMP/board.db`. Schema constraints, verified:

- `runs` requires non-NULL `card_id`, `column_id`, `harness`, `argv_json`,
  `prompt_snapshot`.
- `outcome` is CHECK-constrained to NULL / `ok` / `fail` / `cancelled` / `lost`.
- A partial unique index allows only one run per card with `ended_at IS NULL`.

```bash
python3 - "$TMP/board.db" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1])
rows = [
    (1, 1, 'fake', '["fake"]', 'prompt', "datetime('now','-600 seconds')", 'ok'),
    (1, 1, 'fake', '["fake"]', 'prompt', "datetime('now','-300 seconds')", 'fail'),
    (1, 1, 'fake', '["fake"]', 'prompt', "datetime('now','-60 seconds')", None),
]
for card, col, harness, argv, prompt, started, outcome in rows:
    ended = "NULL" if outcome is None else "datetime('now')"
    con.execute(
        f"INSERT INTO runs (card_id, column_id, harness, argv_json,"
        f" prompt_snapshot, started_at, ended_at, outcome)"
        f" VALUES (?,?,?,?,?,{started},{ended},?)",
        (card, col, harness, argv, prompt, outcome))
con.commit()
PY
```

Re-verify column names against the live schema first
(`sqlite3 "$TMP/board.db" '.schema runs'`); adjust rather than guess.

### 5b.6 Cleanup: never `pkill -f`

SKILL.md rule 7 concretely: `pkill -f "board.sock"` **matched the invoking
shell's own command line and killed the session** before cleanup ran, because
the shell's argv contains the whole script text. There is no WezTerm pane to
kill in this flow; stop the daemon by exact PID:

```bash
BOARD_PID="$(lsof -t "$TMP/board.sock" 2>/dev/null | head -1 || true)"
if [ -n "$BOARD_PID" ]; then
  tr '\0' ' ' <"/proc/$BOARD_PID/cmdline"; echo
  # Only after confirming the cmdline is the isolated board daemon:
  kill "$BOARD_PID"
fi
rm -rf /tmp/hb-venv
```

`lsof` only nominates the candidate; `/proc/<pid>/cmdline` authorizes the
signal. Then continue with section 8, skipping its `wezterm cli kill-pane`.

### 5b.7 Limitation — state this to the user

PTY + `pyte` gives real emitted attributes at exact dimensions and is more
deterministic than a screenshot, but it renders under a synthetic 256-color
model. It does **not** validate the user's actual terminal palette or font.

It can prove "the marker is bright yellow `#cdcd00`, bold, exactly one on
screen". It cannot settle "does this look good in my terminal". When judgement
about real appearance is needed, hand the interactive board to the user instead
of claiming visual approval.

## 6. Rebuild/restart loop

After prototype edits, or after refreshing the execution-validation mirror from its implementation worktree:

```bash
(cd "$WT" && ~/.cargo/bin/cargo fmt --all)
CARGO_TARGET_DIR="$TARGET" ~/.cargo/bin/cargo test \
  --manifest-path "$WT/Cargo.toml" -p board-tui --features fake-client --test update
CARGO_TARGET_DIR="$TARGET" ~/.cargo/bin/cargo clippy \
  --manifest-path "$WT/Cargo.toml" -p board-tui --features fake-client --all-targets \
  -- -D warnings
CARGO_TARGET_DIR="$TARGET" ~/.cargo/bin/cargo build \
  --manifest-path "$WT/Cargo.toml" --release -p board-cli
```

Restart the disposable plugin pane explicitly so the screenshot cannot use an old binary:

```bash
BOARD_PANE="$(herdr --session "$SESSION" api snapshot |
  jq -r '.result.snapshot.panes[] | select(.label=="Board") | .pane_id' | head -1)"
[ -z "$BOARD_PANE" ] || {
  printf 'HERDR MUTATION: close old prototype pane %s\n' "$BOARD_PANE"
  herdr --session "$SESSION" pane close "$BOARD_PANE"
}
printf 'HERDR MUTATION: reopen prototype overlay\n'
herdr --session "$SESSION" plugin action invoke open-board --plugin herdr-board
```

## 7. Promotion

After explicit approval in prototype mode:

1. Save the disposable worktree diff.
2. Apply behavior tests to the designated implementation checkout/worktree first; run and record red.
3. Apply source changes there; run focused green tests.

Execution-validation mode requires no promotion: its source changes already live in `$EXECUTION_WORKTREE`.
4. Add layout/reducer tests plus narrow/wide/overflow snapshots.
5. Update README, `docs/design.md`, and `CHANGELOG.md`.
6. Run:

```bash
~/.cargo/bin/cargo fmt --all --check
~/.cargo/bin/cargo clippy --all-targets -- -D warnings
~/.cargo/bin/cargo test --workspace --all-features
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
PATH="$HOME/.cargo/bin:$PATH" e2e/run-all.sh
```

Do not install or overwrite the user's managed plugin during validation.

## 8. Cleanup and recovery

Reload variables from the state file if needed, then clean exact resources:

```bash
# shellcheck disable=SC1090
. "$STATE"
# shellcheck disable=SC1091
. "$REPO/e2e/lib.sh"

[ -z "${PANE:-}" ] || env -u WEZTERM_UNIX_SOCKET wezterm cli kill-pane \
  --pane-id "$PANE" 2>/dev/null || true

# A candidate PID from a socket or state file never authorizes a signal alone.
if ! e2e_process_identity_verify "$BOARD_PID" "$BOARD_IDENTITY"; then
  echo "refusing board-daemon signal: /proc identity changed" >&2
  exit 1
fi
kill "$BOARD_PID" 2>/dev/null || true
wait "$BOARD_PID" 2>/dev/null || true

# One exact server verification authorizes this contiguous workspace/session
# teardown; session stop normally removes /proc before delete runs.
if ! e2e_process_identity_verify "$SERVER_PID" "$SERVER_IDENTITY"; then
  echo "refusing Herdr mutations: recorded server identity changed" >&2
  exit 1
fi
[ -z "${WS:-}" ] || {
  printf 'HERDR MUTATION: close disposable workspace %s\n' "$WS"
  "$HERDR_BIN" --session "$SESSION" workspace close "$WS"
}
printf 'HERDR MUTATION: stop/delete disposable session %s\n' "$SESSION"
"$HERDR_BIN" session stop "$SESSION"
"$HERDR_BIN" session delete "$SESSION"

if [ "$VALIDATION_MODE" = prototype ]; then
  git worktree remove --force "$WT"
else
  rm -rf "$WT"
fi
rm -rf "$TARGET" "$TMP"
rm -f "$STATE"

herdr session list --json | jq --arg s "$SESSION" \
  '[.sessions[] | select(.name==$s)]'
git status --short
```

If a run is interrupted, list leftovers first:

```bash
herdr session list --json | jq '.sessions[] | select(.name|test("^hb-(visual|prototype|e2e)-"))'
git worktree list
ps -C board -o pid=,args= 2>/dev/null || true
```

Delete only resources whose generated names/paths match the recorded state.

## 9. Common failures

| Symptom | Cause / correction |
|---|---|
| `nested herdr is disabled` | Unset `HERDR_ENV`, pane/tab/workspace IDs, and `HERDR_SOCKET_PATH` before attaching. |
| WezTerm CLI references a missing socket | Unset `WEZTERM_UNIX_SOCKET`; permission changes may restart WezTerm. |
| `cargo: command not found` | Use `~/.cargo/bin/cargo` or prepend `$HOME/.cargo/bin`. |
| Plugin opens an old UI | Rebuild the separate target, close the exact `Board` pane, then reopen. |
| Board uses real user state | Stop immediately; verify server/workspace inherited isolated `BOARD_DB`/`BOARD_SOCKET`. |
| AF_UNIX path/connect errors | Keep DB/socket under short `/tmp/hb-*.XXXXXX` paths. |
| Screenshot lacks colors | Use `get-text --escapes` or PNG; plain TestBackend snapshots omit style. |
| Screenshot capture denied | Grant macOS Screen Recording to WezTerm/process and retry. |
| Auto column launches a real agent | Use manual columns or the repository fake harness/e2e stack. |
| Cleanup risks user sessions | Stop; compare against the state file and mutate only generated session/workspace IDs. |
| `wezterm: command not found` but `TERM_PROGRAM=WezTerm` | WSL2 with WezTerm on the Windows host. Branch on `command -v wezterm`, not `TERM_PROGRAM`; use section 5b. |
| `failed to connect to Socket("gui-sock-<pid>"); terminating` | `/mnt/c/Program Files/WezTerm/wezterm.exe` cannot reach the Windows-namespace mux socket. Do not use the `.exe`; use section 5b. |
| Session dies mid-cleanup after a `pkill` | `pkill -f "board.sock"` matches the invoking shell's own argv. Use `lsof -t "$TMP/board.sock"`, confirm `/proc/<pid>/cmdline`, then `kill` that PID. |
| `pip install pyte` refuses: externally-managed environment | PEP 668. `python3 -m venv /tmp/hb-venv && /tmp/hb-venv/bin/pip install -q pyte`. |
| PTY capture renders at 80x24 regardless of `LINES`/`COLUMNS` | ratatui sizes from the PTY. Set `TIOCSWINSZ` with `fcntl.ioctl` after `pty.fork()`. |
| A marker "appears twice" only in the `.ansi` file | The escape stream is cumulative across redraws. Assert on the `pyte`-rendered final screen (`.txt`/`.attrs`), never the raw stream. |
