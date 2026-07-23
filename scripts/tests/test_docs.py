from __future__ import annotations

import hashlib
import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class DocumentationContractTests(unittest.TestCase):
    def test_final_version_and_ownership_catalog_is_documented(self) -> None:
        index = (ROOT / "docs/README.md").read_text(encoding="utf-8")
        for text in (
            "Board socket | v1",
            "SQLite | schema v11",
            "Herdr client | 0.7.5 / socket protocol 17",
            "Runtime launch | daemon-owned",
            "Config | typed `RootConfig`",
            "scenarios 01–21",
        ):
            self.assertIn(text, index)

    def test_scenario_catalog_and_runner_cover_01_through_21(self) -> None:
        scenarios = sorted((ROOT / "e2e").glob("[0-9][0-9]-*.sh"))
        self.assertEqual(
            [path.name[:2] for path in scenarios],
            [f"{number:02d}" for number in range(1, 22)],
        )
        runner = (ROOT / "e2e/run-all.sh").read_text(encoding="utf-8")
        for scenario in scenarios:
            self.assertIn(scenario.name, runner)

    def test_maintained_markdown_links_resolve(self) -> None:
        documents = [
            ROOT / "AGENTS.md",
            ROOT / "README.md",
            ROOT / "CHANGELOG.md",
            *sorted((ROOT / "docs").glob("*.md")),
            ROOT / "e2e/README.md",
        ]
        for document in documents:
            for link in re.findall(r"\[[^]]+\]\(([^)]+)\)", document.read_text()):
                target = link.split("#", 1)[0]
                if not target or "://" in target or target.startswith("mailto:"):
                    continue
                self.assertTrue(
                    (document.parent / target).exists(),
                    f"broken link in {document}: {link}",
                )

    def test_obsolete_herdr_worktree_surface_has_no_rust_consumers(self) -> None:
        source = "\n".join(
            path.read_text(encoding="utf-8")
            for path in (ROOT / "crates").rglob("*.rs")
        )
        for symbol in (
            "worktree_create",
            "worktree_remove",
            "WorktreeCreateParams",
            "WorktreeInfo",
            "WorktreeCreated",
            "WorktreeRemoved",
        ):
            self.assertNotIn(symbol, source)

    def test_schema_fixture_hash_is_unchanged(self) -> None:
        fixture = ROOT / "crates/board-herdr/tests/fixtures/schema.json"
        digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
        self.assertEqual(
            digest,
            "1ef4eb9ec655cb0c89726895f437d8654bdde13a22e591fda06a9015d03d88c7",
        )


if __name__ == "__main__":
    unittest.main()
