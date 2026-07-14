#!/usr/bin/env bash
# Idempotent launcher for the board overlay — used by both the `open-board` action
# and a herdr keybinding (`[[keys.command]]` with `type = "shell"`). Mirrors
# herdr-file-viewer's launcher: "open-or-focus, toggle off on repeat".
#
#   - no board pane in the current workspace     -> open the overlay (focused)
#   - a board pane exists but isn't focused        -> focus it
#   - the focused pane IS the board pane           -> close it (herdr has no
#                                                     hide-without-close; reopening
#                                                     is cheap — the TUI refetches)
#
# herdr actions/keybindings run a command (no declarative "open this pane" field),
# so this shells out to the herdr CLI via $HERDR_BIN_PATH (herdr injects it; fall
# back to `herdr` on PATH). The pane is identified by title (the [[panes]] title
# "Board"). Any failure degrades to OPEN, preserving always-open behavior.
set -uo pipefail

herdr_bin="${HERDR_BIN_PATH:-herdr}"

open_pane() {
  exec "$herdr_bin" plugin pane open \
    --plugin herdr-board \
    --entrypoint board \
    --placement overlay \
    --focus
}

# Decide OPEN / "FOCUS <pane>" / "CLOSE <pane>" from the live pane list. Needs
# python3 for robust JSON parsing; without it we always OPEN.
decision="OPEN"
if command -v python3 >/dev/null 2>&1; then
  panes="$("$herdr_bin" pane list 2>/dev/null || true)"   # outputs JSON (no --json flag)
  if [ -n "$panes" ]; then
    decision="$(printf '%s' "$panes" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("OPEN"); sys.exit(0)
res = data.get("result", data)
panes = res.get("panes", []) if isinstance(res, dict) else []
board = None
for p in panes:
    # herdr reports the [[panes]] title as the pane label.
    name = (p.get("label") or p.get("title") or "")
    if name == "Board":
        board = p
        break
if not board:
    print("OPEN"); sys.exit(0)
pid = board.get("pane_id") or ""
if not pid:
    print("OPEN"); sys.exit(0)
if board.get("focused"):
    print("CLOSE " + str(pid))
else:
    print("FOCUS " + str(pid))
' 2>/dev/null || echo OPEN)"
  fi
fi

case "$decision" in
  "FOCUS "*)
    pid="${decision#FOCUS }"
    exec "$herdr_bin" plugin pane focus "$pid"
    ;;
  "CLOSE "*)
    pid="${decision#CLOSE }"
    exec "$herdr_bin" pane close "$pid"
    ;;
  *)
    open_pane
    ;;
esac
