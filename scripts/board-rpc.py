#!/usr/bin/env python3
"""board-rpc.py — minimal raw boardd protocol client.

The `board` CLI covers cards/comments/moves/runs, but column creation and other
protocol methods have no CLI verb (columns are normally made in the TUI). This
helper speaks the NDJSON-over-unix-socket protocol directly so scripts (notably
scripts/e2e.sh) can call any method, e.g. `column.create`.

Usage:
    board-rpc.py <method> [JSON_PARAMS]

    JSON_PARAMS defaults to "{}". The socket path is $BOARD_SOCKET, else
    ~/.local/share/herdr-board/boardd.sock.

Prints the raw response line to stdout. Exits non-zero (and prints to stderr) on
a protocol error response, so callers can `set -e`.

Examples:
    board-rpc.py board.get
    board-rpc.py column.create '{"name":"Execute","trigger":"auto"}'
    board-rpc.py column.update '{"id":2,"on_success_column_id":3}'
"""
import json
import os
import socket
import sys


def socket_path() -> str:
    p = os.environ.get("BOARD_SOCKET")
    if p:
        return p
    return os.path.expanduser("~/.local/share/herdr-board/boardd.sock")


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    method = sys.argv[1]
    params = sys.argv[2] if len(sys.argv) > 2 else "{}"
    try:
        params_obj = json.loads(params)
    except json.JSONDecodeError as e:
        print(f"board-rpc.py: invalid JSON params: {e}", file=sys.stderr)
        return 2

    req = {"id": "rpc", "method": method, "params": params_obj}
    path = socket_path()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.connect(path)
            s.sendall((json.dumps(req) + "\n").encode("utf-8"))
            # Read one NDJSON line (the response for our id).
            buf = b""
            while b"\n" not in buf:
                chunk = s.recv(4096)
                if not chunk:
                    break
                buf += chunk
    except OSError as e:
        print(f"board-rpc.py: cannot reach boardd at {path}: {e}", file=sys.stderr)
        return 1

    line = buf.split(b"\n", 1)[0].decode("utf-8", "replace")
    if not line:
        print("board-rpc.py: empty response", file=sys.stderr)
        return 1
    print(line)
    try:
        resp = json.loads(line)
    except json.JSONDecodeError:
        return 0
    if isinstance(resp, dict) and "error" in resp:
        print(f"board-rpc.py: protocol error: {resp['error']}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
