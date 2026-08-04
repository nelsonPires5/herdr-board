from __future__ import annotations

import re
import unittest
from pathlib import Path

from _support import REPO_ROOT as ROOT


HERDR_VERSION = "0.8.0"
HERDR_PROTOCOL = 19
HERDR_SHA256 = "b872ea7e40fa2cb17e857ac9b62b1bf26db7b403c622f5d2f3f5b35f6e9acd28"
HERDR_URL = (
    "https://github.com/herdrdev/herdr/releases/download/"
    f"v{HERDR_VERSION}/herdr-linux-x86_64"
)


class LiveE2ECIContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        cls.wrapper_path = ROOT / "e2e/ci.sh"
        cls.wrapper = (
            cls.wrapper_path.read_text(encoding="utf-8")
            if cls.wrapper_path.exists()
            else ""
        )
        cls.lib = (ROOT / "e2e/lib.sh").read_text(encoding="utf-8")
        cls.real_claude = (ROOT / "e2e/real-claude-haiku-smoke.sh").read_text(
            encoding="utf-8"
        )
        cls.real_pi = (ROOT / "e2e/real-pi-smoke.sh").read_text(encoding="utf-8")

    def test_workflow_has_one_bounded_read_only_live_job_after_fast_gates(self) -> None:
        self.assertEqual(len(re.findall(r"^  live-e2e:\s*$", self.workflow, re.M)), 1)
        job = self.workflow.split("  live-e2e:", 1)[1]
        self.assertIn("needs: [fmt, clippy, docs, scripts, e2e-safety, test]", job)
        self.assertIn("runs-on: ubuntu-latest", job)
        self.assertRegex(job, r"timeout-minutes:\s*[1-9][0-9]*")
        self.assertIn("permissions:\n      contents: read", job)
        self.assertIn("persist-credentials: false", job)

    def test_workflow_uses_runner_context_only_inside_steps(self) -> None:
        job = self.workflow.split("  live-e2e:", 1)[1]
        job_preamble = job.split("    steps:", 1)[0]
        self.assertNotIn("${{ runner.", job_preamble)

    def test_workflow_runs_only_the_checked_in_wrapper_and_always_uploads(self) -> None:
        job = self.workflow.split("  live-e2e:", 1)[1]
        self.assertEqual(re.findall(r"^\s*run:\s*(.+)$", job, re.M), ["bash e2e/ci.sh"])
        self.assertIn("uses: actions/upload-artifact@v4", job)
        self.assertIn("if: always()", job)
        self.assertIn("path: e2e-artifacts", job)
        self.assertIn("retention-days: 30", job)
        self.assertIn("if-no-files-found: warn", job)

    def test_workflow_caches_exact_pinned_binary_without_credentials(self) -> None:
        job = self.workflow.split("  live-e2e:", 1)[1]
        self.assertIn("uses: actions/cache@v4", job)
        self.assertIn(f"Cache Herdr {HERDR_VERSION}", job)
        self.assertIn(f"herdr-board-{HERDR_VERSION}-linux-x86_64", job)
        self.assertIn("x86_64", job)
        self.assertIn(HERDR_SHA256, job)
        self.assertIn(
            f"key: herdr-${{{{ runner.os }}}}-x86_64-{HERDR_VERSION}-{HERDR_SHA256}",
            job,
        )
        for forbidden in (
            "HERDR_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "E2E_REAL_PI",
            "0.7.5",
            "protocol 17",
            "3dc83288073e4c2d3c679a30e7be97bcca9141c6fd17dbbb9219142e95c59253",
        ):
            self.assertNotIn(forbidden, job)

    def test_wrapper_pins_and_verifies_supply_chain_and_protocol(self) -> None:
        self.assertTrue(self.wrapper_path.is_file())
        self.assertIn("set -euo pipefail", self.wrapper)
        self.assertIn("umask 077", self.wrapper)
        self.assertIn(HERDR_URL, self.wrapper)
        self.assertIn(HERDR_SHA256, self.wrapper)
        self.assertIn('"$HERDR_BIN" --version', self.wrapper)
        self.assertIn(f"HERDR_VERSION={HERDR_VERSION}", self.wrapper)
        self.assertIn(f"HERDR_PROTOCOL={HERDR_PROTOCOL}", self.wrapper)
        self.assertIn("protocol", self.wrapper)
        self.assertIn('"$REPO_ROOT/e2e/run-all.sh" --require-all', self.wrapper)
        self.assertNotIn("v0.7.5", self.wrapper)
        self.assertNotIn("protocol 17", self.wrapper)
        self.assertNotIn(
            "3dc83288073e4c2d3c679a30e7be97bcca9141c6fd17dbbb9219142e95c59253",
            self.wrapper,
        )

    def test_plugin_manifest_pins_exact_minimum_herdr_version(self) -> None:
        manifest = (ROOT / "herdr-plugin.toml").read_text(encoding="utf-8")
        self.assertEqual(
            re.findall(r'(?m)^min_herdr_version = "([^"]+)"$', manifest),
            [HERDR_VERSION],
        )

    def test_wrapper_forces_one_fresh_release_build_before_scenarios(self) -> None:
        command = (
            'E2E_FORCE_BUILD=1 "$REPO_ROOT/e2e/run-all.sh" --require-all '
            '2>&1 | tee "$EXPORT_DIR/suite.log"'
        )
        self.assertEqual(self.wrapper.count(command), 1)

    def test_e2e_preflights_and_real_claude_pin_the_same_exact_contract(self) -> None:
        self.assertIn(f'[ "$version" = "herdr {HERDR_VERSION}" ]', self.lib)
        self.assertIn(f'[ "$reported_version" = "{HERDR_VERSION}" ]', self.lib)
        self.assertIn(f'[ "$protocol" = "{HERDR_PROTOCOL}" ]', self.lib)
        self.assertIn(
            f'[ "$HERDR_VERSION" = "herdr {HERDR_VERSION}" ]', self.real_claude
        )
        self.assertIn(f".protocol == {HERDR_PROTOCOL}", self.real_claude)
        self.assertIn(f"herdr_schema_protocol={HERDR_PROTOCOL}", self.real_claude)
        self.assertIn(
            f'.version == "{HERDR_VERSION}" and .protocol == {HERDR_PROTOCOL}',
            self.real_claude,
        )
        self.assertIn(
            r"claude:[[:space:]]+current[[:space:]]+\(v7\)",
            self.real_claude,
        )
        for source in (self.lib, self.real_claude):
            self.assertNotIn("0.7.5", source)
            self.assertNotIn("protocol 17", source)

    def test_real_pi_pins_exact_herdr_protocol_and_pi_v8(self) -> None:
        source = self.real_pi
        self.assertIn(f'[ "$HERDR_VERSION" = "herdr {HERDR_VERSION}" ]', source)
        self.assertIn(f".protocol == {HERDR_PROTOCOL}", source)
        self.assertIn(
            "grep -Eq '^pi:[[:space:]]+current[[:space:]]+\\(v8\\)([[:space:]]+\\(.+\\))?$'",
            source,
        )
        self.assertNotIn("grep -q 'current'", source)

    def test_awaiting_scenario_uses_nanosecond_report_sequences(self) -> None:
        awaiting = (ROOT / "e2e/15-awaiting.sh").read_text(encoding="utf-8")
        self.assertIn(
            "SEQ=\"$(python3 -c 'import time; print(time.time_ns())')\"",
            awaiting,
        )
        self.assertNotIn("date +%s", awaiting)
        self.assertIn("--harness pi", awaiting)
        self.assertIn('--agent-session-path "$AGENT_SESSION_PATH"', awaiting)

    def test_active_e2e_descriptions_use_current_protocol_and_integrations(self) -> None:
        readme = (ROOT / "e2e/README.md").read_text(encoding="utf-8")
        managed = (ROOT / "e2e/16-managed-p17.sh").read_text(encoding="utf-8")
        configured = (ROOT / "e2e/17-configured-p17-runner.sh").read_text(
            encoding="utf-8"
        )
        awaiting = (ROOT / "e2e/15-awaiting.sh").read_text(encoding="utf-8")
        fake_agent = (ROOT / "e2e/fake-agent.sh").read_text(encoding="utf-8")
        fake_pi = (ROOT / "e2e/fake-bin/pi").read_text(encoding="utf-8")
        fake_claude = (ROOT / "e2e/fake-bin/claude").read_text(encoding="utf-8")
        terminal_shim = (ROOT / "e2e/fake-bin/managed-terminal-shim.py").read_text(
            encoding="utf-8"
        )
        for source in (
            readme,
            managed,
            configured,
            awaiting,
            fake_agent,
            fake_pi,
            fake_claude,
            terminal_shim,
        ):
            self.assertNotIn("0.7.5", source)
            self.assertNotIn("protocol-17", source)
            self.assertNotIn("protocol 17", source)
        self.assertIn("Herdr 0.8.0 / socket protocol 19", readme)
        self.assertIn("protocol-19/current", readme)
        self.assertIn("Pi integration v8", readme)
        self.assertIn("Pi integration v8", awaiting)
        self.assertIn("current Claude integration v7", readme)
        self.assertIn("protocol-19", managed)
        self.assertIn("protocol-19", configured)
        self.assertIn("protocol-19", fake_pi)
        self.assertIn("protocol-19", fake_claude)
        self.assertIn("protocol-19", terminal_shim)
        for path in sorted((ROOT / "e2e").glob("*.sh")):
            source = path.read_text(encoding="utf-8")
            with self.subTest(path=path.name):
                self.assertNotIn("0.7.5", source)
                self.assertNotIn("protocol-17", source)
                self.assertNotIn("protocol 17", source)
                self.assertNotIn("Pi integration v6", source)
        issue_template = (ROOT / ".github/ISSUE_TEMPLATE/bug_report.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn('placeholder: "0.8.0"', issue_template)
        self.assertTrue((ROOT / "e2e/16-managed-p17.sh").is_file())
        self.assertTrue((ROOT / "e2e/17-configured-p17-runner.sh").is_file())

    def test_wrapper_preserves_private_artifact_ownership_and_suite_status(self) -> None:
        self.assertIn("PIPESTATUS[0]", self.wrapper)
        self.assertIn("e2e-artifacts", self.wrapper)
        self.assertIn(".owned-artifacts", self.wrapper)
        self.assertIn("hb-e2e-run", self.wrapper)
        self.assertNotIn("hb-e2e-run.*", self.wrapper)
        self.assertNotIn("E2E_ARTIFACT_ROOT=", self.wrapper)

    def test_no_floating_or_provider_or_docker_escape_hatches(self) -> None:
        combined = self.workflow + "\n" + self.wrapper
        for forbidden in (
            "herdr update",
            ":latest",
            "docker run",
            "E2E_REAL_PI=1",
            "E2E_REAL_CLAUDE_HAIKU=1",
        ):
            self.assertNotIn(forbidden, combined)


if __name__ == "__main__":
    unittest.main()
