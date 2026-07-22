#!/usr/bin/env python3
"""Report fake managed-agent session identity then idle lifecycle to Herdr.

This fixture mirrors the installed Pi/Claude integrations without loading either
client or contacting a provider. Every request/reply is appended to RECORD so a
failed managed launch cannot be mistaken for readiness.
"""

import json
import os
import socket
import sys
import time
from typing import Any


def update_record(path: str, **fields: Any) -> None:
    with open(path, encoding="utf-8") as source:
        record = json.load(source)
    record.update(fields)
    temporary = path + ".tmp"
    with open(temporary, "w", encoding="utf-8") as target:
        json.dump(record, target, ensure_ascii=False, indent=2)
        target.write("\n")
    os.replace(temporary, path)


def request(socket_path: str, envelope: dict[str, Any]) -> dict[str, Any]:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(2.0)
    try:
        client.connect(socket_path)
        client.sendall((json.dumps(envelope, separators=(",", ":")) + "\n").encode())
        chunks = bytearray()
        while b"\n" not in chunks:
            chunk = client.recv(65536)
            if not chunk:
                break
            chunks.extend(chunk)
    finally:
        client.close()
    line = bytes(chunks).splitlines()[0] if chunks else b""
    if not line:
        raise RuntimeError("Herdr closed the report socket without a reply")
    return json.loads(line)


def main() -> int:
    if len(sys.argv) != 6:
        print(
            "managed-agent-report: expected RECORD AGENT SESSION_ID SESSION_PATH START_SOURCE",
            file=sys.stderr,
        )
        return 2
    record_path, agent, session_id, session_path, start_source = sys.argv[1:]
    pane_id = os.environ.get("HERDR_PANE_ID", "")
    socket_path = os.environ.get("HERDR_SOCKET_PATH", "")
    source = f"herdr:{agent}"
    if not pane_id or not socket_path:
        update_record(
            record_path,
            readiness_report="missing-herdr-env",
            herdr_pane_id=pane_id,
            report_socket=socket_path,
            reports=[],
        )
        print("managed-agent-report: HERDR_PANE_ID/socket required", file=sys.stderr)
        return 2

    # One process owns both sequence numbers, making ordering explicit. The
    # nanosecond wall-clock base is large enough not to lose to integration
    # reports from an earlier runtime; incrementing it is strictly monotonic.
    sequence = time.time_ns()
    common: dict[str, Any] = {
        "pane_id": pane_id,
        "source": source,
        "agent": agent,
    }
    session_ref: dict[str, Any] = {"agent_session_path": session_path}
    if session_id:
        session_ref["agent_session_id"] = session_id

    requests = [
        (
            "session_identity",
            "pane.report_agent_session",
            {**common, **session_ref, "seq": sequence, "session_start_source": start_source},
        ),
        (
            "idle_lifecycle",
            "pane.report_agent",
            {**common, **session_ref, "seq": sequence + 1, "state": "idle"},
        ),
    ]
    reports: list[dict[str, Any]] = []
    update_record(
        record_path,
        herdr_pane_id=pane_id,
        report_socket=socket_path,
        agent_session_id=session_id or None,
        agent_session_path=session_path,
        session_start_source=start_source,
        reports=reports,
        readiness_report="reporting",
    )

    for index, (phase, method, params) in enumerate(requests):
        envelope = {
            "id": f"e2e:{source}:{os.getpid()}:{index}",
            "method": method,
            "params": params,
        }
        result: dict[str, Any] = {"phase": phase, "request": envelope}
        try:
            reply = request(socket_path, envelope)
            result["reply"] = reply
            result["ok"] = "result" in reply and "error" not in reply
        except Exception as error:  # evidence must survive transport failures
            result["ok"] = False
            result["error"] = f"{type(error).__name__}: {error}"
        reports.append(result)
        update_record(record_path, reports=reports)
        if not result["ok"]:
            update_record(record_path, readiness_report=f"failed:{phase}")
            print(f"managed-agent-report: {phase} failed: {result}", file=sys.stderr)
            return 2

    update_record(record_path, readiness_report="ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
