"""Contract tests for the Docker sandbox wrapper (`scripts/sandbox.sh` and
`docker/`).

Everything here is daemon-free: the wrapper is exercised only through
`--help`, argument-validation failures, and `--dry-run` (which must print the
planned docker commands without a docker daemon), plus static pinning and
isolation assertions over the checked-in files. The full in-container behavior
is validated by `scripts/sandbox.sh gates` (documented in docs/sandbox.md).
"""
from __future__ import annotations

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

from _support import REPO_ROOT, clean_env, run_bash

WRAPPER = REPO_ROOT / "scripts" / "sandbox.sh"
DOCKER_DIR = REPO_ROOT / "docker"
DOCKERFILE = DOCKER_DIR / "Dockerfile"

HERDR_X86_64_SHA = "b872ea7e40fa2cb17e857ac9b62b1bf26db7b403c622f5d2f3f5b35f6e9acd28"
HERDR_AARCH64_SHA = "f647ac66468d9efbc642fe534fb284468f0aea60641606fc008dfc0d82a3ca87"
RUST_VERSION = "1.97.0"


def sandbox(
    *args: str, env_extra: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Run the wrapper with host BOARD_*/HERDR_* variables dropped."""
    drop = [k for k in os.environ if k.startswith(("BOARD_", "HERDR_"))]
    env = clean_env(drop=drop)
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        ["bash", str(WRAPPER), *args],
        cwd=str(REPO_ROOT),
        env=env,
        text=True,
        capture_output=True,
        timeout=30,
    )


class ArgumentHandlingTests(unittest.TestCase):
    def test_help_exits_zero_and_lists_subcommands(self) -> None:
        proc = sandbox("--help")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        for sub in (
            "gates", "prepare", "selfcheck", "shell", "board", "tui",
            "smoke", "artifacts", "lock", "down", "reset", "doctor",
        ):
            self.assertIn(sub, proc.stdout)

    def test_no_subcommand_fails(self) -> None:
        proc = sandbox()
        self.assertEqual(proc.returncode, 2)

    def test_unknown_subcommand_fails(self) -> None:
        proc = sandbox("definitely-not-a-subcommand")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("unknown subcommand", proc.stderr)

    def test_unknown_flag_fails(self) -> None:
        proc = sandbox("--nope", "gates")
        self.assertEqual(proc.returncode, 2)

    def test_smoke_requires_provider(self) -> None:
        proc = sandbox("smoke", "--allow-network")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("--provider", proc.stderr)

    def test_smoke_requires_explicit_network_opt_in(self) -> None:
        proc = sandbox("smoke", "--provider", "codex")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("--allow-network", proc.stderr)

    def test_smoke_refuses_pi_before_any_launch(self) -> None:
        proc = sandbox("smoke", "--provider", "pi", "--allow-network")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("WezTerm", proc.stderr)

    def test_smoke_refuses_antigravity_before_any_launch(self) -> None:
        proc = sandbox("smoke", "--provider", "antigravity", "--allow-network")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("Antigravity", proc.stderr)

    def test_smoke_rejects_unknown_provider(self) -> None:
        proc = sandbox("smoke", "--provider", "gpt", "--allow-network")
        self.assertEqual(proc.returncode, 2)

    def test_board_requires_arguments(self) -> None:
        proc = sandbox("--dry-run", "board")
        self.assertEqual(proc.returncode, 2)

    def test_reset_requires_scope(self) -> None:
        proc = sandbox("reset")
        self.assertEqual(proc.returncode, 2)

    def test_reset_rejects_unknown_scope(self) -> None:
        proc = sandbox("--dry-run", "reset", "--everything")
        self.assertEqual(proc.returncode, 2)

    def test_missing_credentials_fail_before_container_launch(self) -> None:
        # A HOME with no provider credentials at all must fail the pre-launch
        # checks without ever reaching docker.
        with tempfile.TemporaryDirectory() as home:
            proc = sandbox(
                "smoke", "--provider", "codex", "--allow-network",
                env_extra={"HOME": home, "CODEX_HOME": str(Path(home) / ".codex")},
            )
        self.assertEqual(proc.returncode, 2)
        self.assertIn("missing Codex config dir", proc.stderr)
        self.assertNotIn("docker run", proc.stdout)


class DryRunIsolationTests(unittest.TestCase):
    """--dry-run must compose the full isolation profile without a daemon."""

    def dry_gates(self) -> str:
        proc = sandbox("--dry-run", "gates")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(proc.stdout, "dry-run printed nothing")
        return proc.stdout

    def test_deterministic_mode_uses_network_none(self) -> None:
        out = self.dry_gates()
        self.assertIn("--network none", out)

    def test_deterministic_mode_is_non_root_dropped_caps(self) -> None:
        out = self.dry_gates()
        self.assertIn("--user 1000:1000", out)
        self.assertIn("--cap-drop ALL", out)
        self.assertIn("--security-opt no-new-privileges", out)

    def test_worktree_mounted_read_only(self) -> None:
        out = self.dry_gates()
        self.assertIn(f"-v {REPO_ROOT}:/repo:ro", out)

    def test_no_host_pid_or_host_network_or_privileged(self) -> None:
        out = self.dry_gates()
        self.assertNotIn("--pid host", out)
        self.assertNotIn("--network host", out)
        self.assertNotIn("--privileged", out)

    def test_mutable_state_in_named_volumes_outside_the_repo(self) -> None:
        out = self.dry_gates()
        for vol, mount in (
            ("-cargo", "/opt/cargo"),
            ("-target", "/repo/target"),
            ("-state", "/home/board"),
            ("-artifacts", "/artifacts"),
        ):
            self.assertRegex(out, rf"-v hb-sb-[a-z0-9-]+{vol}:{mount}")
        # No repo path may be writable in deterministic modes except the
        # build-output volume at /repo/target.
        ro_lines = [ln for ln in out.splitlines() if f"-v {REPO_ROOT}" in ln]
        self.assertNotEqual([], ro_lines)
        for ln in ro_lines:
            self.assertIn(":/repo:ro", ln)

    def test_no_board_herdr_env_passthrough(self) -> None:
        out = self.dry_gates()
        self.assertNotRegex(out, r"-e BOARD_[A-Z_]+=")
        self.assertNotRegex(out, r"-e HERDR_[A-Z_]+=")

    def test_tmpfs_tmp_is_short_private_and_executable(self) -> None:
        # Docker tmpfs mounts default to noexec; the workspace's own tests exec
        # probe scripts from tempdirs, so the sandbox must opt in explicitly.
        out = self.dry_gates()
        self.assertRegex(out, r"--tmpfs /tmp:rw,exec,nosuid,nodev,")

    def test_smoke_dry_run_enables_network_only_with_opt_in(self) -> None:
        # Without --allow-network there is no docker run at all (pre-checks
        # fail first); with it, the network is on and secrets mount read-only.
        with tempfile.TemporaryDirectory() as home:
            codex = Path(home) / ".codex"
            codex.mkdir()
            for f in ("auth.json", "config.toml", "herdr-agent-state.sh"):
                (codex / f).write_text("stub\n", encoding="utf-8")
            proc = sandbox(
                "--dry-run", "smoke", "--provider", "codex", "--allow-network",
                env_extra={"HOME": home, "CODEX_HOME": str(codex)},
            )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        run_lines = [ln for ln in proc.stdout.splitlines() if "docker run" in ln]
        self.assertTrue(run_lines, "smoke dry-run printed no docker run")
        self.assertNotIn("--network none", " ".join(run_lines))
        self.assertIn(f"-v {codex}:/secrets/codex:ro", proc.stdout)
        self.assertIn("-e E2E_REAL_CODEX=1", proc.stdout)


class PinningTests(unittest.TestCase):
    def test_base_image_is_digest_pinned(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        self.assertIsNotNone(re.search(r"^FROM \S+@sha256:[0-9a-f]{64}", text, re.M))

    def test_no_floating_tags_in_dockerfile(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        self.assertNotIn(":latest", text)

    def test_rust_toolchain_is_pinned(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        self.assertIn(f"rust:{RUST_VERSION}-slim-bookworm@sha256:", text)

    def test_herdr_assets_are_sha_pinned_for_both_architectures(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        self.assertIn(HERDR_X86_64_SHA, text)
        self.assertIn(HERDR_AARCH64_SHA, text)
        self.assertIn("herdr-linux-x86_64", text)
        self.assertIn("herdr-linux-aarch64", text)
        self.assertIn("HERDR_VERSION=0.8.0", text)
        self.assertIn("releases/download/v${HERDR_VERSION}/", text)

    def test_herdr_version_and_protocol_verified_at_build(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        self.assertIn('"herdr ${HERDR_VERSION}"', text)
        self.assertIn(".protocol == ${HERDR_PROTOCOL}", text)
        self.assertIn("HERDR_PROTOCOL=19", text)

    def test_unsupported_architecture_fails_the_build(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        self.assertRegex(text, r"unsupported architecture")

    def test_container_runs_as_non_root_user(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        self.assertIn("--uid 1000", text)
        self.assertIsNotNone(re.search(r"^USER board\s*$", text, re.M))


class CleanupTests(unittest.TestCase):
    def test_reset_only_touches_sandbox_prefixed_resources(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        for scope in ("--cargo", "--target", "--state", "--artifacts", "--all"):
            self.assertIn(f"{scope})", text)
        # Every docker volume rm references the wrapper's own variables.
        for m in re.finditer(r"docker volume rm ([^\n]+)", text):
            args = m.group(1)
            for tok in args.split():
                if tok == ";;":
                    continue
                self.assertRegex(tok, r'^"?\$VOL_')

    def test_artifacts_refuses_to_write_inside_the_repository(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        self.assertIn("refusing to write artifacts inside the repository", text)


class StaticSafetyTests(unittest.TestCase):
    def test_wrapper_never_mounts_docker_socket(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        self.assertNotIn("docker.sock", text)
        self.assertNotRegex(text, r"-v /var/run")

    def test_host_secrets_mount_only_in_smoke_path(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        smoke_start = text.index("cmd_smoke()")
        for m in re.finditer(r"/secrets", text):
            self.assertGreater(m.start(), smoke_start, "/secrets mount outside cmd_smoke")

    def test_provider_optin_env_only_in_smoke_path(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        smoke_start = text.index("cmd_smoke()")
        for marker in ("E2E_REAL_CLAUDE_HAIKU=1", "E2E_REAL_CODEX=1", "E2E_REAL_OPENCODE=1"):
            self.assertGreater(text.index(marker), smoke_start, marker)

    def test_docker_dir_scripts_exist_and_are_bash(self) -> None:
        for name in ("lib.sh", "selfcheck.sh", "gates.sh", "prepare.sh",
                     "env-entrypoint.sh", "smoke.sh", "lock.sh"):
            path = DOCKER_DIR / name
            self.assertTrue(path.is_file(), name)
            self.assertTrue(path.stat().st_mode & 0o111, f"{name} not executable")
            self.assertTrue(path.read_text(encoding="utf-8").startswith("#!"), name)

    def test_selfcheck_asserts_the_isolation_contract(self) -> None:
        text = (DOCKER_DIR / "selfcheck.sh").read_text(encoding="utf-8")
        for required in (
            "read-only", "non-root", "docker socket", "network",
            "unreachable", "hb_audit_mounts",
        ):
            self.assertIn(required, text)
        lib = (DOCKER_DIR / "lib.sh").read_text(encoding="utf-8")
        # The audit greps mountinfo for the docker socket (escaped for grep -E)
        # and allowlists exactly the sandbox mounts, including the build-output
        # volume mounted inside the otherwise read-only repo.
        self.assertIn("docker\\.sock", lib)
        self.assertIn("/var/run/docker", lib)
        self.assertIn("/repo/target", lib)

    def test_gates_runner_runs_every_maintained_gate(self) -> None:
        text = (DOCKER_DIR / "gates.sh").read_text(encoding="utf-8")
        for required in (
            "selfcheck.sh",
            "cargo fmt --all --check",
            "cargo clippy --workspace --all-targets --all-features -- -D warnings",
            "cargo test --workspace --all-features",
            "python3 -m unittest discover -s scripts/tests -p 'test_*.py'",
            "e2e/test-harness.sh",
            "run-all.sh --require-all",
            "E2E_FORCE_BUILD=1",
            "CARGO_NET_OFFLINE=true",
        ):
            self.assertIn(required, text)

    def test_gates_failure_names_the_failing_gate(self) -> None:
        text = (DOCKER_DIR / "gates.sh").read_text(encoding="utf-8")
        self.assertIn("stopping at the first failing gate", text)


if __name__ == "__main__":
    unittest.main()
