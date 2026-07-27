#!/usr/bin/env python3
"""hrpc.py — one-shot herdr socket RPC helper for the e2e scenarios.

The `herdr` CLI covers most operations, but the e2e scenarios also need to make
raw structural assertions (tab.list / pane.list / pane.layout) and to target a
specific session's socket without the CLI's output wrapping. This speaks herdr's
NDJSON-over-unix-socket protocol directly (mirrors board-herdr's envelope):

    request:  {"id":"<str>","method":"<name>","params":{...}}
    success:  {"id":"<str>","result":{...}}
    error:    {"id":"<str>","error":{"code":"<str>","message":"<str>"}}

herdr serves ONE request per connection and closes the socket after the reply,
so every call opens a fresh connection (same as `board-herdr`'s HerdrClient).

Usage:
    hrpc.py <method> [JSON_PARAMS]

    JSON_PARAMS defaults to "{}". The socket path is taken from
    $HERDR_SOCKET_PATH (herdr's canonical variable), else $HERDR_SOCKET, else the
    default session's ~/.config/herdr/herdr.sock — matching board-herdr's
    default_socket_path().

Prints the raw `result` payload as one JSON line to stdout. Exits non-zero (and
prints to stderr) on a protocol error response, so callers can `set -e`.

Examples:
    HERDR_SOCKET_PATH=/path/to/session.sock hrpc.py tab.list '{"workspace_id":"w3"}'
    hrpc.py pane.list '{"workspace_id":"w3"}'
    hrpc.py pane.layout '{"pane_id":"w3:t1:p1"}'
"""
import json
import sys
from pathlib import Path

# The socket resolution / connect / send / read-to-newline transport is shared
# with scripts/board-rpc.py; only the response interpretation below differs.
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))

import ndjson_rpc  # noqa: E402  (path shim above must run first)

SOCKET_ENV_VARS = ("HERDR_SOCKET_PATH", "HERDR_SOCKET")
DEFAULT_SOCKET = "~/.config/herdr/herdr.sock"


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
        print(f"hrpc.py: invalid JSON params: {e}", file=sys.stderr)
        return 2

    path = socket_path()
    try:
        line = ndjson_rpc.request_line(path, "hrpc", method, params_obj)
    except OSError as e:
        print(f"hrpc.py: cannot reach herdr at {path}: {e}", file=sys.stderr)
        return 1

    if not line:
        print("hrpc.py: empty response", file=sys.stderr)
        return 1
    try:
        resp = json.loads(line)
    except json.JSONDecodeError:
        print(line)
        return 0
    if isinstance(resp, dict) and resp.get("error"):
        print(f"hrpc.py: protocol error: {resp['error']}", file=sys.stderr)
        return 1
    result = resp.get("result", resp) if isinstance(resp, dict) else resp
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
