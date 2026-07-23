from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import socket
import subprocess
import tempfile
import textwrap
import threading
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
E2E_LIB = REPO_ROOT / "e2e" / "lib.sh"
REAL_CLAUDE_SMOKE = REPO_ROOT / "e2e" / "real-claude-haiku-smoke.sh"


class _RpcSocket:
    """Tiny deterministic responder for the one hrpc probe used by lib.sh."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.stop = threading.Event()
        self.ready = threading.Event()
        self.thread = threading.Thread(target=self._serve, daemon=True)

    def start(self) -> None:
        self.thread.start()
        if not self.ready.wait(timeout=2):
            raise RuntimeError("fake Herdr socket did not start")

    def _serve(self) -> None:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(self.path))
            listener.listen()
            listener.settimeout(0.05)
            self.ready.set()
            while not self.stop.is_set():
                try:
                    conn, _ = listener.accept()
                except TimeoutError:
                    continue
                with conn:
                    conn.recv(4096)
                    try:
                        conn.sendall(b'{"id":"hrpc","result":{}}\n')
                    except BrokenPipeError:
                        pass

    def close(self) -> None:
        self.stop.set()
        # Wake accept() so the thread can finish promptly.
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as wake:
                wake.connect(str(self.path))
        except OSError:
            pass
        self.thread.join(timeout=2)
        try:
            self.path.unlink()
        except FileNotFoundError:
            pass


class _NonHerdrSocket(_RpcSocket):
    """A listening Unix socket that cannot answer a Herdr RPC probe."""

    def _serve(self) -> None:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(self.path))
            listener.listen()
            listener.settimeout(0.05)
            self.ready.set()
            while not self.stop.is_set():
                try:
                    conn, _ = listener.accept()
                except TimeoutError:
                    continue
                with conn:
                    conn.recv(4096)
                    try:
                        conn.sendall(b'not-herdr\\n')
                    except BrokenPipeError:
                        pass


class E2ESafetyTests(unittest.TestCase):
    def _write_executable(self, directory: Path, name: str, body: str) -> Path:
        path = directory / name
        path.write_text(f"#!/bin/sh\n{body}", encoding="utf-8")
        path.chmod(0o755)
        return path

    def _write_server_executable(self, directory: Path) -> Path:
        """Build a tiny exact-argv fake Herdr server for identity-gated tests."""
        source = r"""
