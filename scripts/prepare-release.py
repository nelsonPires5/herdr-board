#!/usr/bin/env python3
"""Prepare release state for herdr-board.

This helper is workflow-friendly and stdlib-only. It can:
- plan a semver bump from the current workspace version;
- apply a target release version across Cargo.toml, herdr-plugin.toml,
  Cargo.lock, and CHANGELOG.md;
- keep the release cut idempotent on a rerun when the target is already ready.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from datetime import date
from pathlib import Path
from typing import Dict, Iterable, Tuple

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11 fallback.
    tomllib = None

SEMVER_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
LOCK_BLOCK_RE = re.compile(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]|\Z)")
REF_LINE_RE = re.compile(r"^\[([^\]]+)\]:\s+(\S+)\s*$")
TARGET_HEADING_RE = re.compile(r"^## \[(?P<version>[^\]]+)\] - .+$")
LOCAL_PACKAGES = ("board-cli", "board-core", "board-daemon", "board-herdr", "board-tui")


class ReleaseError(RuntimeError):
    pass


@dataclass(frozen=True)
class Plan:
    current: str
    target: str


@dataclass(frozen=True)
class ApplyResult:
    current: str
    target: str
    changed: bool
    already_prepared: bool


@dataclass(frozen=True)
class ChangelogAnalysis:
    unreleased_body: str
    target_section_exists: bool
    prepared: bool


# -------------------------
# semver / version helpers
# -------------------------

def parse_semver(version: str) -> Tuple[int, int, int]:
    match = SEMVER_RE.match(version)
    if not match:
        raise ReleaseError(f"invalid semver: {version!r}")
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def format_semver(parts: Tuple[int, int, int]) -> str:
    return f"{parts[0]}.{parts[1]}.{parts[2]}"


def bump_semver(version: str, bump: str) -> str:
    major, minor, patch = parse_semver(version)
    if bump == "patch":
        patch += 1
    elif bump == "minor":
        minor += 1
        patch = 0
    elif bump == "major":
        major += 1
        minor = 0
        patch = 0
    else:
        raise ReleaseError(f"unknown bump kind: {bump!r}")
    return format_semver((major, minor, patch))


# -------------------------
# file I/O
# -------------------------

def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ReleaseError(f"missing file: {path}") from exc


def atomic_write(path: Path, content: str) -> None:
    """Replace one file atomically, keeping the temporary file beside it."""
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        if path.exists():
            os.chmod(temporary, stat.S_IMODE(path.stat().st_mode))
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


# Kept as a small seam for tests and for callers that want to inject a write
# failure. Each file is independently atomic; a rerun repairs any earlier
# files that were successfully replaced.
def write_text(path: Path, content: str) -> None:
    atomic_write(path, content)


# -------------------------
# Cargo.toml / lock / plugin parsing
# -------------------------

def parse_workspace_version(cargo_toml: str) -> str:
    if tomllib is not None:
        try:
            data = tomllib.loads(cargo_toml)
            version = data["workspace"]["package"]["version"]
        except Exception as exc:  # pragma: no cover - invalid fixture / repo file.
            raise ReleaseError("Cargo.toml missing [workspace.package].version") from exc
        if not isinstance(version, str):
            raise ReleaseError("Cargo.toml [workspace.package].version is not a string")
        return version

    # Fallback: scan the [workspace.package] section.
    in_workspace_package = False
    for raw in cargo_toml.splitlines():
        line = raw.strip()
        if line.startswith("[") and line.endswith("]"):
            in_workspace_package = line == "[workspace.package]"
            continue
        if in_workspace_package and line.startswith("version"):
            match = re.match(r'^version\s*=\s*"([^"]+)"\s*$', line)
            if match:
                return match.group(1)
    raise ReleaseError("Cargo.toml missing [workspace.package].version")


def workspace_version_changed(head_cargo_toml: str, parent_cargo_toml: str) -> bool:
    """Return whether the workspace version changed from the first parent."""
    return parse_workspace_version(head_cargo_toml) != parse_workspace_version(parent_cargo_toml)


def rewrite_workspace_version(cargo_toml: str, target: str) -> str:
    lines = cargo_toml.splitlines(keepends=True)
    out: list[str] = []
    in_workspace_package = False
    replaced = False
    for raw in lines:
        line = raw.strip()
        if line.startswith("[") and line.endswith("]"):
            in_workspace_package = line == "[workspace.package]"
            out.append(raw)
            continue
        if in_workspace_package and re.match(r'^version\s*=\s*"[^"]+"\s*$', line):
            if replaced:
                raise ReleaseError("Cargo.toml has multiple [workspace.package].version lines")
            prefix = raw[: len(raw) - len(raw.lstrip())]
            newline = "\n" if raw.endswith("\n") else ""
            out.append(f'{prefix}version = "{target}"{newline}')
            replaced = True
        else:
            out.append(raw)
    if not replaced:
        raise ReleaseError("Cargo.toml missing [workspace.package].version")
    return "".join(out)


def parse_simple_version_toml(toml_text: str) -> str:
    match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', toml_text, re.M)
    if not match:
        raise ReleaseError("missing top-level version = \"...\" line")
    return match.group(1)


def rewrite_simple_version_toml(toml_text: str, target: str) -> str:
    match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', toml_text, re.M)
    if not match:
        raise ReleaseError("missing top-level version = \"...\" line")
    return toml_text[: match.start(1)] + target + toml_text[match.end(1) :]


def parse_lock_versions(lock_text: str) -> Dict[str, str]:
    versions: Dict[str, str] = {}
    for match in LOCK_BLOCK_RE.finditer(lock_text):
        block = match.group(0)
        name_match = re.search(r'^name = "([^"]+)"\s*$', block, re.M)
        version_match = re.search(r'^version = "([^"]+)"\s*$', block, re.M)
        if not name_match or not version_match:
            continue
        name = name_match.group(1)
        if name in LOCAL_PACKAGES:
            versions[name] = version_match.group(1)
    missing = [name for name in LOCAL_PACKAGES if name not in versions]
    if missing:
        raise ReleaseError(f"Cargo.lock missing local package entries: {', '.join(missing)}")
    return versions


def rewrite_lock_versions(lock_text: str, target: str) -> str:
    def replace_block(match: re.Match[str]) -> str:
        block = match.group(0)
        name_match = re.search(r'^name = "([^"]+)"\s*$', block, re.M)
        if not name_match:
            return block
        name = name_match.group(1)
        if name not in LOCAL_PACKAGES:
            return block
        new_block, count = re.subn(
            r'^version = "([^"]+)"\s*$',
            f'version = "{target}"',
            block,
            count=1,
            flags=re.M,
        )
        if count != 1:
            raise ReleaseError(f"Cargo.lock block for {name!r} has no version line")
        return new_block

    return LOCK_BLOCK_RE.sub(replace_block, lock_text)


# -------------------------
# changelog parsing / rewrite
# -------------------------

def repo_url_from_env_or_git(repo_root: Path, explicit: str | None = None) -> str:
    if explicit:
        return normalize_repo_url(explicit)
    repo_env = os.environ.get("GITHUB_REPOSITORY")
    if repo_env:
        return f"https://github.com/{repo_env}"
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo_root), "remote", "get-url", "origin"],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        raise ReleaseError("cannot infer repo URL; pass --repo-url") from exc
    return normalize_repo_url(proc.stdout.strip())


def normalize_repo_url(value: str) -> str:
    url = value.strip()
    if url.endswith(".git"):
        url = url[:-4]
    if url.startswith("https://github.com/"):
        return url
    if url.startswith("git@github.com:"):
        return "https://github.com/" + url[len("git@github.com:") :]
    if url.startswith("ssh://git@github.com/"):
        return "https://github.com/" + url[len("ssh://git@github.com/") :]
    return url


def split_changelog(changelog_text: str) -> tuple[list[str], list[str]]:
    lines = changelog_text.splitlines(keepends=True)
    link_start = next((i for i, line in enumerate(lines) if REF_LINE_RE.match(line)), None)
    if link_start is None:
        raise ReleaseError("CHANGELOG.md has no link reference section")
    return lines[:link_start], lines[link_start:]


def parse_ref_lines(ref_lines: Iterable[str]) -> Dict[str, str]:
    refs: Dict[str, str] = {}
    for line in ref_lines:
        match = REF_LINE_RE.match(line)
        if not match:
            raise ReleaseError(f"invalid changelog reference line: {line.rstrip()!r}")
        refs[match.group(1)] = match.group(2)
    return refs


def analyze_changelog(changelog_text: str, target: str, repo_url: str) -> ChangelogAnalysis:
    sections, refs = split_changelog(changelog_text)
    unreleased_idx = next((i for i, line in enumerate(sections) if line.startswith("## [Unreleased]")), None)
    if unreleased_idx is None:
        raise ReleaseError("CHANGELOG.md is missing the [Unreleased] heading")
    next_heading_idx = next(
        (i for i in range(unreleased_idx + 1, len(sections)) if sections[i].startswith("## ")),
        len(sections),
    )
    unreleased_body = "".join(sections[unreleased_idx + 1 : next_heading_idx]).strip("\n")
    target_heading_exists = any(
        TARGET_HEADING_RE.match(line) and TARGET_HEADING_RE.match(line).group("version") == target
        for line in sections
    )
    refs_map = parse_ref_lines(refs)
    compare_url = f"{repo_url}/compare/v{target}...HEAD"
    release_url = f"{repo_url}/releases/tag/v{target}"
    prepared = (
        target_heading_exists
        and not unreleased_body.strip()
        and refs_map.get("Unreleased") == compare_url
        and refs_map.get(target) == release_url
    )
    return ChangelogAnalysis(unreleased_body=unreleased_body, target_section_exists=target_heading_exists, prepared=prepared)


def rewrite_changelog(changelog_text: str, target: str, release_date: str, repo_url: str) -> str:
    sections, refs = split_changelog(changelog_text)
    unreleased_idx = next((i for i, line in enumerate(sections) if line.startswith("## [Unreleased]")), None)
    if unreleased_idx is None:
        raise ReleaseError("CHANGELOG.md is missing the [Unreleased] heading")
    next_heading_idx = next(
        (i for i in range(unreleased_idx + 1, len(sections)) if sections[i].startswith("## ")),
        len(sections),
    )
    unreleased_body = "".join(sections[unreleased_idx + 1 : next_heading_idx]).strip("\n")
    if not unreleased_body.strip():
        raise ReleaseError("CHANGELOG.md [Unreleased] is empty")

    compare_url = f"{repo_url}/compare/v{target}...HEAD"
    release_url = f"{repo_url}/releases/tag/v{target}"

    prefix = "".join(sections[: unreleased_idx + 1]) + "\n"
    out = [prefix, f"## [{target}] - {release_date}\n\n", unreleased_body.rstrip("\n"), "\n"]
    suffix = "".join(sections[next_heading_idx:]).lstrip("\n")
    if suffix:
        out.extend(["\n", suffix])

    rendered_refs: list[str] = []
    inserted_target = False
    seen_unreleased = False
    for line in refs:
        match = REF_LINE_RE.match(line)
        if not match:
            raise ReleaseError(f"invalid changelog reference line: {line.rstrip()!r}")
        name = match.group(1)
        if name == "Unreleased":
            rendered_refs.append(f"[Unreleased]: {compare_url}\n")
            if not inserted_target:
                rendered_refs.append(f"[{target}]: {release_url}\n")
                inserted_target = True
            seen_unreleased = True
        elif name == target:
            if not inserted_target:
                rendered_refs.append(f"[{target}]: {release_url}\n")
                inserted_target = True
            # Skip the old target line; the canonical line is inserted after Unreleased.
        else:
            rendered_refs.append(line)
    if not seen_unreleased:
        raise ReleaseError("CHANGELOG.md has no [Unreleased] reference line")
    if not inserted_target:
        raise ReleaseError(f"CHANGELOG.md has no [{target}] reference line after rewrite")
    out.append("".join(rendered_refs))
    return "".join(out)


# -------------------------
# validation / core workflow
# -------------------------

def verify_release(repo_root: Path, *, repo_url: str | None = None) -> str:
    """Verify the complete, prepared release state and return its version."""
    repo_root = repo_root.resolve()
    cargo_text = read_text(repo_root / "Cargo.toml")
    plugin_text = read_text(repo_root / "herdr-plugin.toml")
    lock_text = read_text(repo_root / "Cargo.lock")
    changelog_text = read_text(repo_root / "CHANGELOG.md")

    version = parse_workspace_version(cargo_text)
    parse_semver(version)
    plugin_version = parse_simple_version_toml(plugin_text)
    lock_versions = parse_lock_versions(lock_text)
    mismatches = []
    if plugin_version != version:
        mismatches.append(f"herdr-plugin.toml={plugin_version}, expected {version}")
    mismatches.extend(
        f"Cargo.lock {name}={package_version}, expected {version}"
        for name, package_version in lock_versions.items()
        if package_version != version
    )
    if mismatches:
        raise ReleaseError("release version mismatch: " + "; ".join(mismatches))

    changelog = analyze_changelog(
        changelog_text,
        version,
        repo_url_from_env_or_git(repo_root, repo_url),
    )
    if not changelog.prepared:
        raise ReleaseError(
            f"CHANGELOG.md is not prepared for v{version}: "
            "expected an empty [Unreleased], a release section, and matching links"
        )
    return version


def plan_release(repo_root: Path, bump: str) -> Plan:
    cargo_text = read_text(repo_root / "Cargo.toml")
    current = parse_workspace_version(cargo_text)
    target = bump_semver(current, bump)
    return Plan(current=current, target=target)


def apply_release(
    repo_root: Path,
    target_version: str,
    *,
    release_date: str | None = None,
    repo_url: str | None = None,
) -> ApplyResult:
    repo_root = repo_root.resolve()
    release_date = release_date or date.today().isoformat()
    repo_url = repo_url_from_env_or_git(repo_root, repo_url)

    cargo_path = repo_root / "Cargo.toml"
    plugin_path = repo_root / "herdr-plugin.toml"
    lock_path = repo_root / "Cargo.lock"
    changelog_path = repo_root / "CHANGELOG.md"

    cargo_text = read_text(cargo_path)
    plugin_text = read_text(plugin_path)
    lock_text = read_text(lock_path)
    changelog_text = read_text(changelog_path)

    current_version = parse_workspace_version(cargo_text)
    if not SEMVER_RE.match(target_version):
        raise ReleaseError(f"invalid target version: {target_version!r}")
    plugin_version = parse_simple_version_toml(plugin_text)
    lock_versions = parse_lock_versions(lock_text)
    changelog = analyze_changelog(changelog_text, target_version, repo_url)

    if current_version == target_version:
        all_target = plugin_version == target_version and all(
            version == target_version for version in lock_versions.values()
        )
        if all_target and changelog.prepared:
            return ApplyResult(
                current=current_version,
                target=target_version,
                changed=False,
                already_prepared=True,
            )

    if current_version != target_version:
        if plugin_version != current_version or any(
            version != current_version for version in lock_versions.values()
        ):
            raise ReleaseError(
                "version mismatch: Cargo.toml, herdr-plugin.toml, and Cargo.lock must all start at the same version"
            )
        if not changelog.unreleased_body.strip():
            raise ReleaseError("CHANGELOG.md [Unreleased] is empty")
        if changelog.target_section_exists:
            raise ReleaseError(f"CHANGELOG.md already contains a v{target_version} section")
        updated_changelog = rewrite_changelog(
            changelog_text, target_version, release_date, repo_url
        )
    elif changelog.target_section_exists:
        # A failed run may have replaced the changelog last. It is safe to
        # retain it only when it is already a complete prepared changelog.
        if not changelog.prepared:
            raise ReleaseError(
                f"CHANGELOG.md has a partial or invalid v{target_version} section"
            )
        updated_changelog = changelog_text
    else:
        # A failed run may have replaced Cargo.toml/plugin/lock first. The
        # original Unreleased body is still enough to finish the changelog.
        if not changelog.unreleased_body.strip():
            raise ReleaseError("CHANGELOG.md [Unreleased] is empty")
        updated_changelog = rewrite_changelog(
            changelog_text, target_version, release_date, repo_url
        )

    updated_cargo = rewrite_workspace_version(cargo_text, target_version)
    updated_plugin = rewrite_simple_version_toml(plugin_text, target_version)
    updated_lock = rewrite_lock_versions(lock_text, target_version)

    # Do not roll back: every replacement is atomic, and keeping a valid
    # partial state makes an interrupted run recoverable by rerunning apply.
    for path, content in (
        (cargo_path, updated_cargo),
        (plugin_path, updated_plugin),
        (lock_path, updated_lock),
        (changelog_path, updated_changelog),
    ):
        write_text(path, content)

    verify_release(repo_root, repo_url=repo_url)
    return ApplyResult(current=current_version, target=target_version, changed=True, already_prepared=False)


# -------------------------
# CLI
# -------------------------

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=".", help="repo root (default: current directory)")
    sub = parser.add_subparsers(dest="cmd", required=True)

    plan = sub.add_parser("plan", help="print the target version for a bump")
    plan.add_argument("--bump", choices=("patch", "minor", "major"), required=True)

    apply = sub.add_parser("apply", help="apply a target version to the release files")
    apply.add_argument("--target-version", required=True)
    apply.add_argument("--date", help="release date in YYYY-MM-DD (default: today)")
    apply.add_argument("--repo-url", help="GitHub repo URL for changelog links")

    verify = sub.add_parser("verify", help="verify synchronized, prepared release files")
    verify.add_argument("--repo-url", help="GitHub repo URL for changelog links")

    return parser


def cmd_plan(args: argparse.Namespace) -> int:
    repo_root = Path(args.repo)
    plan = plan_release(repo_root, args.bump)
    print(json.dumps(plan.__dict__, separators=(",", ":")))
    return 0


def cmd_apply(args: argparse.Namespace) -> int:
    repo_root = Path(args.repo)
    result = apply_release(
        repo_root,
        args.target_version,
        release_date=args.date,
        repo_url=args.repo_url,
    )
    print(json.dumps(result.__dict__, separators=(",", ":")))
    return 0


def cmd_verify(args: argparse.Namespace) -> int:
    version = verify_release(Path(args.repo), repo_url=args.repo_url)
    print(json.dumps({"verified": True, "version": version}, separators=(",", ":")))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.cmd == "plan":
            return cmd_plan(args)
        if args.cmd == "apply":
            return cmd_apply(args)
        if args.cmd == "verify":
            return cmd_verify(args)
        raise AssertionError(f"unknown command: {args.cmd!r}")
    except ReleaseError as exc:
        print(f"prepare-release.py: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
