from __future__ import annotations

import importlib.util
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

SCRIPT_PATH = Path(__file__).resolve().parents[1] / "prepare-release.py"
SPEC = importlib.util.spec_from_file_location("prepare_release", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import plumbing.
    raise RuntimeError(f"cannot load {SCRIPT_PATH}")
prepare_release = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = prepare_release
SPEC.loader.exec_module(prepare_release)


class PrepareReleaseTests(unittest.TestCase):
    def write_fixture(
        self,
        repo_root: Path,
        *,
        cargo_version: str = "0.1.0",
        plugin_version: str = "0.1.0",
        lock_version: str = "0.1.0",
        unreleased_body: str = "- Release prep automation\n- Update workflows\n",
        target_link_version: str = "0.1.0",
    ) -> None:
        repo_root.mkdir(parents=True, exist_ok=True)
        (repo_root / "Cargo.toml").write_text(
            textwrap.dedent(
                f"""
                [workspace]
                resolver = "2"
                members = [
                    "crates/board-core",
                    "crates/board-herdr",
                    "crates/board-tui",
                    "crates/board-daemon",
                    "crates/board-cli",
                ]

                [workspace.package]
                version = "{cargo_version}"
                edition = "2021"
                license = "MIT"
                """
            ).lstrip(),
            encoding="utf-8",
        )
        (repo_root / "herdr-plugin.toml").write_text(
            textwrap.dedent(
                f"""
                id = "herdr-board"
                name = "Herdr Board"
                version = "{plugin_version}"
                description = "Kanban board for AI coding agents."
                min_herdr_version = "0.7.0"
                platforms = ["linux"]
                """
            ).lstrip(),
            encoding="utf-8",
        )
        (repo_root / "Cargo.lock").write_text(
            textwrap.dedent(
                f"""
                version = 4

                [[package]]
                name = "board-cli"
                version = "{lock_version}"
                dependencies = []

                [[package]]
                name = "board-core"
                version = "{lock_version}"
                dependencies = []

                [[package]]
                name = "board-daemon"
                version = "{lock_version}"
                dependencies = []

                [[package]]
                name = "board-herdr"
                version = "{lock_version}"
                dependencies = []

                [[package]]
                name = "board-tui"
                version = "{lock_version}"
                dependencies = []

                [[package]]
                name = "serde"
                version = "1.0.0"
                source = "registry+https://github.com/rust-lang/crates.io-index"
                checksum = "deadbeef"
                """
            ).lstrip(),
            encoding="utf-8",
        )
        body_block = unreleased_body.rstrip("\n")
        changelog_parts = [
            "# Changelog",
            "",
            "All notable changes to this project are documented here.",
            "",
            "## [Unreleased]",
            "",
        ]
        if body_block:
            changelog_parts.append(body_block)
            changelog_parts.append("")
        changelog_parts.extend(
            [
                "## [0.1.0] - 2026-07-15",
                "",
                "### Added",
                "- Initial release.",
                "",
                f"[Unreleased]: https://github.com/example/herdr-board/compare/v{target_link_version}...HEAD",
                "[0.1.0]: https://github.com/example/herdr-board/releases/tag/v0.1.0",
                "",
            ]
        )
        (repo_root / "CHANGELOG.md").write_text("\n".join(changelog_parts), encoding="utf-8")

    def test_bump_semver_patch_minor_major(self) -> None:
        self.assertEqual(prepare_release.bump_semver("0.1.0", "patch"), "0.1.1")
        self.assertEqual(prepare_release.bump_semver("0.1.0", "minor"), "0.2.0")
        self.assertEqual(prepare_release.bump_semver("1.2.3", "major"), "2.0.0")

    def test_workspace_version_changed_compares_first_parent(self) -> None:
        parent = '[workspace.package]\nversion = "0.1.0"\n'
        same_head = '[workspace.package]\nversion = "0.1.0"\n'
        bumped_head = '[workspace.package]\nversion = "0.2.0"\n'
        self.assertFalse(prepare_release.workspace_version_changed(same_head, parent))
        self.assertTrue(prepare_release.workspace_version_changed(bumped_head, parent))

    def test_apply_release_updates_synced_files_and_changelog(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.write_fixture(repo)

            result = prepare_release.apply_release(
                repo,
                "0.2.0",
                release_date="2026-07-16",
                repo_url="https://github.com/example/herdr-board",
            )

            self.assertTrue(result.changed)
            self.assertFalse(result.already_prepared)
            self.assertEqual(result.current, "0.1.0")
            self.assertEqual(result.target, "0.2.0")

            cargo = (repo / "Cargo.toml").read_text(encoding="utf-8")
            plugin = (repo / "herdr-plugin.toml").read_text(encoding="utf-8")
            lock = (repo / "Cargo.lock").read_text(encoding="utf-8")
            changelog = (repo / "CHANGELOG.md").read_text(encoding="utf-8")

            self.assertIn('version = "0.2.0"', cargo)
            self.assertIn('version = "0.2.0"', plugin)
            self.assertEqual(
                prepare_release.parse_lock_versions(lock),
                {
                    "board-cli": "0.2.0",
                    "board-core": "0.2.0",
                    "board-daemon": "0.2.0",
                    "board-herdr": "0.2.0",
                    "board-tui": "0.2.0",
                },
            )
            self.assertIn("## [0.2.0] - 2026-07-16", changelog)
            self.assertIn("- Release prep automation", changelog)
            self.assertIn("- Update workflows", changelog)
            self.assertIn(
                "[Unreleased]: https://github.com/example/herdr-board/compare/v0.2.0...HEAD",
                changelog,
            )
            self.assertIn(
                "[0.2.0]: https://github.com/example/herdr-board/releases/tag/v0.2.0",
                changelog,
            )
            self.assertNotIn("[0.1.0] - 2026-07-16", changelog)

    def test_apply_release_is_idempotent_on_rerun(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.write_fixture(repo)

            first = prepare_release.apply_release(
                repo,
                "0.2.0",
                release_date="2026-07-16",
                repo_url="https://github.com/example/herdr-board",
            )
            self.assertTrue(first.changed)

            snapshot = {
                name: (repo / name).read_text(encoding="utf-8")
                for name in ("Cargo.toml", "herdr-plugin.toml", "Cargo.lock", "CHANGELOG.md")
            }

            second = prepare_release.apply_release(
                repo,
                "0.2.0",
                release_date="2026-07-16",
                repo_url="https://github.com/example/herdr-board",
            )
            self.assertFalse(second.changed)
            self.assertTrue(second.already_prepared)

            after = {
                name: (repo / name).read_text(encoding="utf-8")
                for name in ("Cargo.toml", "herdr-plugin.toml", "Cargo.lock", "CHANGELOG.md")
            }
            self.assertEqual(after, snapshot)

    def test_verify_command_validates_prepared_release(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.write_fixture(repo)
            prepare_release.apply_release(
                repo,
                "0.2.0",
                release_date="2026-07-16",
                repo_url="https://github.com/example/herdr-board",
            )

            self.assertEqual(
                prepare_release.verify_release(
                    repo, repo_url="https://github.com/example/herdr-board"
                ),
                "0.2.0",
            )
            self.assertEqual(
                prepare_release.main(
                    [
                        "--repo",
                        str(repo),
                        "verify",
                        "--repo-url",
                        "https://github.com/example/herdr-board",
                    ]
                ),
                0,
            )

    def test_verify_rejects_each_version_source_and_changelog(self) -> None:
        cases = (
            ("herdr-plugin.toml", 'version = "9.9.9"', "release version mismatch"),
            ("Cargo.lock", 'name = "board-tui"\nversion = "9.9.9"', "release version mismatch"),
            ("CHANGELOG.md", "## [0.1.0] - 2026-07-15", "not prepared"),
        )
        for filename, needle, message in cases:
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as tmp:
                repo = Path(tmp)
                self.write_fixture(repo)
                prepare_release.apply_release(
                    repo,
                    "0.2.0",
                    release_date="2026-07-16",
                    repo_url="https://github.com/example/herdr-board",
                )
                path = repo / filename
                text = path.read_text(encoding="utf-8")
                if filename == "Cargo.lock":
                    text = text.replace('name = "board-tui"\nversion = "0.2.0"', needle)
                elif filename == "CHANGELOG.md":
                    text = text.replace("## [0.2.0] - 2026-07-16", needle)
                else:
                    text = text.replace('version = "0.2.0"', 'version = "9.9.9"')
                path.write_text(text, encoding="utf-8")
                with self.assertRaisesRegex(prepare_release.ReleaseError, message):
                    prepare_release.verify_release(
                        repo, repo_url="https://github.com/example/herdr-board"
                    )

    def test_verify_rejects_missing_one_of_five_local_lock_packages(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.write_fixture(repo)
            lock = (repo / "Cargo.lock").read_text(encoding="utf-8")
            start = lock.index('[[package]]\nname = "board-tui"')
            end = lock.index("[[package]]", start + 1)
            (repo / "Cargo.lock").write_text(lock[:start] + lock[end:], encoding="utf-8")
            with self.assertRaisesRegex(prepare_release.ReleaseError, "missing local package entries"):
                prepare_release.verify_release(
                    repo, repo_url="https://github.com/example/herdr-board"
                )

    def test_apply_recovers_from_every_atomic_write_partial_state(self) -> None:
        original_files = ("Cargo.toml", "herdr-plugin.toml", "Cargo.lock", "CHANGELOG.md")
        for failed_at in range(len(original_files)):
            with self.subTest(failed_at=failed_at), tempfile.TemporaryDirectory() as tmp:
                repo = Path(tmp)
                self.write_fixture(repo)
                real_write = prepare_release.write_text
                calls = 0

                def fail_once(path: Path, content: str) -> None:
                    nonlocal calls
                    if calls == failed_at:
                        calls += 1
                        raise OSError(f"simulated write failure at {path.name}")
                    calls += 1
                    real_write(path, content)

                with mock.patch.object(prepare_release, "write_text", side_effect=fail_once):
                    with self.assertRaises(OSError):
                        prepare_release.apply_release(
                            repo,
                            "0.2.0",
                            release_date="2026-07-16",
                            repo_url="https://github.com/example/herdr-board",
                        )

                first_calls = calls
                self.assertEqual(first_calls, failed_at + 1)
                result = prepare_release.apply_release(
                    repo,
                    "0.2.0",
                    release_date="2026-07-16",
                    repo_url="https://github.com/example/herdr-board",
                )
                self.assertTrue(result.changed)
                self.assertEqual(
                    prepare_release.verify_release(
                        repo, repo_url="https://github.com/example/herdr-board"
                    ),
                    "0.2.0",
                )

    def test_atomic_write_replaces_file_without_leftover_temp_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "file.txt"
            path.write_text("old", encoding="utf-8")
            path.chmod(0o640)
            prepare_release.atomic_write(path, "new")
            self.assertEqual(path.read_text(encoding="utf-8"), "new")
            self.assertEqual(path.stat().st_mode & 0o777, 0o640)
            self.assertEqual(list(Path(tmp).iterdir()), [path])

    def test_apply_release_errors_on_mismatch_and_empty_unreleased(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.write_fixture(repo, plugin_version="9.9.9")

            with self.assertRaisesRegex(prepare_release.ReleaseError, "version mismatch"):
                prepare_release.apply_release(
                    repo,
                    "0.2.0",
                    release_date="2026-07-16",
                    repo_url="https://github.com/example/herdr-board",
                )

        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.write_fixture(repo, unreleased_body="")

            with self.assertRaisesRegex(prepare_release.ReleaseError, r"\[Unreleased\] is empty"):
                prepare_release.apply_release(
                    repo,
                    "0.2.0",
                    release_date="2026-07-16",
                    repo_url="https://github.com/example/herdr-board",
                )


if __name__ == "__main__":  # pragma: no cover - convenience.
    unittest.main()
