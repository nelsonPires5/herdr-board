#!/usr/bin/env python3
"""Drive the real `board tui` under a PTY at an exact size and capture output.

Use on WSL2 or any machine where the WezTerm CLI is unreachable (see
`playbook.md` section 5b). Replaces `wezterm cli get-text --escapes`.

Writes three files:
  <out>.ansi  raw cumulative escape stream (all frames, for debugging only)
  <out>.txt   final screen state rendered by pyte (the grid to assert on)
  <out>.attrs per-cell attributes of the final screen (fg/bg/bold/reverse)

Usage:
  pty-capture.py <tmp> <cols> <rows> <out-prefix> [keys...]

`<tmp>` is the isolated stack directory holding board.db/board.sock/config.toml.
The board binary comes from $BOARD_BIN, or falls back to
"$TARGET/release/board", or "./target/release/board".

Keys: literal text, or names TAB ENTER ESC UP DOWN LEFT RIGHT,
or WHEELUP:n / WHEELDOWN:n (SGR mouse wheel; point it with $WHEEL_COL/$WHEEL_ROW,
default col 20 row 12 — put it inside the section you want to scroll).
"""
import fcntl
import os
import pty
import select
import struct
import sys
import termios
import time

NAMED = {
    "TAB": "\t", "ENTER": "\r", "ESC": "\x1b",
    "DOWN": "\x1b[B", "UP": "\x1b[A",
    "LEFT": "\x1b[D", "RIGHT": "\x1b[C",
}
STARTUP_SETTLE = float(os.environ.get("STARTUP_SETTLE", "2.5"))
KEY_SETTLE = float(os.environ.get("KEY_SETTLE", "0.8"))
WHEEL_COL = os.environ.get("WHEEL_COL", "20")
WHEEL_ROW = os.environ.get("WHEEL_ROW", "12")


def board_bin():
    cand = os.environ.get("BOARD_BIN")
    if not cand:
        target = os.environ.get("TARGET")
        cand = f"{target}/release/board" if target else "./target/release/board"
    cand = os.path.abspath(cand)
    if not os.access(cand, os.X_OK):
        sys.exit(f"board binary not executable: {cand} (set BOARD_BIN)")
    return cand


def key_bytes(tok):
    if tok.startswith(("WHEELUP:", "WHEELDOWN:")):
        name, n = tok.split(":")
        btn = 64 if name == "WHEELUP" else 65
        return (f"\x1b[<{btn};{WHEEL_COL};{WHEEL_ROW}M" * int(n)).encode()
    return NAMED.get(tok, tok).encode()


def child_env(tmp, cols, rows):
    env = dict(os.environ)
    env.update({
        "BOARD_DB": f"{tmp}/board.db",
        "BOARD_SOCKET": f"{tmp}/board.sock",
        "HERDR_BOARD_CONFIG": f"{tmp}/config.toml",
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
        # Informational only; ratatui sizes from the PTY via TIOCSWINSZ below.
        "LINES": str(rows),
        "COLUMNS": str(cols),
    })
    # A nested agent session must not leak into the disposable stack.
    for v in ("HERDR_ENV", "HERDR_PANE_ID", "HERDR_TAB_ID",
              "HERDR_WORKSPACE_ID", "HERDR_SOCKET_PATH"):
        env.pop(v, None)
    return env


def render(raw, cols, rows, out):
    """Feed the cumulative stream to pyte and dump the FINAL screen only.

    The raw stream contains every redraw, so grepping it hits stale frames.
    Only the emulated end state answers "what is on screen now".
    """
    try:
        import pyte
    except ImportError:
        import re
        plain = re.sub(r"\x1b\[[0-9;?<>]*[a-zA-Z]", "",
                       raw.decode("utf-8", "replace"))
        with open(out + ".txt", "w") as f:
            f.write(plain)
        print("pyte missing: wrote stripped stream, NOT a final-screen render.",
              file=sys.stderr)
        print("  python3 -m venv /tmp/hb-venv && /tmp/hb-venv/bin/pip -q install pyte",
              file=sys.stderr)
        return
    screen = pyte.Screen(cols, rows)
    pyte.Stream(screen).feed(raw.decode("utf-8", "replace"))
    with open(out + ".txt", "w") as f:
        f.write("\n".join(screen.display) + "\n")
    with open(out + ".attrs", "w") as f:
        for y in range(rows):
            for x in range(cols):
                c = screen.buffer[y][x]
                if c.data.strip() == "":
                    continue
                f.write(f"{y}\t{x}\t{c.data}\tfg={c.fg}\tbg={c.bg}"
                        f"\tbold={c.bold}\treverse={c.reverse}\n")


def main():
    if len(sys.argv) < 5:
        sys.exit(__doc__)
    tmp, cols, rows, out = (sys.argv[1], int(sys.argv[2]),
                            int(sys.argv[3]), sys.argv[4])
    keys = sys.argv[5:]
    binary = board_bin()
    env = child_env(tmp, cols, rows)

    pid, fd = pty.fork()
    if pid == 0:
        os.execve(binary, [binary, "tui"], env)
    # ratatui reads the window size from the PTY, not from LINES/COLUMNS.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    chunks = []

    def drain(budget):
        end = time.time() + budget
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.1)
            if not r:
                continue
            try:
                d = os.read(fd, 65536)
            except OSError:
                break
            if not d:
                break
            chunks.append(d)

    drain(STARTUP_SETTLE)
    for tok in keys:
        os.write(fd, key_bytes(tok))
        drain(KEY_SETTLE)
    raw = b"".join(chunks)
    with open(out + ".ansi", "wb") as f:
        f.write(raw)

    os.write(fd, b"q")
    drain(0.5)
    try:
        os.kill(pid, 15)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)

    render(raw, cols, rows, out)
    print(f"{out}: {len(raw)} bytes ansi, {cols}x{rows}")


main()
