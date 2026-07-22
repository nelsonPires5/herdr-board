from __future__ import annotations

import json
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
HELPER = REPO_ROOT / "scripts" / "stage_claude_config.py"
REAL_CLAUDE_SMOKE = REPO_ROOT / "e2e" / "real-claude-haiku-smoke.sh"


class StageClaudeConfigTests(unittest.TestCase):
    """Provider-free contract tests for the real-Claude config staging boundary."""

    def _fixture(self, root: Path) -> tuple[Path, Path, Path, bytes, bytes, str]:
        source = root / "real-claude"
        workspace = root / "disposable-workspace"
        staged = root / "staged-claude"
        source.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        staged.mkdir(mode=0o700)

        credentials = b'{"oauthAccount": {"emailAddress": "fake@example.test"}}\n'
        remote_settings = b'{"approved": ["fake-remote-setting"], "version": 7}\n'
        (source / ".credentials.json").write_bytes(credentials)
        (source / "remote-settings.json").write_bytes(remote_settings)
        # Deliberately make the source modes too permissive: staging must set
        # the destination mode rather than inheriting the source mode.
        (source / ".credentials.json").chmod(0o644)
        (source / "remote-settings.json").chmod(0o644)

        hook = source / "hooks" / "herdr-agent-state.sh"
        hook.parent.mkdir()
        hook.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        hook.chmod(0o755)
        herdr_hook = {
            "type": "command",
            "command": str(hook),
        }
        unrelated_hook = {"type": "command", "command": "/tmp/not-herdr.sh"}
        settings = {
            "theme": "light",
            "skipDangerousModePermissionPrompt": True,
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": "startup",
                        "hooks": [herdr_hook, unrelated_hook],
                    }
                ]
            },
            "permissions": {"allow": ["Bash(*)"]},
            "env": {"SHOULD_NOT_BE_STAGED": "yes"},
            "someUnrelatedSetting": {"nested": True},
        }
        (source / "settings.json").write_text(
            json.dumps(settings) + "\n", encoding="utf-8"
        )
        # A source global state file must not be copied or merged into the
        # disposable staged config.
        (source / ".claude.json").write_text(
            json.dumps({"projects": {"/unrelated/source/path": {"trusted": True}}}),
            encoding="utf-8",
        )
        return source, workspace, staged, credentials, remote_settings, str(hook)

    def _run_helper(
        self, source: Path, workspace: Path, staged: Path
    ) -> subprocess.CompletedProcess[str]:
        # This is the deliberately provider-free CLI contract for the helper.
        return subprocess.run(
            [
                sys.executable,
                str(HELPER),
                "--source-config-dir",
                str(source),
                "--workspace-dir",
                str(workspace),
                "--staged-config-dir",
                str(staged),
            ],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def _assert_mode(self, path: Path, mode: int = 0o600) -> None:
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), mode, path)

    def test_stages_minimal_config_and_exact_workspace_trust(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage-claude-test-") as tmp:
            source, workspace, staged, credentials, remote_settings, hook = self._fixture(
                Path(tmp)
            )
            # Exercise canonicalization rather than handing the helper an
            # already-normalized path.
            workspace_alias = workspace.parent / ".." / workspace.parent.name / workspace.name
            result = self._run_helper(source, workspace_alias, staged)
            self.assertEqual(result.returncode, 0, result.stderr)

            self.assertEqual((staged / ".credentials.json").read_bytes(), credentials)
            self.assertEqual(
                (staged / "remote-settings.json").read_bytes(), remote_settings
            )
            self._assert_mode(staged / ".credentials.json")
            self._assert_mode(staged / "remote-settings.json")

            settings = json.loads((staged / "settings.json").read_text(encoding="utf-8"))
            self.assertEqual(
                set(settings), {"hooks", "skipDangerousModePermissionPrompt", "theme"}
            )
            self.assertTrue(settings["skipDangerousModePermissionPrompt"])
            self.assertEqual(settings["theme"], "dark")
            session_start = settings["hooks"]["SessionStart"]
            self.assertEqual(
                [hook_entry for group in session_start for hook_entry in group["hooks"]],
                [{"type": "command", "command": hook}],
            )
            self.assertEqual(len(session_start), 1)
            self._assert_mode(staged / "settings.json")

            claude = json.loads((staged / ".claude.json").read_text(encoding="utf-8"))
            self.assertEqual(
                claude,
                {
                    "hasCompletedOnboarding": True,
                    "projects": {
                        str(workspace.resolve()): {
                            "hasTrustDialogAccepted": True,
                            "hasClaudeMdExternalIncludesApproved": False,
                        }
                    },
                },
            )
            self._assert_mode(staged / ".claude.json")
            self.assertEqual(
                {path.name for path in staged.iterdir()},
                {
                    ".credentials.json",
                    "remote-settings.json",
                    "settings.json",
                    ".claude.json",
                },
            )

    def test_rejects_missing_or_unsafe_prerequisites(self) -> None:
        cases = (
            (
                "credentials",
                lambda source, workspace: (source / ".credentials.json").unlink(),
            ),
            (
                "remote-settings",
                lambda source, workspace: (source / "remote-settings.json").unlink(),
            ),
            ("settings", lambda source, workspace: (source / "settings.json").unlink()),
            ("workspace", lambda source, workspace: workspace.rmdir()),
        )
        for label, mutate in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory(
                prefix="stage-claude-test-"
            ) as tmp:
                source, workspace, staged, *_ = self._fixture(Path(tmp))
                mutate(source, workspace)
                result = self._run_helper(source, workspace, staged)
                self.assertNotEqual(result.returncode, 0, result.stdout)

        unsafe_settings = (
            {"type": "command", "command": "hooks/herdr-agent-state.sh"},
            {
                "type": "command",
                "command": str(Path("/tmp") / "herdr-agent-state.sh"),
            },
        )
        for hook in unsafe_settings:
            with self.subTest(hook=hook), tempfile.TemporaryDirectory(
                prefix="stage-claude-test-"
            ) as tmp:
                source, workspace, staged, *_ = self._fixture(Path(tmp))
                settings = json.loads(
                    (source / "settings.json").read_text(encoding="utf-8")
                )
                settings["hooks"]["SessionStart"][0]["hooks"] = [hook]
                (source / "settings.json").write_text(
                    json.dumps(settings), encoding="utf-8"
                )
                result = self._run_helper(source, workspace, staged)
                self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_rejects_existing_out_of_tree_or_symlinked_herdr_hooks(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage-claude-test-") as tmp:
            root = Path(tmp)
            source, workspace, staged, *_ = self._fixture(root)
            outside = root / "outside" / "herdr-agent-state.sh"
            outside.parent.mkdir()
            outside.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            settings = json.loads((source / "settings.json").read_text(encoding="utf-8"))
            settings["hooks"]["SessionStart"][0]["hooks"] = [
                {"type": "command", "command": str(outside)}
            ]
            (source / "settings.json").write_text(json.dumps(settings), encoding="utf-8")
            result = self._run_helper(source, workspace, staged)
            self.assertNotEqual(result.returncode, 0, result.stdout)

        with tempfile.TemporaryDirectory(prefix="stage-claude-test-") as tmp:
            root = Path(tmp)
            source, workspace, staged, *_ = self._fixture(root)
            hook = source / "hooks" / "herdr-agent-state.sh"
            hook.unlink()
            outside = root / "outside" / "herdr-agent-state.sh"
            outside.parent.mkdir()
            outside.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            hook.symlink_to(outside)
            result = self._run_helper(source, workspace, staged)
            self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_accepts_installed_hook_with_shell_arguments(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage-claude-test-") as tmp:
            source, workspace, staged, *_ = self._fixture(Path(tmp))
            hook = source / "hooks" / "herdr-agent-state.sh"
            settings = json.loads((source / "settings.json").read_text(encoding="utf-8"))
            settings["hooks"]["SessionStart"][0]["hooks"] = [
                {"type": "command", "command": f"bash '{hook}' session"}
            ]
            (source / "settings.json").write_text(json.dumps(settings), encoding="utf-8")
            result = self._run_helper(source, workspace, staged)
            self.assertEqual(result.returncode, 0, result.stderr)
            staged_settings = json.loads(
                (staged / "settings.json").read_text(encoding="utf-8")
            )
            self.assertEqual(
                staged_settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
                f"bash '{hook}' session",
            )

    def test_smoke_state_file_is_private_temp_child(self) -> None:
        source = REAL_CLAUDE_SMOKE.read_text(encoding="utf-8")
        self.assertNotRegex(source, r'(?m)^STATE="/tmp/')
        tmp_assignment = source.index('TMP="$(mktemp -d')
        state_assignment = source.index('STATE="$TMP/')
        first_write_state_use = source.index("\nwrite_state\n", source.index("write_state()"))
        self.assertGreater(state_assignment, tmp_assignment)
        self.assertLess(state_assignment, first_write_state_use)
        self.assertRegex(source, r'(?m)^STATE="\$TMP/[^"/]+"$')

    def test_smoke_uses_stager_and_hashes_remote_settings_before_and_after(self) -> None:
        source = REAL_CLAUDE_SMOKE.read_text(encoding="utf-8")
        self.assertRegex(
            source,
            r"python3\s+[^\n]*scripts/stage_claude_config\.py",
            "smoke must invoke the provider-free staging helper",
        )

        hash_start = source.index("hash_real_files()")
        hash_end = source.index("\n}", hash_start)
        hash_block = source[hash_start:hash_end]
        self.assertIn("REAL_REMOTE_SETTINGS", hash_block)
        self.assertRegex(
            source,
            r"REAL_REMOTE_SETTINGS=.{0,120}remote-settings\.json",
            "smoke must identify the real remote-settings file",
        )
        self.assertRegex(
            source,
            r'REAL_HASHES_BEFORE="\$\(hash_real_files\)"',
            "the real remote-settings file must be included in the before hash",
        )
        self.assertRegex(
            source,
            r'hashes_after="\$\(hash_real_files',
            "the real remote-settings file must be included in the after hash",
        )


if __name__ == "__main__":
    unittest.main()