#include <unistd.h>
int main(void) {
    for (;;) pause();
}
"""
        path = directory / "fake-herdr-server"
        result = subprocess.run(
            ["cc", "-x", "c", "-O2", "-o", str(path), "-"],
            input=source,
            text=True,
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(result.stderr)
        path.chmod(0o755)
        return path

    def _run_bash(
        self, script: str, *, env: dict[str, str], timeout: float = 5
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", "-c", textwrap.dedent(script)],
            env=env,
            text=True,
            capture_output=True,
            check=False,
            timeout=timeout,
        )

    def _base_env(self, *, herdr: Path) -> dict[str, str]:
        env = os.environ.copy()
        env["HERDR_BIN_PATH"] = str(herdr)
        # Do not let a caller's E2E session or managed-pane settings affect the
        # source-only shell harness below.
        for key in (
            "E2E_SESSION",
            "E2E_SESSION_SOCKET",
            "E2E_FAKE_MANAGED_ZDOT",
            "E2E_MANAGED_ZDOTDIR",
            "HERDR_BIN_RESOLVED",
        ):
            env.pop(key, None)
        return env

    def test_function_named_herdr_cannot_redirect_path_resolution(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake_dir = root / "bin"
            fake_dir.mkdir()
            executable = self._write_executable(fake_dir, "herdr", "exit 0\n")
            env = os.environ.copy()
            env.pop("HERDR_BIN_PATH", None)
            env.pop("HERDR_BIN_RESOLVED", None)
            env["PATH"] = f"{fake_dir}{os.pathsep}{env['PATH']}"

            result = self._run_bash(
                f"""
                herdr() {{ printf '%s\\n' 'malicious exported function'; }}
                export -f herdr
                source {shlex.quote(str(E2E_LIB))}
                e2e_resolve_herdr_bin
                printf '%s\\n' "$HERDR_BIN"
                """,
                env=env,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(executable))

    def test_absolute_herdr_bin_path_is_not_rewritten(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake = self._write_executable(root, "herdr-absolute", "exit 0\n")
            env = self._base_env(herdr=fake)

            result = self._run_bash(
                f"""
                source {shlex.quote(str(E2E_LIB))}
                e2e_resolve_herdr_bin
                printf '%s\\n' "$HERDR_BIN"
                """,
                env=env,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(fake))

    def test_session_name_collision_is_rejected_before_server_launch(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name = "hb-test-existing"
            server_log = root / "server.log"
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = self._write_executable(
                fake_bin,
                "herdr",
                """
                if [ "$1" = "--session" ] && [ "$2" = "hb-test-existing" ] && [ "$3" = "server" ]; then
                    printf '%s\\n' server >> "$SERVER_LOG"
                    exit 0
                fi
                if [ "$1" = "session" ] && [ "$2" = "list" ]; then
                    printf '{"sessions":[{"name":"hb-test-existing","socket_path":"%s"}]}\\n' "$COLLISION_SOCKET"
                    exit 0
                fi
                exit 99
                """,
            )
            # Make the old 15-second retry loop instantaneous while retaining
            # the real command ordering under test.
            self._write_executable(fake_bin, "sleep", "exit 0\n")
            env = self._base_env(herdr=fake)
            env["SERVER_LOG"] = str(server_log)
            env["COLLISION_SOCKET"] = ""

            result = self._run_bash(
                f"""
                source {shlex.quote(str(E2E_LIB))}
                e2e_session_boot {shlex.quote(name)} SOCK PID
                """,
                env={**env, "PATH": f"{fake_bin}{os.pathsep}{env['PATH']}"},
            )

            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertFalse(server_log.exists(), "collision must be checked before server")

    def test_session_name_collision_rejects_stale_and_nonherdr_sockets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name = "hb-test-existing"
            server_log = root / "server.log"
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = self._write_executable(
                fake_bin,
                "herdr",
                f"""
                if [ "$1" = "--session" ] && [ "$2" = "{name}" ] && [ "$3" = "server" ]; then
                    printf '%s\\n' server >> "$SERVER_LOG"
                    exit 0
                fi
                if [ "$1" = "session" ] && [ "$2" = "list" ]; then
                    printf '{{"sessions":[{{"name":"{name}","socket_path":"%s"}}]}}\\n' "$COLLISION_SOCKET"
                    exit 0
                fi
                exit 99
                """,
            )
            env = self._base_env(herdr=fake)
            env["SERVER_LOG"] = str(server_log)
            path_cases = [("stale", str(root / "stale.sock"), None)]
            nonherdr = _NonHerdrSocket(root / "non-herdr.sock")
            nonherdr.start()
            path_cases.append(("non-Herdr", str(nonherdr.path), nonherdr))
            try:
                for label, collision_socket, _ in path_cases:
                    with self.subTest(label=label):
                        result = self._run_bash(
                            f"""
                            source {shlex.quote(str(E2E_LIB))}
                            e2e_session_boot {shlex.quote(name)} SOCK PID
                            """,
                            env={
                                **env,
                                "COLLISION_SOCKET": collision_socket,
                                "PATH": f"{fake_bin}{os.pathsep}{env['PATH']}",
                            },
                        )
                        self.assertNotEqual(result.returncode, 0, result.stdout)
                        self.assertFalse(server_log.exists(), "collision must precede server launch")
                        server_log.unlink(missing_ok=True)
            finally:
                nonherdr.close()

    def test_standalone_init_failure_removes_owned_fake_managed_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            managed = Path(tempfile.mkdtemp(prefix="hb-e2e-managed.", dir="/tmp"))
            fake_herdr = self._write_executable(root, "herdr", "exit 0\\n")
            try:
                (managed / "home").mkdir()
                (managed / "zdot").mkdir()
                for directory in (managed, managed / "home", managed / "zdot"):
                    directory.chmod(0o700)
                result = self._run_bash(
                    f"""
                    source {shlex.quote(str(E2E_LIB))}
                    export HERDR_BIN_PATH={shlex.quote(str(fake_herdr))}
                    export HERDR_BIN={shlex.quote(str(fake_herdr))}
                    unset E2E_INVOCATION_TOKEN
                    export E2E_OWNER_ID=standalone-$$
                    export E2E_MANAGED_ROOT={shlex.quote(str(managed))}
                    export E2E_LOCAL_MANAGED_ROOT="$E2E_MANAGED_ROOT"
                    export E2E_MANAGED_ROOT_CREATOR_PID=$$
                    e2e_scenario_root_ensure
                    printf 'herdr-board fake-managed boundary\\nowner=%s\\ntoken=%s\\n' \\
                        "$E2E_OWNER_ID" "$E2E_INVOCATION_TOKEN" >"$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
                    chmod 600 "$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
                    # Fail before reserved-session boot, after init has installed
                    # its cleanup trap and recorded the exact managed root.
                    export E2E_FAKE_AGENT={shlex.quote(str(root / 'missing-agent'))}
                    ( e2e_init )
                    status=$?
                    printf 'init-status=%s root-exists=%s\\n' "$status" "$([ -e {shlex.quote(str(managed))} ] && echo yes || echo no)"
                    [ "$status" -ne 0 ] && [ ! -e {shlex.quote(str(managed))} ]
                    """,
                    env=os.environ.copy(),
                )
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                self.assertNotIn("Booting ephemeral", result.stdout)
                self.assertIn("init-status=1 root-exists=no", result.stdout)
            finally:
                # Keep the RED test itself from leaking a managed root.
                import shutil
                shutil.rmtree(managed, ignore_errors=True)

    def test_exited_boot_process_cannot_adopt_coincident_session(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name = "hb-test-exited"
            socket_path = root / "coincident.sock"
            server_log = root / "server.log"
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = self._write_executable(
                fake_bin,
                "herdr",
                f"""
                if [ "$1" = "--session" ] && [ "$2" = "{name}" ] && [ "$3" = "server" ]; then
                    printf '%s\\n' server >> "$SERVER_LOG"
                    /bin/sleep 0.1
                    exit 0
                fi
                if [ "$1" = "session" ] && [ "$2" = "list" ]; then
                    calls=0
                    if [ -f "$SESSION_LIST_STATE" ]; then
                        calls=$(cat "$SESSION_LIST_STATE")
                    fi
                    calls=$((calls + 1))
                    printf '%s\\n' "$calls" > "$SESSION_LIST_STATE"
                    if [ "$calls" -eq 1 ]; then
                        printf '%s\\n' '{{"sessions":[]}}'
                    else
                        /bin/sleep 0.1
                        printf '%s\\n' {shlex.quote(json.dumps({'sessions': [{'name': name, 'socket_path': str(socket_path)}]}))}
                    fi
                    exit 0
                fi
                exit 99
                """,
            )
            env = self._base_env(herdr=fake)
            env["SERVER_LOG"] = str(server_log)
            env["SESSION_LIST_STATE"] = str(root / "session-list.state")
            rpc_socket = _RpcSocket(socket_path)
            rpc_socket.start()
            try:
                result = self._run_bash(
                    f"""
                    source {shlex.quote(str(E2E_LIB))}
                    e2e_session_boot {shlex.quote(name)} SOCK PID
                    printf 'booted %s %s\\n' "$SOCK" "$PID"
                    """,
                    env=env,
                )
            finally:
                rpc_socket.close()

            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertNotIn("booted ", result.stdout)
            self.assertTrue(server_log.exists(), "the fake boot process should have run")

    @unittest.skipUnless(Path("/proc").is_dir(), "process identity tests require Linux /proc")
    def test_process_identity_token_is_proc_bound_and_exact(self) -> None:
        """The RED contract for the identity primitive used by e2e cleanup.

        The child is deliberately an ordinary local Python sleeper: this test
        must not need Herdr, a provider, or a real session.  The command line
        carries the expected session/name context so a PID-only check cannot
        satisfy this contract.
        """
        session = "hb-test-identity-session"
        name = "hb-test-identity-name"
        context = "hb-identity-cmdline-context"
        result = self._run_bash(
            f"""
            set -u
            source {shlex.quote(str(E2E_LIB))}
            python3 -c 'import time; time.sleep(30)' \\
                --session {shlex.quote(session)} --name {shlex.quote(name)} \\
                --identity-context {shlex.quote(context)} &
            pid=$!
            cleanup() {{ kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }}
            trap cleanup EXIT
            for _ in 1 2 3 4 5; do kill -0 "$pid" 2>/dev/null && break; sleep 0.02; done
            kill -0 "$pid" 2>/dev/null
            start=$(awk '{{print $22}}' "/proc/$pid/stat")
            exe=$(readlink "/proc/$pid/exe")
            cmdline=$(tr '\\0' ' ' < "/proc/$pid/cmdline")
            token=$(e2e_process_identity_capture "$pid" {shlex.quote(session)} {shlex.quote(name)})
            [ -n "$token" ]
            case "$token" in *"$start"*) ;; *) exit 21 ;; esac
            case "$token" in *"$exe"*) ;; *) exit 22 ;; esac
            case "$token" in *"{session}"*) ;; *) exit 23 ;; esac
            case "$token" in *"{name}"*) ;; *) exit 24 ;; esac
            case "$token" in *"--session"*"--name"*) ;; *) exit 25 ;; esac
            case "$token" in *"{context}"*) ;; *) exit 26 ;; esac
            case "$cmdline" in *"{context}"*) ;; *) exit 27 ;; esac
            e2e_process_identity_verify "$pid" "$token"

            altered_start=${{token/$start/0}}
            [ "$altered_start" != "$token" ]
            if e2e_process_identity_verify "$pid" "$altered_start"; then exit 31; fi
            altered_exe=${{token/$exe/definitely-not-the-executable}}
            [ "$altered_exe" != "$token" ]
            if e2e_process_identity_verify "$pid" "$altered_exe"; then exit 32; fi
            altered_name=${{token/{name}/different-name}}
            [ "$altered_name" != "$token" ]
            if e2e_process_identity_verify "$pid" "$altered_name"; then exit 33; fi
            altered_session=${{token/{session}/different-session}}
            [ "$altered_session" != "$token" ]
            if e2e_process_identity_verify "$pid" "$altered_session"; then exit 34; fi
            """,
            env=os.environ.copy(),
        )
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    @unittest.skipUnless(Path("/proc").is_dir(), "process identity tests require Linux /proc")
    def test_mismatched_identity_cannot_abort_or_teardown_live_session(self) -> None:
        """A live but reused PID must not authorize Herdr stop/delete."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            calls = root / "herdr-calls.log"
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = self._write_executable(
                fake_bin,
                "herdr",
                'printf "%s\\n" "$*" >> "$HERDR_CALLS"\nexit 0\n',
            )
            env = self._base_env(herdr=fake)
            env["HERDR_CALLS"] = str(calls)
            server = self._write_server_executable(root)
            session = "hb-test-live-session"

            for operation in ("e2e_session_abort_owned", "e2e_session_teardown"):
                with self.subTest(operation=operation):
                    calls.unlink(missing_ok=True)
                    result = self._run_bash(
                        f"""
                        set -u
                        source {shlex.quote(str(E2E_LIB))}
                        server={shlex.quote(str(server))}
                        owner_token=live-session-fixture-token
                        env E2E_HERDR_OWNER_TOKEN="$owner_token" "$server" \\
                            --session {shlex.quote(session)} server >/dev/null 2>&1 &
                        pid=$!
                        cleanup() {{ kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }}
                        trap cleanup EXIT
                        for _ in 1 2 3 4 5; do kill -0 "$pid" 2>/dev/null && break; sleep 0.02; done
                        kill -0 "$pid" 2>/dev/null
                        token=$(e2e_process_identity_capture "$pid" {shlex.quote(session)} {shlex.quote(session)} "$server" "$owner_token")
                        bad_token=${{token/{session}/reused-session}}
                        [ "$bad_token" != "$token" ]
                        if {operation} {shlex.quote(session)} "$pid" "$bad_token"; then exit 41; fi
                        kill -0 "$pid" 2>/dev/null
                        [ ! -s "$HERDR_CALLS" ]
                        """,
                        env=env,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    @unittest.skipUnless(Path("/proc").is_dir(), "process identity tests require Linux /proc")
    def test_workspace_close_identity_gate_covers_primary_and_secondary_shapes(self) -> None:
        """A reused live PID must not authorize a deferred workspace close.

        The primary workspace uses the primary session's PID/token globals.  A
        workspace in an explicitly selected secondary session supplies that
        session's PID/token after the socket argument.
        """
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            calls = root / "herdr-calls.log"
            fake = self._write_executable(
                root,
                "herdr",
                """
                if [ "$1" = workspace ] && [ "$2" = close ]; then
                    printf '%s\\n' "$*" >> "$HERDR_CALLS"
                    exit 0
                fi
                exit 99
                """,
            )
            env = self._base_env(herdr=fake)
            env["HERDR_CALLS"] = str(calls)
            session = "hb-test-workspace-session"
            name = "hb-test-workspace-server"
            secondary_socket = root / "secondary.sock"
            server = self._write_server_executable(root)

            for shape in ("primary", "secondary"):
                for identity_mode, expected_calls in (("mismatch", 0), ("exact", 1)):
                    with self.subTest(shape=shape, identity=identity_mode):
                        calls.unlink(missing_ok=True)
                        identity_arg = "identity" if identity_mode == "exact" else "bad_token"
                        if shape == "primary":
                            register = f'''
                            E2E_SESSION_PID="$pid"
                            E2E_SESSION_IDENTITY="${{{identity_arg}}}"
                            e2e_ws_defer_close workspace
                            '''
                        else:
                            # Keep the primary token wrong: the explicit
                            # secondary token must be the one that authorizes.
                            register = f'''
                            E2E_SESSION_PID="$pid"
                            E2E_SESSION_IDENTITY="$bad_token"
                            e2e_ws_defer_close workspace {shlex.quote(str(secondary_socket))} "$pid" "${{{identity_arg}}}"
                            '''
                        result = self._run_bash(
                            f"""
                            set -u
                            source {shlex.quote(str(E2E_LIB))}
                            e2e_scenario_root_ensure
                            secondary_socket={shlex.quote(str(secondary_socket))}
                            E2E_SESSION_SOCKET="$secondary_socket"
                            E2E_SESSION_SOCKETS[{shlex.quote(session)}]="$secondary_socket"
                            server={shlex.quote(str(server))}
                            owner_token=workspace-session-fixture-token
                            env E2E_HERDR_OWNER_TOKEN="$owner_token" "$server" \\
                                --session {shlex.quote(session)} server >/dev/null 2>&1 &
                            pid=$!
                            cleanup() {{ kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }}
                            trap cleanup EXIT
                            for _ in 1 2 3 4 5; do kill -0 "$pid" 2>/dev/null && break; sleep 0.02; done
                            kill -0 "$pid" 2>/dev/null
                            identity=$(e2e_process_identity_capture "$pid" {shlex.quote(session)} {shlex.quote(session)} "$server" "$owner_token")
                            bad_token=${{identity/{session}/reused-workspace-server}}
                            {textwrap.dedent(register)}
                            E2E_KEEP=0
                            e2e_cleanup
                            cleanup_status=$?
                            calls=$(wc -l <"$HERDR_CALLS" 2>/dev/null || printf '0')
                            if [ "$calls" -ne {expected_calls} ]; then exit 61; fi
                            if ! kill -0 "$pid" 2>/dev/null; then exit 62; fi
                            """,
                            env=env,
                        )
                        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    @unittest.skipUnless(Path("/proc").is_dir(), "process identity tests require Linux /proc")
    def test_daemon_stop_identity_gate_does_not_signal_reused_live_pid(self) -> None:
        """Daemon stop must verify its token before signaling, then reap exact owners."""
        result = self._run_bash(
            f"""
            set -u
            source {shlex.quote(str(E2E_LIB))}
            pid=""
            cleanup() {{
                if [ -n "${{pid:-}}" ]; then
                    kill "$pid" 2>/dev/null || true
                    wait "$pid" 2>/dev/null || true
                fi
            }}
            trap cleanup EXIT
            for mode in mismatch exact; do
                python3 -c 'import time; time.sleep(30)' \\
                    --session hb-test-daemon-session --name daemon-owner &
                pid=$!
                for _ in 1 2 3 4 5; do kill -0 "$pid" 2>/dev/null && break; sleep 0.02; done
                kill -0 "$pid" 2>/dev/null
                identity=$(e2e_process_identity_capture "$pid" hb-test-daemon-session daemon-owner)
                bad_token=${{identity/daemon-owner/reused-daemon-owner}}
                E2E_DAEMON_PID="$pid"
                if [ "$mode" = mismatch ]; then
                    E2E_DAEMON_IDENTITY="$bad_token"
                    e2e_daemon_stop || true
                    # The mismatched owner is still a live sleeper.
                    if ! kill -0 "$pid" 2>/dev/null; then exit 52; fi
                    kill "$pid"
                    wait "$pid" 2>/dev/null || true
                else
                    E2E_DAEMON_IDENTITY="$identity"
                    e2e_daemon_stop || true
                    # Exact ownership permits the helper to signal and reap it.
                    if kill -0 "$pid" 2>/dev/null; then exit 51; fi
                fi
                pid=""
            done
            """,
            env=os.environ.copy(),
        )
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    def test_cleanup_reports_owned_deferred_failure_when_scenario_succeeds(self) -> None:
        """Cleanup errors must not be erased when the scenario itself returned 0."""
        result = self._run_bash(
            f"""
            set -u
            source {shlex.quote(str(E2E_LIB))}
            owned_deferred_cleanup() {{ return 23; }}
            e2e_defer owned_deferred_cleanup
            e2e_cleanup
            status=$?
            [ "$status" -ne 0 ]
            """,
            env=os.environ.copy(),
        )
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    @unittest.skipUnless(Path("/proc").is_dir(), "process identity tests require Linux /proc")
    def test_session_cleanup_propagates_stop_failure_and_refuses_delete(self) -> None:
        """A failed stop remains visible and fail-closed delete is not attempted."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            calls = root / "herdr-calls.log"
            managed = Path(tempfile.mkdtemp(prefix="hb-e2e-managed.", dir="/tmp"))
            (managed / ".herdr-board-fake-managed").write_text("owned\n", encoding="utf-8")
            fake = self._write_executable(
                root,
                "herdr",
                """
                printf '%s\\n' "$*" >> "$HERDR_CALLS"
                case "$1 $2" in
                  "session stop") exit 7 ;;
                  "session delete") exit 8 ;;
                esac
                exit 99
                """,
            )
            env = self._base_env(herdr=fake)
            env["HERDR_CALLS"] = str(calls)
            server = self._write_server_executable(root)
            result = self._run_bash(
                f"""
                set -u
                source {shlex.quote(str(E2E_LIB))}
                unset E2E_INVOCATION_TOKEN
                export E2E_OWNER_ID=hb-test-cleanup
                e2e_scenario_root_ensure
                E2E_MANAGED_ROOT={shlex.quote(str(managed))}
                export E2E_LOCAL_MANAGED_ROOT="$E2E_MANAGED_ROOT"
                export E2E_MANAGED_ROOT_OWNER=hb-test-cleanup E2E_MANAGED_ROOT_CREATOR_PID=$$
                printf 'herdr-board fake-managed boundary\\nowner=%s\\ntoken=%s\\n' \\
                    "$E2E_OWNER_ID" "$E2E_INVOCATION_TOKEN" >"$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
                chmod 600 "$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
                e2e_root_resource_register managed managed-root "$E2E_MANAGED_ROOT" \\
                    "$E2E_MANAGED_ROOT/.herdr-board-fake-managed"
                e2e_defer "e2e_managed_root_remove_owned 'hb-test-cleanup'"
                server={shlex.quote(str(server))}
                owner_token=cleanup-session-fixture-token
                env E2E_HERDR_OWNER_TOKEN="$owner_token" "$server" \\
                    --session hb-test-cleanup server >/dev/null 2>&1 &
                pid=$!
                fallback() {{ kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }}
                trap fallback EXIT
                for _ in 1 2 3 4 5; do kill -0 "$pid" 2>/dev/null && break; sleep 0.02; done
                identity=$(e2e_process_identity_capture "$pid" hb-test-cleanup hb-test-cleanup "$server" "$owner_token")
                e2e_defer_session_teardown hb-test-cleanup "$pid" "$identity"
                e2e_cleanup
                status=$?
                [ "$status" -ne 0 ] || exit 71
                grep -qx 'session stop hb-test-cleanup' "$HERDR_CALLS" || exit 72
                ! grep -qx 'session delete hb-test-cleanup' "$HERDR_CALLS" || exit 73
                [ ! -e "$E2E_MANAGED_ROOT" ] || exit 74
                """,
                env=env,
            )
            try:
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            finally:
                if managed.exists():
                    shutil.rmtree(managed)

    def test_real_claude_cleanup_records_and_verifies_server_identity_before_mutation(self) -> None:
        """Static contract check; importing/running the provider smoke is unsafe."""
        source = REAL_CLAUDE_SMOKE.read_text(encoding="utf-8")
        self.assertRegex(source, r"SERVER_IDENTITY(?:_TOKEN)?=\"\"")

        state_start = source.index("write_state()")
        state_end = source.index("\n}\n", state_start)
        state_block = source[state_start:state_end]
        self.assertRegex(state_block, r"SERVER_IDENTITY(?:_TOKEN)?")

        cleanup_start = source.index("cleanup()")
        cleanup_end = source.index("\ntrap cleanup EXIT", cleanup_start)
        cleanup = source[cleanup_start:cleanup_end]
        verify = re.search(
            r"e2e_process_identity_verify\s+\"\$SERVER_PID\"\s+\"\$SERVER_IDENTITY(?:_TOKEN)?\"",
            cleanup,
        )
        self.assertIsNotNone(verify, "cleanup must verify the recorded server identity")
        guard = re.search(
            r"if\s+!?\s*e2e_process_identity_verify\s+\"\$SERVER_PID\"\s+\"\$SERVER_IDENTITY(?:_TOKEN)?\"\s*;\s*then",
            cleanup,
        )
        self.assertIsNotNone(guard, "identity verification must gate cleanup, not be advisory")
        workspace_mutation = cleanup.index("HERDR MUTATION: close disposable workspace")
        session_mutation = cleanup.index("HERDR MUTATION: stop/delete disposable session")
        assert verify is not None
        self.assertLess(verify.start(), workspace_mutation)
        self.assertLess(verify.start(), session_mutation)

        launch = source.index('"$HERDR_BIN" --session "$SESSION" server')
        pid_assignment = source.index("SERVER_PID=$!", launch)
        capture = re.search(
            r"e2e_process_identity_capture\s+\"\$SERVER_PID\"\s+\"\$SESSION\"",
            source[pid_assignment:],
        )
        self.assertIsNotNone(capture, "the server identity must be captured after $!")
        assert capture is not None
        capture_offset = pid_assignment + capture.start()
        self.assertGreater(capture_offset, pid_assignment)
        workspace_create = source.index("HERDR MUTATION: create one disposable workspace")
        self.assertLess(capture_offset, workspace_create)


if __name__ == "__main__":
    unittest.main()
