import copy
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "e2e_process_identity", ROOT / "e2e" / "process_identity.py"
)
assert SPEC and SPEC.loader
identity = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = identity
SPEC.loader.exec_module(identity)


class PortableProcessIdentityTests(unittest.TestCase):
    def setUp(self):
        self.key = b"k" * 32
        self.owner = "0123456789abcdef0123456789abcdef"
        env = dict(os.environ, E2E_HERDR_OWNER_TOKEN=self.owner)
        self.child = subprocess.Popen(["/bin/sleep", "30"], env=env)
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
            "/bin/sleep",
            "30",
            "/bin/sleep",
            self.owner,
            "E2E_HERDR_OWNER_TOKEN",
            provisional,
            self.key,
        )
        return provisional, stable

    def test_mode_is_portable_octal(self):
        with tempfile.TemporaryDirectory() as directory:
            os.chmod(directory, 0o700)
            self.assertEqual(identity.stat.S_IMODE(os.stat(directory).st_mode), 0o700)

    def test_snapshot_preserves_complete_argv(self):
        current = identity.snapshot(self.child.pid)
        self.assertEqual(current.cmdline, ["/bin/sleep", "30"])
        self.assertEqual(current.parent_pid, str(os.getpid()))
        self.assertTrue(current.start_time)
        self.assertTrue(Path(current.exe).is_absolute())

    def test_snapshot_preserves_empty_and_spaced_arguments(self):
        child = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(30)", "odd value", ""]
        )
        try:
            current = identity.snapshot(child.pid)
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
                "/bin/sleep",
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
                "/bin/sleep",
                "30",
                "/bin/sleep",
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


if __name__ == "__main__":
    unittest.main()
