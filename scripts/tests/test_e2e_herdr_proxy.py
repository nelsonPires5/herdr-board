"""The scenario Herdr proxy forwards and answers control on every platform.

Unix sockets on Unix, `\\\\.\\pipe\\` + the socket path on Windows (the same
convention Herdr and boardd use).
"""

import asyncio
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

from _support import E2E_DIR

spec = importlib.util.spec_from_file_location("herdr_proxy", E2E_DIR / "herdr-proxy.py")
herdr_proxy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(herdr_proxy)


async def request(path: str, payload: dict) -> dict:
    reader, writer = await herdr_proxy.open_connection(path)
    writer.write(json.dumps(payload).encode() + b"\n")
    await writer.drain()
    line = await asyncio.wait_for(reader.readline(), timeout=5)
    writer.close()
    return json.loads(line)


async def round_trip(directory: str) -> tuple[dict, dict]:
    target, listen, control = (str(Path(directory) / n) for n in ("t.sock", "l.sock", "c.sock"))

    async def upstream(reader, writer):
        received = json.loads(await reader.readline())
        writer.write(json.dumps({"id": received["id"], "result": {"echo": received["method"]}}).encode() + b"\n")
        await writer.drain()
        writer.close()

    proxy = herdr_proxy.Proxy(target)
    servers = [
        await herdr_proxy.start_server(upstream, target),
        await herdr_proxy.start_server(proxy.client, listen),
        await herdr_proxy.start_server(proxy.control, control),
    ]
    try:
        forwarded = await request(listen, {"id": "1", "method": "pane.close", "params": {"pane_id": "p9"}})
        status = await request(control, {"command": "status"})
    finally:
        for server in servers:
            server.close()
    return forwarded, status


class HerdrProxyTransportTest(unittest.TestCase):
    def test_forwards_a_request_and_reports_it_over_control(self):
        # AF_UNIX paths cap near 108 bytes; keep the Unix directory short.
        with tempfile.TemporaryDirectory(dir=None if sys.platform == "win32" else "/tmp") as directory:
            forwarded, status = asyncio.run(round_trip(directory))
        self.assertEqual(forwarded, {"id": "1", "result": {"echo": "pane.close"}})
        self.assertTrue(status["ok"])
        self.assertEqual(status["pane_closes"], ["p9"])


if __name__ == "__main__":
    unittest.main()
