import copy
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


from _support import REPO_ROOT as ROOT
SPEC = importlib.util.spec_from_file_location(
    "e2e_process_identity", ROOT / "e2e" / "process_identity.py"
)
assert SPEC and SPEC.loader
identity = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = identity
SPEC.loader.exec_module(identity)

# A long-lived fixture child; its argv doubles as the session/name/command
# evidence the identity layer checks.
SLEEP_ARGV = (
    [sys.executable, "-c", "import time; time.sleep(30)"]
    if sys.platform == "win32"
    else ["/bin/sleep", "30"]
)


def inspect(pid):
    """Snapshot once the child is inspectable (Windows needs a moment)."""
    for _ in range(100):
        try:
            return identity.snapshot(pid)
        except identity.IdentityError:
            time.sleep(0.01)
    raise AssertionError("fixture child never became inspectable")


class PortableProcessIdentityTests(unittest.TestCase):
    def setUp(self):
        self.key = b"k" * 32
        self.owner = "0123456789abcdef0123456789abcdef"
        env = dict(os.environ, E2E_HERDR_OWNER_TOKEN=self.owner)
        self.child = subprocess.Popen(SLEEP_ARGV, env=env)
        for _ in range(50):
            try:
                identity.snapshot(self.child.pid)
                break
            except identity.IdentityError:
                time.sleep(0.01)
        else:
            self.fail("fixture child never became inspectable")

    def tearDown(self):
        if self.child.poll() is None:
            self.child.terminate()
        self.child.wait(timeout=5)

    def tokens(self):
        provisional = identity.provisional_capture(
            self.child.pid,
            self.owner,
            str(os.getpid()),
            "E2E_HERDR_OWNER_TOKEN",
            self.key,
        )
        stable = identity.stable_capture(
            self.child.pid,
            SLEEP_ARGV[0],
            SLEEP_ARGV[-1],
            SLEEP_ARGV[0],
            self.owner,
            "E2E_HERDR_OWNER_TOKEN",
            provisional,
            self.key,
        )
        return provisional, stable

    @unittest.skipIf(sys.platform == "win32", "Windows has no POSIX mode bits")
    def test_mode_is_portable_octal(self):
        with tempfile.TemporaryDirectory() as directory:
            os.chmod(directory, 0o700)
            self.assertEqual(identity.stat.S_IMODE(os.stat(directory).st_mode), 0o700)

    def test_snapshot_preserves_complete_argv(self):
        current = identity.snapshot(self.child.pid)
        self.assertEqual(current.cmdline, SLEEP_ARGV)
        self.assertEqual(current.parent_pid, str(os.getpid()))
        self.assertTrue(current.start_time)
        self.assertTrue(Path(current.exe).is_absolute())

    def test_linux_cmdline_parser_preserves_trailing_empty_argument(self):
        raw = b"/usr/bin/example\0ordinary\0spaced argument\0\0"
        self.assertEqual(
            identity._parse_linux_cmdline(raw),
            ["/usr/bin/example", "ordinary", "spaced argument", ""],
        )
        for malformed in (b"", b"/usr/bin/example\0ordinary"):
            with self.subTest(raw=malformed):
                with self.assertRaises(identity.IdentityError):
                    identity._parse_linux_cmdline(malformed)

    def test_snapshot_preserves_empty_and_spaced_arguments(self):
        child = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(30)", "odd value", ""]
        )
        try:
            current = inspect(child.pid)
            self.assertEqual(current.cmdline[-2:], ["odd value", ""])
        finally:
            child.terminate()
            child.wait(timeout=5)

    def test_signed_provisional_and_stable_identity_verify(self):
        provisional, stable = self.tokens()
        identity.validate_token(provisional, self.key)
        identity.validate_token(stable, self.key)
        self.assertTrue(identity.verify_identity(self.child.pid, provisional, self.key))
        self.assertTrue(identity.verify_identity(self.child.pid, stable, self.key))
        self.assertTrue(
            identity.provisional_transition_verify(
                self.child.pid,
                provisional,
                str(os.getpid()),
                SLEEP_ARGV[0],
                "unused",
                "session",
                "E2E_HERDR_OWNER_TOKEN",
                self.key,
            )
        )

    def test_every_identity_field_tamper_fails(self):
        _, stable = self.tokens()
        mutations = {
            "signature": "0" * 64,
            "start_time": "0:0",
            "exe": "/bin/false",
            "cmdline": ["/bin/false"],
            "owner_token": "wrong",
            "parent_pid": "1",
        }
        for field, value in mutations.items():
            with self.subTest(field=field):
                tampered = copy.deepcopy(stable)
                tampered[field] = value
                self.assertFalse(
                    identity.verify_identity(self.child.pid, tampered, self.key)
                )

    def test_wrong_key_and_missing_provisional_fail(self):
        provisional, _ = self.tokens()
        with self.assertRaises(identity.IdentityError):
            identity.validate_token(provisional, b"z" * 32)
        with self.assertRaises(identity.IdentityError):
            identity.stable_capture(
                self.child.pid,
                SLEEP_ARGV[0],
                SLEEP_ARGV[-1],
                SLEEP_ARGV[0],
                self.owner,
                "E2E_HERDR_OWNER_TOKEN",
                {},
                self.key,
            )

    def test_dead_process_is_not_verified(self):
        _, stable = self.tokens()
        self.child.terminate()
        self.child.wait(timeout=5)
        self.assertFalse(identity.verify_identity(self.child.pid, stable, self.key))


