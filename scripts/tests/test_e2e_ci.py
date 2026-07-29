from __future__ import annotations

import re
import unittest
from pathlib import Path

from _support import REPO_ROOT as ROOT


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

    def test_workflow_has_one_bounded_read_only_live_job_after_fast_gates(self) -> None:
        self.assertEqual(len(re.findall(r"^  live-e2e:\s*$", self.workflow, re.M)), 1)
        job = self.workflow.split("  live-e2e:", 1)[1]
        self.assertIn("needs: [fmt, clippy, docs, scripts, e2e-safety, test]", job)
        self.assertIn("runs-on: ubuntu-latest", job)
        self.assertRegex(job, r"timeout-minutes:\s*[1-9][0-9]*")
        self.assertIn("permissions:\n      contents: read", job)
        self.assertIn("persist-credentials: false", job)

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
        self.assertIn("0.7.5", job)
        self.assertIn("x86_64", job)
        self.assertIn("3dc83288073e4c2d3c679a30e7be97bcca9141c6fd17dbbb9219142e95c59253", job)
        for forbidden in ("HERDR_API_KEY", "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "E2E_REAL_PI"):
            self.assertNotIn(forbidden, job)

    def test_wrapper_pins_and_verifies_supply_chain_and_protocol(self) -> None:
        self.assertTrue(self.wrapper_path.is_file())
        self.assertIn("set -euo pipefail", self.wrapper)
        self.assertIn("umask 077", self.wrapper)
        self.assertIn("https://github.com/herdrdev/herdr/releases/download/v0.7.5/herdr-linux-x86_64", self.wrapper)
        self.assertIn("3dc83288073e4c2d3c679a30e7be97bcca9141c6fd17dbbb9219142e95c59253", self.wrapper)
        self.assertIn('"$HERDR_BIN" --version', self.wrapper)
        self.assertIn("HERDR_VERSION=0.7.5", self.wrapper)
        self.assertIn("protocol", self.wrapper)
        self.assertIn("17", self.wrapper)
        self.assertIn('"$REPO_ROOT/e2e/run-all.sh" --require-all', self.wrapper)

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
