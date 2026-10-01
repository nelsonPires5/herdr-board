"""The shared NDJSON RPC transport speaks a Windows named pipe natively."""

import json
import sys
import tempfile
import threading
import unittest
from pathlib import Path

from _support import REPO_ROOT

sys.path.insert(0, str(REPO_ROOT / "scripts"))
import ndjson_rpc  # noqa: E402


@unittest.skipUnless(sys.platform == "win32", "Windows named pipes")
class WindowsPipeTransportTest(unittest.TestCase):
    def test_request_line_round_trips_over_the_named_pipe_for_a_socket_path(self):
        import _winapi

        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "herdr.sock")
            pipe = _winapi.CreateNamedPipe(
                "\\\\.\\pipe\\" + path,
                _winapi.PIPE_ACCESS_DUPLEX,
                _winapi.PIPE_WAIT,  # byte type and read mode are 0
                1,
                65536,
                65536,
                0,
                _winapi.NULL,
            )
            received = []

            def serve():
                _winapi.ConnectNamedPipe(pipe, False)
                data, _ = _winapi.ReadFile(pipe, 65536)
                received.append(json.loads(data))
                _winapi.WriteFile(pipe, b'{"id":"7","result":{"type":"pong"}}\n')
                _winapi.CloseHandle(pipe)

            server = threading.Thread(target=serve, daemon=True)
            server.start()
            line = ndjson_rpc.request_line(path, "7", "ping", {})
            server.join(timeout=5)

        self.assertEqual(json.loads(line), {"id": "7", "result": {"type": "pong"}})
        self.assertEqual(received[0]["method"], "ping")


if __name__ == "__main__":
    unittest.main()
