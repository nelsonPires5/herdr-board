# Reproducible Herdr Board Prototype Playbook

## Contents

1. Preflight
2. Isolated worktree and build
3. Disposable Herdr/board stack
4. Temporary WezTerm client
5. Fixtures and captures
6. Rebuild/restart loop
7. Promotion
8. Cleanup and recovery
9. Common failures

Commands below were exercised with Herdr 0.7.4 / protocol 16. Re-verify before use.

## 1. Preflight

```bash
REPO="$(git rev-parse --show-toplevel)"
cd "$REPO"
git status --short
herdr --version
herdr status
herdr api schema --json | jq '{protocol,schema_version}'
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

## 2. Isolated worktree and build

```bash
RUN_ID="$$"
SESSION="hb-visual-$RUN_ID"
TMP="$(mktemp -d /tmp/hb-visual.XXXXXX)"
WT="/tmp/herdr-board-visual-$RUN_ID"
TARGET="/tmp/herdr-board-target-$RUN_ID"
STATE="/tmp/hb-visual-$RUN_ID.env"

git worktree add --detach "$WT" HEAD
CARGO_TARGET_DIR="$TARGET" ~/.cargo/bin/cargo build \
  --manifest-path "$WT/Cargo.toml" --release -p board-cli
ln -s "$TARGET" "$WT/target"

cat >"$STATE" <<EOF
SESSION=$SESSION
TMP=$TMP
WT=$WT
TARGET=$TARGET
EOF
```

The symlink satisfies `herdr-plugin.toml`'s relative `./target/release/board` without overwriting the main checkout's binary.

For a baseline/proposal comparison, capture the baseline before editing this worktree. Then edit the same worktree and capture the proposal with identical fixture data and dimensions.

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

printf 'HERDR MUTATION: invoke real plugin action\n'
herdr --session "$SESSION" plugin action invoke open-board --plugin herdr-board
```

Verify placement and process paths:

```bash
herdr --session "$SESSION" api snapshot | jq \
  '.result.snapshot | {focused_pane_id,workspaces,tabs,panes,layouts}'
herdr --session "$SESSION" plugin log list --plugin herdr-board --limit 10
```

## 4. Temporary WezTerm client

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

## 6. Rebuild/restart loop

After prototype edits:

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

After explicit approval:

1. Save the worktree diff.
2. Apply behavior tests to main first; run and record red.
3. Apply source changes; run focused green tests.
4. Add layout/reducer tests plus narrow/wide/overflow snapshots.
5. Update README, `docs/design.md`, and `CHANGELOG.md`.
6. Run:

```bash
~/.cargo/bin/cargo fmt --all --check
~/.cargo/bin/cargo clippy --all-targets -- -D warnings
~/.cargo/bin/cargo test --workspace --all-features
PATH="$HOME/.cargo/bin:$PATH" e2e/run-all.sh
```

Do not install or overwrite the user's managed plugin during validation.

## 8. Cleanup and recovery

Reload variables from the state file if needed, then clean exact resources:

```bash
# shellcheck disable=SC1090
. "$STATE"

[ -z "${PANE:-}" ] || env -u WEZTERM_UNIX_SOCKET wezterm cli kill-pane \
  --pane-id "$PANE" 2>/dev/null || true

[ -z "${WS:-}" ] || {
  printf 'HERDR MUTATION: close disposable workspace %s\n' "$WS"
  herdr --session "$SESSION" workspace close "$WS" 2>/dev/null || true
}

BOARD_PID="$(lsof -t "$TMP/board.sock" 2>/dev/null | head -1 || true)"
[ -z "$BOARD_PID" ] || kill "$BOARD_PID" 2>/dev/null || true

printf 'HERDR MUTATION: stop/delete disposable session %s\n' "$SESSION"
herdr session stop "$SESSION" 2>/dev/null || true
herdr session delete "$SESSION" 2>/dev/null || true

git worktree remove --force "$WT" 2>/dev/null || true
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
