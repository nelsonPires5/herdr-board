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
            is_events = request.get("method") == "events.subscribe"
            if is_events:
                self.subscriptions += 1
                if self.reject_events:
                    return
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
            elif command != "status":
                raise ValueError("unknown command")
            response = {"ok": True, "offline": self.offline,
                        "reject_events": self.reject_events,
                        "subscriptions": self.subscriptions,
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
