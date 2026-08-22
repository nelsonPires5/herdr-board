from __future__ import annotations

import fnmatch
import json
import os
import re
import unittest
from pathlib import Path

from _support import REPO_ROOT as ROOT

SCENARIO_GLOB = "[0-9][0-9]-*.sh"
SCENARIO_NAME_RE = re.compile(r"\b([0-9]{2}-[a-z0-9][a-z0-9-]*\.sh)\b")
INSTALL_REF_RE = re.compile(r"--ref\s+v(\d+\.\d+\.\d+)")
WORKSPACE_VERSION_RE = re.compile(
    r"\[workspace\.package\][^\[]*?^version\s*=\s*\"([^\"]+)\"", re.M | re.S
)

# Documents whose `herdr plugin install … --ref vX.Y.Z` pins ship to users as a
# copy-pasteable command. Must stay in sync with prepare-release.py's
# INSTALL_REF_DOCS. CHANGELOG.md is deliberately absent: its historical entries
# quote the ref that was current at the time and must never be repinned.
INSTALL_REF_DOCS = ("README.md", "docs/install.md", "docs/operations.md")

# Changelog convention, enforced for the `Unreleased` section only. Released
# sections document the conventions of their time and are never rewritten.
CHANGELOG_CATEGORIES = ("### Added", "### Changed", "### Fixed", "### Removed")
CHANGELOG_ENTRY_RE = re.compile(
    r"^\- \[#(\d+)\]\(https://github\.com/nelsonPires5/herdr-board/pull/\d+\)"
)
CHANGELOG_ENTRY_MAX_CHARS = 200

# Every markdown file this repo maintains as documentation (not vendored, not
# generated). Used by the link and scenario-catalog contracts below.
MAINTAINED_MARKDOWN = (
    "AGENTS.md",
    "CONTRIBUTING.md",
    "README.md",
    "CHANGELOG.md",
    "e2e/README.md",
)

# Herdr socket methods `board-herdr` actually calls. The checked-in API schema
# fixture must keep describing all of them, or the crate's decoding tests are
# validating against a contract the client no longer speaks.
BOARD_HERDR_METHODS = (
    "agent.get",
    "agent.prompt",
    "agent.start",
    "agent.wait",
    "events.subscribe",
    "notification.show",
    "pane.close",
    "pane.focus",
    "pane.get",
    "pane.layout",
    "pane.list",
    "pane.read",
    "pane.rename",
    "pane.send_keys",
    "pane.send_text",
    "pane.split",
    "ping",
    "session.snapshot",
    "tab.create",
    "tab.list",
    "tab.rename",
    "workspace.close",
    "workspace.create",
    "workspace.list",
)


def maintained_markdown() -> list[Path]:
    return [ROOT / relative for relative in MAINTAINED_MARKDOWN] + sorted(
        (ROOT / "docs").glob("*.md")
    )


def scenario_paths() -> list[Path]:
    return sorted((ROOT / "e2e").glob(SCENARIO_GLOB))


def parse_markdown_table(text: str, heading: str) -> dict[str, list[str]]:
    """Parse the first markdown table after `heading` into {first cell: rest}.

    Version facts used to be matched as raw substrings of the rendered row, so
    reformatting a table (or widening a column) broke the gate instead of the
    fact. Parsing the table means only a changed *fact* can fail.
    """
    lines = text.splitlines()
    start = next(
        (index for index, line in enumerate(lines) if line.strip() == heading), None
    )
    if start is None:
        raise AssertionError(f"heading {heading!r} not found")
    rows: dict[str, list[str]] = {}
    for line in lines[start:]:
        stripped = line.strip()
        if not stripped.startswith("|"):
            if rows:
                break
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if all(set(cell) <= {"-", ":"} and cell for cell in cells):
            continue
        rows[cells[0]] = cells[1:]
    if not rows:
        raise AssertionError(f"no table found under {heading!r}")
    return rows


