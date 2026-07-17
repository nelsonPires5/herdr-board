from __future__ import annotations

import hashlib
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "install-cli.sh"
MARKER_NAME = ".herdr-board-cli-managed"
MARKER_PREFIX = "herdr-board install-cli.sh managed board sha256:"

# Absolute path to the Python interpreter running this test suite, used
# in stub shebangs so stubs work inside isolated PATHs that lack /usr/bin.
_PYTHON3 = sys.executable


def marker_bytes(content: bytes) -> bytes:
    checksum = hashlib.sha256(content).hexdigest()
    return f"{MARKER_PREFIX}{checksum}\n".encode()


def _write_stub(dir_path: Path, name: str, body: str) -> Path:
    """Write an executable stub with a hardcoded python3 shebang.
    Unlinks any existing entry first so we can replace a symlink."""
    stub = dir_path / name
    try:
        stub.unlink()
    except FileNotFoundError:
        pass
    stub.write_text(f"#!{_PYTHON3}\n{body}", encoding="utf-8")
    stub.chmod(0o755)
    return stub


# ---------------------------------------------------------------------------
# minimal cross-platform stubs (pure Python 3 stdlib, no external deps)
# ---------------------------------------------------------------------------

# Checksum stubs.  sha256sum is called bare; shasum receives '-a 256'.
_SHA256SUM_BODY = (
    "import hashlib, sys\n"
    "print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest() + '  -')\n"
)

_SHASUM_BODY = (
    "import hashlib, sys\n"
    "assert sys.argv[1:] == ['-a', '256'], f'shasum stub expected -a 256, got {sys.argv[1:]}'\n"
    "print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest() + '  -')\n"
)


def _ln_race_body() -> str:
    """ln stub that races the destination into a directory, resolving the
    real ln via shutil.which at construction time."""
    real_ln = shutil.which("ln")
    if real_ln is None:
        raise FileNotFoundError("Cannot find ln on PATH")
    return (
        "import os, sys\n"
        f"REAL_LN = {real_ln!r}\n"
        "dest = sys.argv[-1]\n"
        "try:\n"
        "    os.mkdir(dest, 0o755)\n"
        "except FileExistsError:\n"
        "    pass\n"
        "os.execv(REAL_LN, ['ln'] + sys.argv[1:])\n"
    )


def _mv_marker_race_body() -> str:
    """mv stub that races the marker destination into a directory before
    delegating to the real mv."""
    real_mv = shutil.which("mv")
    if real_mv is None:
        raise FileNotFoundError("Cannot find mv on PATH")
    return (
        "import os, sys\n"
        f"REAL_MV = {real_mv!r}\n"
        "dest = sys.argv[-1]\n"
        "if os.path.basename(dest) == '.herdr-board-cli-managed':\n"
        "    try:\n"
        "        os.mkdir(dest, 0o755)\n"
        "    except FileExistsError:\n"
        "        pass\n"
        "os.execv(REAL_MV, ['mv'] + sys.argv[1:])\n"
    )


