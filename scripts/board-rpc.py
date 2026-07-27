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
import sys

import ndjson_rpc

SOCKET_ENV_VARS = ("BOARD_SOCKET",)
DEFAULT_SOCKET = "~/.local/share/herdr-board/boardd.sock"


def socket_path() -> str:
    return ndjson_rpc.socket_path(SOCKET_ENV_VARS, DEFAULT_SOCKET)


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    method = sys.argv[1]
    params = sys.argv[2] if len(sys.argv) > 2 else "{}"
    try:
        params_obj = ndjson_rpc.parse_params(params)
    except json.JSONDecodeError as e:
        print(f"board-rpc.py: invalid JSON params: {e}", file=sys.stderr)
        return 2

    path = socket_path()
    try:
        line = ndjson_rpc.request_line(path, "rpc", method, params_obj)
    except OSError as e:
        print(f"board-rpc.py: cannot reach boardd at {path}: {e}", file=sys.stderr)
        return 1

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
