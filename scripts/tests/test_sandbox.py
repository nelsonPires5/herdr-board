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


def _stub_provider(home: str, prov: str) -> None:
    """Create the minimum credential + Herdr-integration files a provider
    pre-check requires (stubs; never real credentials)."""
    root = Path(home)
    if prov == "pi":
        d = root / ".pi" / "agent"
        (d / "extensions").mkdir(parents=True)
        for f in ("auth.json", "settings.json", "extensions/herdr-agent-state.ts"):
            (d / f).write_text("stub\n", encoding="utf-8")
    elif prov == "codex":
        d = root / ".codex"
        d.mkdir()
        for f in ("auth.json", "config.toml", "herdr-agent-state.sh"):
            (d / f).write_text("stub\n", encoding="utf-8")
    elif prov == "antigravity":
        d = root / ".gemini"
        (d / "antigravity-cli").mkdir(parents=True)
        (d / "config" / "hooks").mkdir(parents=True)
        for f in ("config/config.json", "antigravity-cli/antigravity-oauth-token",
                  "oauth_creds.json", "google_accounts.json", "state.json",
                  "installation_id", "antigravity-cli/jetski_state.pbtxt",
                  "config/hooks/herdr-agent-state.sh"):
            p = d / f
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text("stub\n", encoding="utf-8")
    else:
        raise AssertionError(prov)