def fenced_block(text: str, heading: str, language: str = "bash") -> list[str]:
    """Return the non-empty lines of the first fenced block after `heading`."""
    lines = text.splitlines()
    start = next(
        (index for index, line in enumerate(lines) if line.strip() == heading), None
    )
    if start is None:
        raise AssertionError(f"heading {heading!r} not found")
    opened = next(
        (
            index
            for index in range(start, len(lines))
            if lines[index].strip() == f"```{language}"
        ),
        None,
    )
    if opened is None:
        raise AssertionError(f"no ```{language} block after {heading!r}")
    body: list[str] = []
    for line in lines[opened + 1 :]:
        if line.strip() == "```":
            return body
        if line.strip():
            body.append(line.strip())
    raise AssertionError(f"unterminated ```{language} block after {heading!r}")


class DocumentationContractTests(unittest.TestCase):
    def test_final_version_and_ownership_catalog_is_documented(self) -> None:
        index = (ROOT / "docs/README.md").read_text(encoding="utf-8")
        contract = parse_markdown_table(index, "## Contract at a glance")
        last_number = scenario_paths()[-1].name[:2]
        for surface, expected in (
            ("Board socket", "v1;"),
            ("SQLite", "schema v15"),
            ("Herdr client", "0.8.2 / socket protocol 20"),
            ("Herdr integrations", "Pi v8; Claude v7"),
            ("Runtime launch", "daemon-owned"),
            ("Config", "typed `RootConfig`"),
            ("Live catalog", f"scenarios 01–{last_number}"),
        ):
            with self.subTest(surface=surface):
                self.assertIn(surface, contract, "contract row is missing")
                self.assertIn(expected, contract[surface][0])

    def test_scenario_catalog_and_runner_cover_every_numbered_scenario(self) -> None:
        """Derive the catalog bound; never hardcode the last scenario number."""
        scenarios = scenario_paths()
        self.assertEqual(
            [path.name[:2] for path in scenarios],
            [f"{number:02d}" for number in range(1, len(scenarios) + 1)],
            "e2e scenario numbers must be contiguous starting at 01",
        )
        runner = (ROOT / "e2e/run-all.sh").read_text(encoding="utf-8")
        for scenario in scenarios:
            with self.subTest(scenario=scenario.name):
                self.assertIn(scenario.name, runner)

    def test_every_numbered_scenario_is_executable(self) -> None:
        """Each scenario advertises `bash e2e/NN-*.sh` *and* standalone running.

        `run-all.sh` invokes them through `bash`, which hides a missing +x bit
        until someone runs one directly.
        """
        for scenario in scenario_paths():
            with self.subTest(scenario=scenario.name):
                self.assertTrue(
                    os.access(scenario, os.X_OK),
                    f"{scenario.name} is not executable; run: chmod +x e2e/{scenario.name}",
                )

    def test_documents_quoting_the_scenario_range_track_the_last_scenario(self) -> None:
        """Every doc that spells the catalog range must name the real last scenario.

        Adding a scenario used to leave stale `01 through 26` prose in docs the
        version-matrix assertion above does not read, so only CI caught it — and
        only for `docs/README.md`. Derive the bound instead of hardcoding it.
        """
        scenarios = scenario_paths()
        last = scenarios[-1]
        last_number = last.name[:2]
        for relative, expected in (
            ("docs/README.md", f"scenarios 01–{last_number}"),
            ("e2e/README.md", f"**01 through {last_number}**"),
            ("AGENTS.md", f"scenarios 01–{last_number}"),
            ("README.md", f"scenarios 01–{last_number}"),
            ("docs/testing.md", f"scenarios 01–{last_number}"),
            ("docs/implementation.md", f"through `e2e/{last.name}`"),
        ):
            with self.subTest(document=relative):
                self.assertIn(
                    expected, (ROOT / relative).read_text(encoding="utf-8")
                )

    def test_scenario_catalog_table_lives_only_in_e2e_readme(self) -> None:
        """`e2e/README.md` is the declared authority for the use-case catalog.

        A second per-scenario table anywhere else goes stale silently — that is
        exactly how `docs/testing.md` ended up missing `27-rescue-dead-pane.sh`.
        Prose may name a scenario; a *table row* may not.
        """
        catalog = (ROOT / "e2e/README.md").read_text(encoding="utf-8")
        for scenario in scenario_paths():
            with self.subTest(scenario=scenario.name):
                self.assertIn(scenario.name, catalog)

        for document in maintained_markdown():
            if document == ROOT / "e2e/README.md":
                continue
            for line in document.read_text(encoding="utf-8").splitlines():
                if not line.lstrip().startswith("|"):
                    continue
                match = SCENARIO_NAME_RE.search(line)
                with self.subTest(document=str(document.relative_to(ROOT))):
                    self.assertIsNone(
                        match,
                        f"{document.relative_to(ROOT)} has a scenario table row for "
                        f"{match.group(1) if match else ''}; the catalog belongs "
                        "only in e2e/README.md",
                    )

    def test_documented_install_pin_matches_the_workspace_version(self) -> None:
        """`--ref vX.Y.Z` installs a tag; a stale pin ships the previous release.

        `scripts/prepare-release.py` rewrites and verifies these same documents,
        so this assertion also proves the two lists have not diverged.
        """
        cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        match = WORKSPACE_VERSION_RE.search(cargo)
        self.assertIsNotNone(match, "Cargo.toml [workspace.package].version not found")
        version = match.group(1)

        found_any = False
        for relative in INSTALL_REF_DOCS:
            path = ROOT / relative
            if not path.is_file():
                continue
            for pin in INSTALL_REF_RE.findall(path.read_text(encoding="utf-8")):
                found_any = True
                with self.subTest(document=relative):
                    self.assertEqual(
                        pin,
                        version,
                        f"{relative} pins --ref v{pin} but the workspace version is "
                        f"{version}; run scripts/prepare-release.py apply",
                    )
        self.assertTrue(found_any, "no --ref pin found in the install documents")

    def test_ci_gate_steps_match_the_documented_gate_list(self) -> None:
        """docs/README.md's gate block and ci.yml must agree in both directions.

        The list used to exist in four places that disagreed (docs/README.md had
        `bash e2e/test-harness.sh`, README.md had `./e2e/run-all.sh`, ci.yml had
        neither). One source plus this two-way assertion is what keeps it fixed.
        """
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertNotIn(
            "run: |",
            workflow,
            "multi-line run: blocks would defeat this single-line parse",
        )
        ci_commands = {
            match.group(1).strip()
            for match in re.finditer(r"^\s*run:\s*(\S.*)$", workflow, re.M)
        }
        documented = set(
            fenced_block(
                (ROOT / "docs/README.md").read_text(encoding="utf-8"),
                "## Test gates (single source)",
            )
        )
        self.assertEqual(
            documented,
            ci_commands,
            "docs/README.md's gate block and ci.yml's run: steps must be identical",
        )
        self.assertIn("bash e2e/test-harness.sh", documented)

        # The other three copies must stay links, not lists.
        for relative in ("AGENTS.md", "CONTRIBUTING.md", "README.md"):
            text = (ROOT / relative).read_text(encoding="utf-8")
            with self.subTest(document=relative):
                self.assertIn("docs/README.md#test-gates-single-source", text)
                self.assertNotIn(
                    "python3 -m unittest discover",
                    text,
                    f"{relative} re-lists the gates; link docs/README.md instead",
                )

    def test_every_python_test_module_runs_in_some_ci_job(self) -> None:
        """No `scripts/tests/test_*.py` may fall outside every CI pattern.

        CI runs this tier as three jobs matching disjoint `-p` patterns instead
        of one `test_*.py` sweep. That trade buys independent failures, but it
        also means a new module lands in *no* job and silently never runs. This
        is the assertion that makes the split safe.
        """
        patterns = [
            match.group(1)
            for line in fenced_block(
                (ROOT / "docs/README.md").read_text(encoding="utf-8"),
                "## Test gates (single source)",
            )
            if (match := re.search(r"-p '([^']+)'", line))
        ]
        self.assertTrue(patterns, "no python -p patterns in the gate block")
        modules = sorted(p.name for p in (ROOT / "scripts/tests").glob("test_*.py"))
        self.assertTrue(modules, "no test modules discovered")
        for module in modules:
            with self.subTest(module=module):
                self.assertTrue(
                    any(fnmatch.fnmatch(module, pattern) for pattern in patterns),
                    f"{module} matches no CI pattern {patterns}; it would never run",
                )

    def test_unreleased_changelog_entries_follow_the_convention(self) -> None:
        """Unreleased entries are one short, PR-linked, user-facing line each.

        Entries used to be one physical line that read as a paragraph of
        implementation detail (env var names, backoff values, module renames).
        The PR link already carries the rationale; the entry must say only
        what the user gains, sit under a Keep-a-Changelog category heading,
        and stay within CHANGELOG_ENTRY_MAX_CHARS so the section stays
        scannable. Historical released sections are deliberately exempt.
        """
        text = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
        body = text.split("## [Unreleased]", 1)[1]
        unreleased = body.split("\n## ", 1)[0]
        category = None
        for line in unreleased.splitlines():
            stripped = line.strip()
            if not stripped:
                continue
            with self.subTest(line=stripped[:60]):
                if stripped.startswith("### "):
                    category = stripped
                    self.assertIn(
                        stripped,
                        CHANGELOG_CATEGORIES,
                        f"{stripped!r} is not a Keep-a-Changelog category "
                        f"(use one of {CHANGELOG_CATEGORIES})",
                    )
                    continue
                self.assertIsNotNone(
                    category,
                    f"entry has no category heading above it: {stripped[:60]!r}",
                )
                self.assertIsNotNone(
                    CHANGELOG_ENTRY_RE.match(stripped),
                    "entry must start with `- [#NN](https://github.com/"
                    "nelsonPires5/herdr-board/pull/NN)`: " + stripped[:60],
                )
                self.assertLessEqual(
                    len(stripped),
                    CHANGELOG_ENTRY_MAX_CHARS,
                    f"entry exceeds {CHANGELOG_ENTRY_MAX_CHARS} chars; move the "
                    f"detail to the PR body: {stripped[:60]!r}",
                )

    def test_release_workflow_stages_what_the_tool_writes(self) -> None:
        """The Prepare Release workflow must not repeat the managed-file list.

        It used to `git add` four filenames literally. When `apply` grew the
        install-ref repin, those three extra files were rewritten on the runner
        and dropped at staging, so v0.9.1 was cut with stale pins — and
        `verify`, which runs before `git add`, could not see it.
        """
        workflow = (ROOT / ".github/workflows/prepare-release.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            "prepare-release.py --repo . files",
            workflow,
            "stage the tool's own file list instead of repeating it",
        )
        for literal in ("git add Cargo.toml", "git add README.md"):
            self.assertNotIn(
                literal,
                workflow,
                f"{literal!r} hardcodes the managed-file list again",
            )
        self.assertIn(
            "-p 'test_docs.py'",
            workflow,
            "re-run the documentation contracts against the bumped tree",
        )

    def test_ci_runs_on_main_and_dev_pushes(self) -> None:
        """Both long-lived branches run the full CI; dev is integration, main production.

        Feature/release PRs merge into `dev`, so the push gate must cover it —
        otherwise a broken integration branch ships silently until promotion.
        """
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("branches: [main, dev]", workflow)

    def test_release_workflow_never_publishes_from_dev(self) -> None:
        """A dev push runs CI but must never publish: Release stays bound to main.

        Both the `workflow_run` branch filter and the job's head_branch guard
        must keep naming `main`, or a dev CI completion would become a release
        path. This is the gate D4 of the dev-branch rollout exists to protect.
        """
        workflow = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        self.assertIn(
            "branches:\n      - main",
            workflow,
            "workflow_run trigger must stay filtered to main",
        )
        self.assertIn(
            "github.event.workflow_run.head_branch == 'main'",
            workflow,
            "release job must stay gated to green CI runs on main",
        )

    def test_prepare_release_targets_an_input_base(self) -> None:
        """Prepare Release defaults to `dev` and supports `main` for hotfixes.

        The checkout and the created/reused PR must both follow the input;
        a rerun must retarget an existing PR whose base changed.
        """
        workflow = (ROOT / ".github/workflows/prepare-release.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("base:", workflow, "base input is missing")
        self.assertIn("default: dev", workflow, "dev must be the default base")
        self.assertIn("- dev", workflow)
        self.assertIn("- main", workflow)
        self.assertIn(
            "ref: ${{ inputs.base }}",
            workflow,
            "release branch must be cut from the selected base",
        )
        self.assertIn(
            '--base "${{ inputs.base }}"',
            workflow,
            "created/reused PR must target the selected base",
        )
        self.assertNotIn(
            "--base main",
            workflow,
            "the PR base must never be hardcoded to main",
        )

    def test_promote_workflow_merges_dev_but_never_tags(self) -> None:
        """The action-owned promotion: green dev CI with a pending bump opens
        (or updates) the `dev -> main` PR; a maintainer merges it, because
        GitHub never creates workflow runs for pushes made with GITHUB_TOKEN —
        an automated merge would silently skip CI and Release could never tag
        it. Tagging stays exclusively in Release.

        The promote workflow must be bound to `dev` CI completions, open the
        PR through `gh pr create`/`gh pr edit`, never merge it, and contain no
        tag- or release-creation step.
        """
        workflow = (ROOT / ".github/workflows/promote.yml").read_text(encoding="utf-8")
        self.assertIn(
            "branches:\n      - dev",
            workflow,
            "promote trigger must be dev-only",
        )
        self.assertIn(
            "github.event.workflow_run.head_branch == 'dev'",
            workflow,
            "promote job must stay gated to dev CI runs",
        )
        self.assertIn("gh pr create", workflow, "promotion PR must be opened here")
        self.assertIn("gh pr edit", workflow, "promotion PR must be updated on reruns")
        self.assertNotIn(
            "gh pr merge",
            workflow,
            "a maintainer merges the promotion PR (GITHUB_TOKEN pushes cannot trigger CI)",
        )
        self.assertNotIn("git tag", workflow, "promote must never create tags")
        self.assertNotIn("gh release", workflow, "promote must never publish")

    def test_maintained_markdown_links_resolve(self) -> None:
        for document in maintained_markdown():
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

    def test_schema_fixture_still_describes_the_pinned_herdr_contract(self) -> None:
        """Assert the invariants the fixture exists to prove, not a digest.

        This used to be a bare sha256 with no message: any regeneration failed
        the gate identically, whether it changed the protocol number or only
        reordered keys, and the failure said nothing about what to do.
        """
        fixture = ROOT / "crates/board-herdr/tests/fixtures/schema.json"
        schema = json.loads(fixture.read_text(encoding="utf-8"))
        hint = (
            "regenerate with `herdr api schema --json` from exactly Herdr 0.8.2 "
            "and re-verify docs/herdr.md before changing this"
        )
        self.assertEqual(schema.get("protocol"), 20, f"protocol must stay 20 — {hint}")
        self.assertEqual(schema.get("schema_version"), 1, hint)

        methods = {
            variant["properties"]["method"]["const"]
            for variant in schema["schemas"]["request"]["oneOf"]
        }
        missing = sorted(set(BOARD_HERDR_METHODS) - methods)
        self.assertEqual(missing, [], f"fixture no longer describes {missing} — {hint}")

        statuses = schema["schemas"]["event"]["$defs"]["AgentStatus"]["enum"]
        self.assertEqual(
            sorted(statuses),
            ["blocked", "done", "idle", "unknown", "working"],
            f"AgentStatus drives the awaiting/done watcher logic — {hint}",
        )


if __name__ == "__main__":
    unittest.main()