@unittest.skipUnless(sys.platform == "win32", "Windows backend")
class WindowsSnapshotTest(unittest.TestCase):
    def test_snapshot_of_a_child_reads_exe_cmdline_and_environ(self) -> None:
        env = dict(os.environ, HB_PROBE_TOKEN="tok-123")
        child = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(30)"], env=env
        )
        try:
            for _ in range(100):
                try:
                    snap = identity.snapshot(child.pid)
                    break
                except identity.IdentityError:
                    time.sleep(0.01)
            else:
                self.fail("child never became inspectable")
            self.assertEqual(
                os.path.normcase(snap.exe), os.path.normcase(sys.executable)
            )
            self.assertEqual(snap.cmdline[1:], ["-c", "import time; time.sleep(30)"])
            self.assertIn(b"HB_PROBE_TOKEN=tok-123", snap.environ)
            self.assertEqual(snap.parent_pid, str(os.getpid()))
            self.assertEqual(snap.state, "R")
            self.assertTrue(identity.process_exists(child.pid))
        finally:
            child.kill()
            child.wait()
        self.assertFalse(identity.process_exists(child.pid))

    def test_direct_child_accepts_only_self_or_an_msys_bash_stub_parent(self) -> None:
        me = str(os.getpid())
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        try:
            snap = inspect(child.pid)
            self.assertTrue(identity.is_direct_child(snap, me))
            self.assertFalse(identity.is_direct_child(snap, "4"))  # System
            # A python.exe grandchild through a python.exe parent is not an MSYS
            # fork stub, so it must not count as a direct child of this process.
            grand = subprocess.Popen(
                [sys.executable, "-c",
                 "import subprocess,sys,time; p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); print(p.pid, flush=True); time.sleep(30)"],
                stdout=subprocess.PIPE, text=True,
            )
            try:
                grandchild = int(grand.stdout.readline())
                self.assertFalse(identity.is_direct_child(inspect(grandchild), me))
            finally:
                grand.kill()
                grand.wait()
                subprocess.run(["taskkill", "/F", "/PID", str(grandchild)], capture_output=True)
        finally:
            child.kill()
            child.wait()

    def test_private_mode_holds_only_inside_the_user_profile(self) -> None:
        with tempfile.TemporaryDirectory() as directory:  # under %TEMP%, in the profile
            file = Path(directory) / "marker"
            file.write_text("x", encoding="utf-8")
            self.assertEqual(identity.private_mode(directory), "700")
            self.assertEqual(identity.private_mode(str(file)), "600")
        outside = os.environ.get("SystemRoot", r"C:\Windows")
        self.assertNotIn(identity.private_mode(outside), {"600", "700"})

    def test_process_exists_never_terminates_the_process(self) -> None:
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        try:
            self.assertTrue(identity.process_exists(child.pid))
            time.sleep(0.3)  # TerminateProcess is asynchronous
            self.assertIsNone(child.poll(), "probing must not kill the process")
        finally:
            child.kill()
            child.wait()


if __name__ == "__main__":
    unittest.main()
