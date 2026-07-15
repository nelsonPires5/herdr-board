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
import os
import socket
import sys


def socket_path() -> str:
    for var in ("HERDR_SOCKET_PATH", "HERDR_SOCKET"):
        p = os.environ.get(var)
        if p:
            return p
    return os.path.expanduser("~/.config/herdr/herdr.sock")


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    method = sys.argv[1]
    params = sys.argv[2] if len(sys.argv) > 2 else "{}"
    try:
        params_obj = json.loads(params)
    except json.JSONDecodeError as e:
        print(f"hrpc.py: invalid JSON params: {e}", file=sys.stderr)
        return 2

    req = {"id": "hrpc", "method": method, "params": params_obj}
    path = socket_path()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.connect(path)
            s.sendall((json.dumps(req) + "\n").encode("utf-8"))
            buf = b""
            while b"\n" not in buf:
                chunk = s.recv(4096)
                if not chunk:
                    break
                buf += chunk
    except OSError as e:
        print(f"hrpc.py: cannot reach herdr at {path}: {e}", file=sys.stderr)
        return 1

    line = buf.split(b"\n", 1)[0].decode("utf-8", "replace")
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
