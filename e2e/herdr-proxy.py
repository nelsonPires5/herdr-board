#!/usr/bin/env python3
"""Controllable transparent Unix-socket proxy for provider-free E2E recovery tests."""

import argparse
import asyncio
import json
import os
import signal


class Proxy:
    def __init__(self, target: str):
        self.target = target
        self.offline = False
        self.reject_events = False
        # Fault injection is deliberately request-scoped: agent.start is a
        # one-shot Herdr RPC, so returning a typed error here exercises the
        # daemon's retry/cleanup boundary without touching the real server.
        self.agent_pane_busy_mode = "none"
        self.agent_start_panes: list[str] = []
        self.busy_injections = 0
        self.pane_splits: list[str] = []
        self.pane_closes: list[str] = []
        # integration.list fault injection: `None` forwards to the real server
        # (the default). A list serves exactly those targets as `available`;
        # `error=True` makes the call fail. The board must degrade gracefully
        # in both directions, so the scenario can pin either behavior.
        self.integration_list_targets: list[str] | None = None
        self.integration_list_error = False
        self.integration_list_calls = 0
        self.connections: set[tuple[asyncio.StreamWriter, asyncio.StreamWriter, bool]] = set()
        self.subscriptions = 0

    async def close_matching(self, *, all_connections: bool = False, events: bool = False):
        selected = [c for c in self.connections if all_connections or (events and c[2])]
        for client, upstream, _ in selected:
            client.close()
            upstream.close()
        for client, upstream, _ in selected:
            await asyncio.gather(client.wait_closed(), upstream.wait_closed(), return_exceptions=True)

    async def client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        upstream_writer = None
        record = None
        try:
            first = await asyncio.wait_for(reader.readline(), timeout=5)
            if not first or self.offline:
                return
            try:
                request = json.loads(first)
            except (UnicodeDecodeError, json.JSONDecodeError):
                return
            method = request.get("method")
            params = request.get("params") or {}
            is_events = method == "events.subscribe"
            if is_events:
                self.subscriptions += 1
                if self.reject_events:
                    return
            if method == "integration.list":
                self.integration_list_calls += 1
                if self.integration_list_error:
                    writer.write(json.dumps({
                        "id": request.get("id"),
                        "error": {
                            "code": "internal",
                            "message": "integration.list unavailable (deterministic e2e fault)",
                        },
                    }, separators=(",", ":")).encode() + b"\n")
                    await writer.drain()
                    return
                if self.integration_list_targets is not None:
                    writer.write(json.dumps({
                        "id": request.get("id"),
                        "result": {
                            "type": "integration_list",
                            "integrations": [
                                {"target": target, "label": target, "command": target,
                                 "available": True, "state": "current"}
                                for target in self.integration_list_targets
                            ],
                        },
                    }, separators=(",", ":")).encode() + b"\n")
                    await writer.drain()
                    return
            if method == "agent.start":
                pane_id = str(params.get("pane_id", ""))
                self.agent_start_panes.append(pane_id)
                if self.agent_pane_busy_mode != "none":
                    self.busy_injections += 1
                    if self.agent_pane_busy_mode == "once":
                        self.agent_pane_busy_mode = "none"
                    writer.write(json.dumps({
                        "id": request.get("id"),
                        "error": {
                            "code": "agent_pane_busy",
                            "message": "agent pane is still busy (deterministic e2e fault)",
                        },
                    }, separators=(",", ":")).encode() + b"\n")
                    await writer.drain()
                    return
            elif method == "pane.split":
                self.pane_splits.append(str(params.get("target_pane_id", "")))
            elif method == "pane.close":
                self.pane_closes.append(str(params.get("pane_id", "")))
            upstream_reader, upstream_writer = await asyncio.open_unix_connection(self.target)
            record = (writer, upstream_writer, is_events)
            self.connections.add(record)
            upstream_writer.write(first)
            await upstream_writer.drain()

            async def copy(src, dst):
                while data := await src.read(65536):
                    dst.write(data)
                    await dst.drain()

            tasks = [asyncio.create_task(copy(reader, upstream_writer)),
                     asyncio.create_task(copy(upstream_reader, writer))]
            done, pending = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            for task in pending:
                task.cancel()
            await asyncio.gather(*done, *pending, return_exceptions=True)
        except (asyncio.CancelledError, ConnectionError, OSError, asyncio.TimeoutError):
            pass
        finally:
            if record is not None:
                self.connections.discard(record)
            writer.close()
            if upstream_writer is not None:
                upstream_writer.close()
            await asyncio.gather(writer.wait_closed(), return_exceptions=True)
            if upstream_writer is not None:
                await asyncio.gather(upstream_writer.wait_closed(), return_exceptions=True)

    async def control(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        try:
            request = json.loads((await reader.readline()).decode())
            command = request.get("command")
            if command == "offline":
                self.offline = True
                await self.close_matching(all_connections=True)
            elif command == "online":
                self.offline = False
            elif command == "reject_events":
                self.reject_events = True
                await self.close_matching(events=True)
            elif command == "allow_events":
                self.reject_events = False
            elif command in ("agent_pane_busy_transient", "busy_once"):
                self.agent_pane_busy_mode = "once"
            elif command in ("agent_pane_busy_persistent", "busy_always"):
                self.agent_pane_busy_mode = "persistent"
            elif command in ("agent_pane_busy_clear", "busy_clear"):
                self.agent_pane_busy_mode = "none"
            elif command == "integration_list_available":
                raw = request.get("targets", "")
                self.integration_list_targets = [t for t in raw.split(",") if t]
                self.integration_list_error = False
            elif command == "integration_list_error":
                self.integration_list_targets = None
                self.integration_list_error = True
            elif command == "integration_list_auto":
                self.integration_list_targets = None
                self.integration_list_error = False
            elif command != "status":
                raise ValueError("unknown command")
            response = {"ok": True, "offline": self.offline,
                        "reject_events": self.reject_events,
                        "agent_pane_busy_mode": self.agent_pane_busy_mode,
                        "busy_injections": self.busy_injections,
                        "agent_start_panes": self.agent_start_panes,
                        "pane_splits": self.pane_splits,
                        "pane_closes": self.pane_closes,
                        "subscriptions": self.subscriptions,
                        "integration_list_mode": ("error" if self.integration_list_error
                                                   else "available" if self.integration_list_targets is not None
                                                   else "auto"),
                        "integration_list_targets": self.integration_list_targets,
                        "integration_list_calls": self.integration_list_calls,
                        "connections": len(self.connections)}
        except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as error:
            response = {"ok": False, "error": str(error)}
        writer.write(json.dumps(response, separators=(",", ":")).encode() + b"\n")
        await writer.drain()
        writer.close()
        await writer.wait_closed()


async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", required=True)
    parser.add_argument("--control", required=True)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    for path in (args.listen, args.control):
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
    proxy = Proxy(args.target)
    data_server = await asyncio.start_unix_server(proxy.client, args.listen)
    control_server = await asyncio.start_unix_server(proxy.control, args.control)
    os.chmod(args.listen, 0o600)
    os.chmod(args.control, 0o600)
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stop.set)
    async with data_server, control_server:
        await stop.wait()
    await proxy.close_matching(all_connections=True)
    for path in (args.listen, args.control):
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    asyncio.run(main())
