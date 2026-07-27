"""Shared NDJSON-over-unix-socket RPC plumbing.

Two one-shot CLI helpers speak this transport against different servers:

- `scripts/board-rpc.py` → boardd (`$BOARD_SOCKET`), used by `e2e/lib.sh`'s
  `brpc`/`col_create` for protocol methods the `board` CLI has no verb for;
- `e2e/hrpc.py` → herdr (`$HERDR_SOCKET_PATH`), used by the scenarios for raw
  structural asserts.

Both envelopes are the same shape and both servers serve exactly one request per
connection and then close it, so the socket resolution, connect, send, and
read-to-first-newline logic is identical. Only response *interpretation* differs,
which is why that part stays in each front end.

    request:  {"id":"<str>","method":"<name>","params":{...}}

Stdlib only: these run inside the e2e suite's narrowed environment.
"""
from __future__ import annotations

import json
import os
import socket
from typing import Any, Iterable

CHUNK_SIZE = 4096


def socket_path(env_vars: Iterable[str], default: str) -> str:
    """First non-empty value among `env_vars`, else the expanded `default`."""
    for var in env_vars:
        value = os.environ.get(var)
        if value:
            return value
    return os.path.expanduser(default)


def parse_params(raw: str) -> Any:
    """Decode the CLI's JSON params argument. Raises json.JSONDecodeError."""
    return json.loads(raw)


def request_line(path: str, request_id: str, method: str, params: Any) -> str:
    """Send one request and return the first response line (`""` if none).

    Raises OSError when the socket cannot be reached, which each front end
    reports with its own program-prefixed message.
    """
    request = {"id": request_id, "method": method, "params": params}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.connect(path)
        connection.sendall((json.dumps(request) + "\n").encode("utf-8"))
        buffer = b""
        while b"\n" not in buffer:
            chunk = connection.recv(CHUNK_SIZE)
            if not chunk:
                break
            buffer += chunk
    return buffer.split(b"\n", 1)[0].decode("utf-8", "replace")