class ArgumentHandlingTests(unittest.TestCase):
    def test_help_exits_zero_and_lists_subcommands(self) -> None:
        proc = sandbox("--help")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        for sub in (
            "gates", "prepare", "selfcheck", "shell", "board", "tui",
            "agent", "artifacts", "lock", "down", "reset", "doctor",
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

    def test_agent_requires_provider(self) -> None:
        proc = sandbox("agent", "--allow-network")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("--provider", proc.stderr)

    def test_agent_requires_explicit_network_opt_in(self) -> None:
        proc = sandbox("agent", "--provider", "codex")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("--allow-network", proc.stderr)

    def test_agent_accepts_pi_and_antigravity(self) -> None:
        # pi and antigravity are now first-class agent providers (the old
        # host-only smoke refusals are gone): with credentials present they
        # must reach the docker phase instead of being refused up front.
        for prov in ("pi", "codex", "antigravity"):
            with tempfile.TemporaryDirectory() as home:
                _stub_provider(home, prov)
                env_extra = {"HOME": home}
                if prov == "codex":
                    env_extra["CODEX_HOME"] = str(Path(home) / ".codex")
                proc = sandbox("--dry-run", "agent", "--provider", prov,
                               "--allow-network", env_extra=env_extra)
            self.assertEqual(proc.returncode, 0, f"{prov}: {proc.stderr}")
            self.assertIn("docker run -d", proc.stdout, prov)

    def test_agent_rejects_unknown_provider(self) -> None:
        proc = sandbox("agent", "--provider", "gpt", "--allow-network")
        self.assertEqual(proc.returncode, 2)

    def test_agent_rejects_provider_with_tui_seed(self) -> None:
        # --seed seeds one card per harness (all three); a lone --provider could
        # never scope it, so the wrapper must reject the combination instead of
        # mounting one provider while seeding cards for three.
        proc = sandbox("--dry-run", "agent", "--provider", "pi",
                       "--allow-network", "--tui", "--seed")
        self.assertEqual(proc.returncode, 2, proc.stderr)
        self.assertIn("--tui --seed seeds all three", proc.stderr)

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
        # checks without ever reaching docker, for every agent provider.
        for prov in ("pi", "codex", "antigravity"):
            with tempfile.TemporaryDirectory() as home:
                env_extra = {"HOME": home}
                if prov == "codex":
                    env_extra["CODEX_HOME"] = str(Path(home) / ".codex")
                proc = sandbox(
                    "agent", "--provider", prov, "--allow-network",
                    env_extra=env_extra,
                )
            self.assertEqual(proc.returncode, 2, prov)
            self.assertIn("missing", proc.stderr.lower())
            self.assertNotIn("docker run", proc.stdout)

    def test_missing_integration_hook_fails_before_launch(self) -> None:
        # A provider whose credential dir exists but whose Herdr integration
        # hook file is absent must be refused before any container launch.
        with tempfile.TemporaryDirectory() as home:
            d = Path(home) / ".codex"
            d.mkdir()
            for f in ("auth.json", "config.toml"):
                (d / f).write_text("stub\n", encoding="utf-8")
            proc = sandbox(
                "agent", "--provider", "codex", "--allow-network",
                env_extra={"HOME": home, "CODEX_HOME": str(d)},
            )
        self.assertEqual(proc.returncode, 2)
        self.assertIn("herdr-agent-state.sh", proc.stderr)
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

    def test_agent_dry_run_enables_network_only_with_opt_in(self) -> None:
        # Without --allow-network there is no docker run at all (pre-checks
        # fail first); with it, the network is on (no --network none) and the
        # opted-in credential dir mounts read-only at /secrets.
        with tempfile.TemporaryDirectory() as home:
            _stub_provider(home, "codex")
            proc = sandbox(
                "--dry-run", "agent", "--provider", "codex", "--allow-network",
                env_extra={"HOME": home, "CODEX_HOME": str(Path(home) / ".codex")},
            )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        run_lines = [ln for ln in proc.stdout.splitlines() if "docker run -d" in ln]
        self.assertTrue(run_lines, "agent dry-run printed no agent container run")
        self.assertNotIn("--network none", " ".join(run_lines))
        self.assertIn(f"-v {Path(home) / '.codex'}:/secrets/codex:ro", proc.stdout)
        self.assertIn("-e AGY_BIN=", proc.stdout)

    def test_agent_dedicated_state_volume_and_no_host_state_leak(self) -> None:
        # The agent container runs on its own state volume (never the offline
        # environment container's), so it cannot collide with or inherit the
        # offline sandbox's herdr/board state.
        with tempfile.TemporaryDirectory() as home:
            _stub_provider(home, "pi")
            proc = sandbox(
                "--dry-run", "agent", "--provider", "pi", "--allow-network",
                env_extra={"HOME": home},
            )
        agent_start = " ".join(ln for ln in proc.stdout.splitlines() if "--name" in ln)
        self.assertRegex(agent_start, r"--name hb-sb-[a-z0-9-]+-agent ")
        mounts = re.findall(r"-v (\S+):/home/board", agent_start)
        self.assertEqual(len(mounts), 1, agent_start)
        self.assertTrue(mounts[0].endswith("-agent-state"), agent_start)
        self.assertNotIn("-env ", agent_start)


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

    def test_provider_cli_node_version_is_pinned(self) -> None:
        text = DOCKERFILE.read_text(encoding="utf-8")
        # pi 0.84.x requires Node >= 22.19.0; the image must carry a pinned
        # Node 22 LTS (the Debian bookworm node is 18 and too old), verified
        # by SHA-256 per architecture, with no floating tags anywhere.
        self.assertIn("NODE_VERSION=22.23.2", text)
        self.assertIn("node-v${NODE_VERSION}-linux-x64.tar.xz", text)
        self.assertIn("node-v${NODE_VERSION}-linux-arm64.tar.xz", text)
        self.assertIn("22.19.0", text)
        self.assertIn("d60acfe00a2932254bb0ad20e01b0d74397a0875595de719654b214f4b03f307", text)
        self.assertIn("fff4078c5def658577f92c88db7db3bc0072924bfb93fe52c1e744a54e94abb8", text)
        self.assertNotIn("nodejs npm", text)


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

    def test_host_secrets_mount_only_in_agent_path(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        agent_start = text.index("# Agent mode")
        for m in re.finditer(r"/secrets", text):
            self.assertGreater(m.start(), agent_start, "/secrets mount outside the agent mode")

    def test_agent_provider_env_only_in_agent_path(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        agent_start = text.index("# Agent mode")
        for marker in ("AGY_BIN=", "agent-entrypoint.sh", "agent-run.sh",
                       "agent-prepare.sh", "agent_base_flags()"):
            self.assertGreater(text.index(marker), agent_start, marker)

    def test_docker_dir_scripts_exist_and_are_bash(self) -> None:
        for name in ("lib.sh", "selfcheck.sh", "gates.sh", "prepare.sh",
                     "env-entrypoint.sh", "agent-prepare.sh", "agent-entrypoint.sh",
                     "agent-run.sh", "lock.sh"):
            path = DOCKER_DIR / name
            self.assertTrue(path.is_file(), name)
            self.assertTrue(path.stat().st_mode & 0o111, f"{name} not executable")
            self.assertTrue(path.read_text(encoding="utf-8").startswith("#!"), name)

    def test_agy_tarball_pins_exist_for_both_arches(self) -> None:
        pins = (DOCKER_DIR / "agy-pin.txt").read_text(encoding="utf-8")
        arm = "6189cf6291625a56c510e80f57489531721bca152ced838e6925725e5ddd9d3d1bfd74b2c379f328d4a2b68a91c383f865d7a0433f707ba8b75ac0fcd96aea00"
        amd = "481f590b102ca6847ef13b865f08d457048a1f3f01851ed2a3818eb09a53264b107ca5e442a8677248d9790fd96eccf4918a2aed82d866b23d294422ba42f67e"
        self.assertIn("arm64", pins)
        self.assertIn("amd64", pins)
        self.assertIn(arm, pins)
        self.assertIn(amd, pins)
        # every pin line spells version/url/sha512 and uses no floating version
        for line in pins.splitlines():
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            for tok in ("version=", "url=", "sha512="):
                self.assertIn(tok, line)
            self.assertRegex(line, r"sha512=[0-9a-f]{128}")

    def test_agent_prepare_pins_clis_and_verifies_checksums(self) -> None:
        text = (DOCKER_DIR / "agent-prepare.sh").read_text(encoding="utf-8")
        # exact pinned npm versions, no floating tags
        self.assertIn("@earendil-works/pi-coding-agent@0.84.2", text)
        self.assertIn("@openai/codex@0.147.0", text)
        self.assertNotIn("npm install --prefix \"$npm_prefix\" \"$npm_pkg\"", text)
        # agy: pinned file download + sha512 verification, never install.sh
        self.assertIn("agy-pin.txt", text)
        self.assertIn("sha512sum --check", text)
        self.assertNotIn("antigravity.google/cli/install.sh", text)
        self.assertIn("/artifacts", text)  # evidence written, never credentials
        self.assertNotIn("/secrets", text)  # prepare never sees host secrets
        # codex needs a WRITABLE config.toml (it persists the directory-trust
        # answer into it): a checked-in minimal container config is installed,
        # never the host's macOS-specific one.
        self.assertIn("agent-codex-config.toml", text)

    def test_agent_codex_config_is_minimal_and_leaks_no_host_paths(self) -> None:
        cfg = (DOCKER_DIR / "agent-codex-config.toml").read_text(encoding="utf-8")
        # pre-trusts the container workspace dirs so a headless run never
        # blocks on the codex "trust this directory?" prompt
        for trust in ('[projects."/"]', '[projects."/home/board"]',
                      '[projects."/home/board/work"]'):
            self.assertIn(trust, cfg)
        self.assertIn('trust_level = "trusted"', cfg)
        # no host/macOS leakage: no Homebrew, no /Applications, no user paths
        self.assertNotIn("/Users/", cfg)
        self.assertNotIn("/Applications", cfg)
        self.assertNotIn("/opt/homebrew", cfg)
        # credentials are never part of this config
        self.assertNotIn("auth.json", cfg)

    def test_agent_agy_settings_skips_onboarding_and_leaks_no_host_paths(self) -> None:
        cfg = (DOCKER_DIR / "agent-agy-settings.json").read_text(encoding="utf-8")
        # agy opens a first-run color-scheme wizard unless the CLI settings
        # exist; the seeded checked-in file pre-selects the UI and auto-proceeds
        # tools so a headless dispatch never blocks on an interactive prompt.
        self.assertIn('"colorScheme": "dark"', cfg)
        self.assertIn('"toolPermission": "always-proceed"', cfg)
        for trust in ('"/home/board"', '"/home/board/work"'):
            self.assertIn(trust, cfg)
        # no host/macOS leakage, no credentials
        self.assertNotIn("/Users/", cfg)
        self.assertNotIn("/Applications", cfg)
        self.assertNotIn("/opt/homebrew", cfg)
        self.assertNotIn("auth.json", cfg)

    def test_agent_entrypoint_wires_creds_ro_and_fails_closed(self) -> None:
        text = (DOCKER_DIR / "agent-entrypoint.sh").read_text(encoding="utf-8")
        # credentials are read-only symlinks from /secrets (never copied)
        self.assertIn("ln -sfn", text)
        self.assertIn('/secrets/codex', text)
        self.assertIn("fatal", text)
        self.assertIn("herdr integration status", text)
        self.assertIn("gemini-3.7-flash", text)
        self.assertIn("exec herdr server", text)
        # surgical wiring: codex config.toml is NOT symlinked from the ro
        # source (it must be writable); pi links only the herdr extension;
        # the host moshi voice extension is never wired.
        self.assertNotIn('"config.toml"', text)
        self.assertIn("herdr-agent-state.ts", text)
        self.assertNotIn("moshi", text)
        self.assertIn("onboarding.json", text)  # agy color-scheme wizard skipped
        self.assertIn("agent-agy-settings.json", text)
        self.assertIn("jetski_state.pbtxt", text)  # host install identity so the
        # account-eligibility gate never re-triggers in a headless run
        text_no_comment = "\n".join(l for l in text.splitlines()
                                     if not l.lstrip().startswith("#"))
        # the pi extension list is restricted to the herdr integration
        self.assertIn("extensions/herdr-agent-state.ts", text_no_comment)

    def test_agent_run_uses_auto_column_and_bounded_watchdog(self) -> None:
        text = (DOCKER_DIR / "agent-run.sh").read_text(encoding="utf-8")
        self.assertIn('--trigger "$trigger"', text)
        self.assertIn("board done --outcome ok", text)
        self.assertIn("seq 1 2400", text)  # 20 min watchdog at 0.5s
        self.assertIn("new-workspace", text)
        self.assertIn("/artifacts", text)
        # headless approval presets per harness (codex/antigravity block on
        # interactive prompts otherwise)
        self.assertIn('codex) perm="approve-for-me"', text)
        self.assertIn('antigravity) perm="sandbox"', text)
        self.assertIn('--permission "$perm"', text)

    def test_agent_run_ensure_column_fails_closed_on_trigger_mismatch(self) -> None:
        # Regressions from final review P1: a stale volume must never silently
        # auto-dispatch seeded cards (an auto 'Todo') or park one-shots (a
        # manual 'Running'). ensure_column must verify the trigger of an
        # existing same-named column and fail rather than reuse it.
        text = (DOCKER_DIR / "agent-run.sh").read_text(encoding="utf-8")
        self.assertIn("'.trigger'", text)
        self.assertIn('refusing to use a mismatched column', text)
        self.assertIn("already exists with trigger", text)

    def test_agent_entrypoint_creates_cache_and_clears_stale_socket(self) -> None:
        # Regressions from final review P1: a fresh agent-state volume must
        # have antigravity-cli/cache before the onboarding marker is written,
        # and a torn-down predecessor must not leave a stale socket that a
        # fresh herdr server cannot bind.
        text = (DOCKER_DIR / "agent-entrypoint.sh").read_text(encoding="utf-8")
        self.assertIn('"$base/antigravity-cli/cache"', text)
        self.assertIn("rm -f /home/board/.config/herdr/herdr.sock", text)

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
