from __future__ import annotations

import hashlib
import os
import shutil
import stat
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "install-cli.sh"
MARKER_NAME = ".herdr-board-cli-managed"
MARKER_PREFIX = "herdr-board install-cli.sh managed board sha256:"


def marker_bytes(content: bytes) -> bytes:
    checksum = hashlib.sha256(content).hexdigest()
    return f"{MARKER_PREFIX}{checksum}\n".encode()


class InstallCliTests(unittest.TestCase):
    def make_fixture(self, root: Path, content: str = "#!/bin/sh\necho first\n") -> tuple[Path, Path]:
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
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["HOME"] = str(home)
        # The plugin is Linux-only and intentionally uses GNU no-target-directory
        # semantics. Let this test suite run on macOS when Homebrew coreutils is
        # available under its conventional g-prefixed command names.
        if os.uname().sysname == "Darwin":
            gnu_commands = {
                command: shutil.which(f"g{command}")
                for command in ("ln", "mv", "sha256sum")
            }
            if all(gnu_commands.values()):
                gnu_bin = home / ".test-gnu-bin"
                gnu_bin.mkdir(parents=True, exist_ok=True)
                for command, executable in gnu_commands.items():
                    command_path = gnu_bin / command
                    if not command_path.exists():
                        command_path.symlink_to(executable)
                env["PATH"] = f"{gnu_bin}{os.pathsep}{env['PATH']}"
        if path_prefix is not None:
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
            fake_sha256sum = fake_bin / "sha256sum"
            fake_sha256sum.write_text("#!/bin/sh\nprintf 'not-a-checksum  -\\n'\n", encoding="utf-8")
            fake_sha256sum.chmod(0o755)

            result = self.run_script(script, home=home, path_prefix=fake_bin)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("sha256sum returned an invalid checksum", result.stderr)
            install_dir = home / ".local" / "bin"
            self.assertFalse((install_dir / "board").exists())
            self.assertFalse((install_dir / MARKER_NAME).exists())


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