class InstallCliTests(unittest.TestCase):
    # ------------------------------------------------------------------
    # helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _setup_isolated_tools(
        tools: Path,
        *,
        with_sha256sum: bool = False,
        with_shasum: bool = False,
    ) -> None:
        """Populate *tools* with symlinks to every external command the
        install script needs.  The caller should set PATH to *tools*
        alone (no /bin or /usr/bin) for a fully isolated environment.

        Checksum tools are only added when the corresponding flag is
        True; when False the tool is deliberately absent from PATH so
        fallback and error paths can be tested deterministically."""
        tools.mkdir(parents=True, exist_ok=True)
        # Every external binary referenced by install-cli.sh, plus the
        # shell used to execute stubs and the script itself.
        for cmd in ("ln", "mv", "mkdir", "cp", "chmod", "rm",
                    "dirname", "mktemp", "bash", "sh"):
            real = shutil.which(cmd)
            if real is None:
                raise FileNotFoundError(f"Cannot find {cmd} on PATH")
            (tools / cmd).symlink_to(real)
        if with_sha256sum:
            _write_stub(tools, "sha256sum", _SHA256SUM_BODY)
        if with_shasum:
            _write_stub(tools, "shasum", _SHASUM_BODY)

    @staticmethod
    def make_fixture(
        root: Path, content: str = "#!/bin/sh\necho first\n"
    ) -> tuple[Path, Path]:
        script = root / "scripts" / "install-cli.sh"
        script.parent.mkdir(parents=True)
        shutil.copy2(SCRIPT, script)
        source = root / "target" / "release" / "board"
        source.parent.mkdir(parents=True)
        source.write_text(content, encoding="utf-8")
        source.chmod(0o751)
        return script, source

    def run_script(
        self,
        script: Path,
        *,
        home: Path,
        install_dir: Path | None = None,
        path_prefix: Path | None = None,
        full_path: str | None = None,
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["HOME"] = str(home)
        if full_path is not None:
            env["PATH"] = full_path
        elif path_prefix is not None:
            env["PATH"] = f"{path_prefix}{os.pathsep}{env['PATH']}"
        if install_dir is None:
            env.pop("HERDR_BOARD_CLI_INSTALL_DIR", None)
        else:
            env["HERDR_BOARD_CLI_INSTALL_DIR"] = str(install_dir)
        return subprocess.run(
            ["bash", str(script)],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    # ------------------------------------------------------------------
    # stock host install / update (real system tools, default PATH)
    # ------------------------------------------------------------------

    def test_first_install_creates_ownership_marker(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"

            result = self.run_script(script, home=home)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(destination.read_bytes(), source.read_bytes())
            self.assertFalse(destination.is_symlink())
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o751)
            marker = install_dir / MARKER_NAME
            self.assertTrue(marker.is_file())
            self.assertEqual(marker.read_bytes(), marker_bytes(source.read_bytes()))

    def test_managed_install_updates_idempotently(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"
            marker = install_dir / MARKER_NAME

            first = self.run_script(script, home=home)
            self.assertEqual(first.returncode, 0, first.stderr)
            original_marker = marker.read_bytes()

            source.write_text("#!/bin/sh\necho second\n", encoding="utf-8")
            source.chmod(0o755)
            second = self.run_script(script, home=home)

            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(destination.read_bytes(), source.read_bytes())
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o755)
            self.assertNotEqual(marker.read_bytes(), original_marker)
            self.assertEqual(marker.read_bytes(), marker_bytes(source.read_bytes()))

    # ------------------------------------------------------------------
    # safety tests
    # ------------------------------------------------------------------

    def test_refuses_unrelated_existing_board_without_changing_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, _source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"
            install_dir.mkdir(parents=True)
            destination.write_bytes(b"unrelated executable\n")
            destination.chmod(0o700)

            result = self.run_script(script, home=home)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing to overwrite", result.stderr)
            self.assertEqual(destination.read_bytes(), b"unrelated executable\n")
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o700)
            self.assertFalse((install_dir / MARKER_NAME).exists())

    def test_refuses_other_unmanaged_destination_types(self) -> None:
        for destination_type in ("broken symlink", "directory"):
            with self.subTest(destination_type=destination_type), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp) / "repo"
                home = Path(tmp) / "home"
                script, _source = self.make_fixture(root)
                install_dir = home / ".local" / "bin"
                destination = install_dir / "board"
                install_dir.mkdir(parents=True)
                if destination_type == "broken symlink":
                    destination.symlink_to(install_dir / "missing-target")
                else:
                    destination.mkdir()
                    (destination / "keep").write_text("untouched", encoding="utf-8")

                result = self.run_script(script, home=home)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("refusing to overwrite", result.stderr)
                if destination_type == "broken symlink":
                    self.assertTrue(destination.is_symlink())
                    self.assertEqual(os.readlink(destination), str(install_dir / "missing-target"))
                else:
                    self.assertEqual((destination / "keep").read_text(encoding="utf-8"), "untouched")

    def test_managed_update_refuses_replaced_board_and_preserves_both_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"
            marker = install_dir / MARKER_NAME

            first = self.run_script(script, home=home)
            self.assertEqual(first.returncode, 0, first.stderr)
            original_marker = marker.read_bytes()

            replacement = b"#!/bin/sh\necho installed by another tool\n"
            destination.write_bytes(replacement)
            destination.chmod(0o700)
            source.write_text("#!/bin/sh\necho second\n", encoding="utf-8")
            source.chmod(0o755)

            update = self.run_script(script, home=home)

            self.assertNotEqual(update.returncode, 0)
            self.assertIn("checksum does not match", update.stderr)
            self.assertEqual(destination.read_bytes(), replacement)
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o700)
            self.assertEqual(marker.read_bytes(), original_marker)

    def test_managed_update_refuses_destination_changed_to_symlinked_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"
            marker = install_dir / MARKER_NAME

            first = self.run_script(script, home=home)
            self.assertEqual(first.returncode, 0, first.stderr)
            original_marker = marker.read_bytes()

            destination.unlink()
            symlink_target = Path(tmp) / "other-tool-directory"
            symlink_target.mkdir()
            keep = symlink_target / "keep"
            keep.write_text("untouched", encoding="utf-8")
            destination.symlink_to(symlink_target, target_is_directory=True)
            source.write_text("#!/bin/sh\necho second\n", encoding="utf-8")
            source.chmod(0o755)

            update = self.run_script(script, home=home)

            self.assertNotEqual(update.returncode, 0)
            self.assertIn("not a regular non-symlink file", update.stderr)
            self.assertTrue(destination.is_symlink())
            self.assertEqual(os.readlink(destination), str(symlink_target))
            self.assertEqual(list(symlink_target.iterdir()), [keep])
            self.assertEqual(keep.read_text(encoding="utf-8"), "untouched")
            self.assertEqual(marker.read_bytes(), original_marker)

    def test_install_directory_override(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            custom_bin = Path(tmp) / "custom" / "bin"
            script, source = self.make_fixture(root)

            result = self.run_script(script, home=home, install_dir=custom_bin)
            destination = custom_bin / "board"
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(destination.read_bytes(), source.read_bytes())
            self.assertTrue((custom_bin / MARKER_NAME).is_file())
            self.assertFalse((home / ".local" / "bin" / "board").exists())

    def test_missing_built_binary_has_useful_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            source.unlink()

            result = self.run_script(script, home=home)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("expected executable source binary", result.stderr)
            self.assertIn("scripts/build.sh", result.stderr)

    def test_invalid_checksum_tool_output_has_useful_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, _source = self.make_fixture(root)
            fake_bin = Path(tmp) / "fake-bin"
            fake_bin.mkdir()
            fake = fake_bin / "sha256sum"
            fake.write_text("#!/bin/sh\nprintf 'not-a-checksum  -\\n'\n", encoding="utf-8")
            fake.chmod(0o755)

            result = self.run_script(script, home=home, path_prefix=fake_bin)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("sha256sum returned an invalid checksum", result.stderr)
            install_dir = home / ".local" / "bin"
            self.assertFalse((install_dir / "board").exists())
            self.assertFalse((install_dir / MARKER_NAME).exists())

    # ------------------------------------------------------------------
    # portability
    # ------------------------------------------------------------------

    def test_shasum_fallback_when_sha256sum_missing(self) -> None:
        """First install succeeds with only shasum (no sha256sum)."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"

            tools = Path(tmp) / "tools"
            # Provide shasum but NOT sha256sum in a fully isolated PATH.
            self._setup_isolated_tools(tools, with_shasum=True, with_sha256sum=False)

            result = self.run_script(script, home=home,
                                     full_path=str(tools))

            self.assertEqual(result.returncode, 0,
                             f"expected success with shasum, got: {result.stderr}")
            self.assertEqual(destination.read_bytes(), source.read_bytes())
            self.assertFalse(destination.is_symlink())
            marker = install_dir / MARKER_NAME
            self.assertTrue(marker.is_file())
            self.assertEqual(marker.read_bytes(), marker_bytes(source.read_bytes()))

    def test_rejects_when_no_checksum_tool_available(self) -> None:
        """Script errors clearly when neither sha256sum nor shasum exists."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, _source = self.make_fixture(root)

            tools = Path(tmp) / "tools"
            # Isolated PATH: no checksum tool at all.
            self._setup_isolated_tools(tools, with_sha256sum=False, with_shasum=False)

            result = self.run_script(script, home=home,
                                     full_path=str(tools))

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("sha256sum", result.stderr.lower())
            self.assertIn("shasum", result.stderr.lower())
            install_dir = home / ".local" / "bin"
            self.assertFalse((install_dir / "board").exists())
            self.assertFalse((install_dir / MARKER_NAME).exists())

    def test_no_gnu_only_T_flags(self) -> None:
        """Static assertion: the script must not depend on GNU-only -T."""
        text = SCRIPT.read_text()
        # Strip comments so we don't flag mentions in prose like
        # "# Portable replacement for ln -T".
        active = re.sub(r'^\s*#.*$', '', text, flags=re.MULTILINE)
        self.assertNotIn("ln -T", active, "script contains GNU-only ln -T")
        self.assertNotIn("mv -T", active, "script contains GNU-only mv -T")
        self.assertNotIn("mv -fT", active, "script contains GNU-only mv -fT")

    def test_first_install_race_directory_replaces_destination(self) -> None:
        """ln TOCTOU: fake ln creates a directory at the destination
        after the [ -d ] check passes but before the real ln runs.
        Expects nonzero exit, no marker, and no leaked link inside the
        raced directory."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"

            tools = Path(tmp) / "tools"
            tools.mkdir()
            _write_stub(tools, "ln", _ln_race_body())

            result = self.run_script(script, home=home, path_prefix=tools)

            self.assertNotEqual(result.returncode, 0,
                                "script should fail when destination is raced into a directory")

            self.assertFalse((install_dir / MARKER_NAME).exists(),
                             "marker must not exist after a raced first install")

            if destination.is_dir():
                leaked = list(destination.iterdir())
                self.assertEqual(len(leaked), 0,
                                 f"no leaked hard link inside raced directory: {leaked}")

    # ------------------------------------------------------------------
    # Marker-placement race: a fake mv turns the marker destination into
    # a directory immediately before the real mv.  The script's marker
    # postcondition catches this, exits non-zero, cleans up any leaked
    # marker inside the raced directory, and rolls back the board binary.
    # ------------------------------------------------------------------

    def test_first_install_marker_race_directory_replaces_marker(self) -> None:
        """Marker-placement race: fake mv creates a directory where the
        ownership marker should go.  Expects nonzero exit, installed board
        removed (rollback), and no leaked marker inside the raced directory."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            home = Path(tmp) / "home"
            script, source = self.make_fixture(root)
            install_dir = home / ".local" / "bin"
            destination = install_dir / "board"
            marker_path = install_dir / MARKER_NAME

            tools = Path(tmp) / "tools"
            self._setup_isolated_tools(tools, with_sha256sum=True)
            _write_stub(tools, "mv", _mv_marker_race_body())

            result = self.run_script(script, home=home, full_path=str(tools))

            self.assertNotEqual(result.returncode, 0,
                               "script should fail when marker is raced into a directory")

            # The board binary MUST be removed — the install was not completed.
            self.assertFalse(destination.exists(),
                             "board must be rolled back after marker placement failure")

            # No leaked marker file inside the raced directory.
            if marker_path.is_dir():
                leaked = list(marker_path.iterdir())
                self.assertEqual(len(leaked), 0,
                                 f"no leaked marker inside raced directory: {leaked}")


class PluginManifestTests(unittest.TestCase):
    def test_build_runs_before_managed_cli_install(self) -> None:
        with (REPO_ROOT / "herdr-plugin.toml").open("rb") as manifest_file:
            manifest = tomllib.load(manifest_file)

        self.assertEqual(
            [step["command"] for step in manifest["build"]],
            [["./scripts/build.sh"], ["./scripts/install-cli.sh"]],
        )


if __name__ == "__main__":
    unittest.main()
