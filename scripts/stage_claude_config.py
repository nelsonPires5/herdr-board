#!/usr/bin/env python3
"""Stage the minimal Claude state needed by the real-provider smoke test."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import stat
import sys
from pathlib import Path


class StageError(Exception):
    pass


def regular_file(path: Path, label: str) -> None:
    try:
        mode = path.lstat().st_mode
    except OSError as exc:
        raise StageError(f"missing {label}: {path}") from exc
    if not stat.S_ISREG(mode):
        raise StageError(f"{label} is not a regular file: {path}")


def validate_herdr_hook(path: Path, source: Path) -> None:
    try:
        mode = path.lstat().st_mode
    except OSError as exc:
        raise StageError(f"missing Herdr hook: {path}") from exc
    if not stat.S_ISREG(mode):
        raise StageError(f"Herdr hook is not a non-symlink regular file: {path}")
    try:
        canonical = path.resolve(strict=True)
    except OSError as exc:
        raise StageError(f"could not canonicalize Herdr hook: {path}") from exc
    expected_parent = source / "hooks"
    if canonical.parent != expected_parent:
        raise StageError(
            f"Herdr hook must be under the source hooks directory: {path}"
        )


def write_private(path: Path, data: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    try:
        fd = os.open(path, flags, 0o600)
    except OSError as exc:
        raise StageError(f"could not create staged file: {path}") from exc
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
        os.chmod(path, 0o600)
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def stage(source_dir: Path, workspace_dir: Path, staged_dir: Path) -> Path:
    if staged_dir.is_symlink():
        raise StageError(f"staged config directory must not be a symlink: {staged_dir}")
    try:
        source = source_dir.resolve(strict=True)
        workspace = workspace_dir.resolve(strict=True)
        staged = staged_dir.resolve(strict=True)
    except OSError as exc:
        raise StageError(f"could not canonicalize configuration paths: {exc}") from exc
    if not source.is_dir():
        raise StageError(f"source config directory is not a directory: {source}")
    if not workspace.is_dir():
        raise StageError(f"workspace directory is not a directory: {workspace}")
    if not staged.is_dir():
        raise StageError(f"staged config directory is not a directory: {staged}")
    try:
        if any(staged.iterdir()):
            raise StageError(f"staged config directory is not empty: {staged}")
        os.chmod(staged, 0o700)
    except OSError as exc:
        raise StageError(f"could not inspect staged config directory: {staged}") from exc

    credentials_path = source / ".credentials.json"
    remote_settings_path = source / "remote-settings.json"
    settings_path = source / "settings.json"
    for path, label in (
        (credentials_path, "Claude credentials"),
        (remote_settings_path, "approved remote settings"),
        (settings_path, "Claude settings"),
    ):
        regular_file(path, label)

    try:
        settings = json.loads(settings_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise StageError(f"Claude settings are not valid UTF-8 JSON: {settings_path}") from exc
    if not isinstance(settings, dict):
        raise StageError("Claude settings root must be an object")
    if settings.get("skipDangerousModePermissionPrompt") is not True:
        raise StageError("Claude settings must acknowledge dangerous permission mode")

    hooks = settings.get("hooks")
    session_start = hooks.get("SessionStart") if isinstance(hooks, dict) else None
    if not isinstance(session_start, list):
        raise StageError("Claude settings have no SessionStart hook list")

    matches: list[tuple[dict[str, object], object]] = []
    for group in session_start:
        if not isinstance(group, dict) or not isinstance(group.get("hooks"), list):
            continue
        for hook in group["hooks"]:
            if not isinstance(hook, dict) or hook.get("type") != "command":
                continue
            command = hook.get("command")
            if not isinstance(command, str):
                continue
            try:
                words = shlex.split(command)
            except ValueError as exc:
                raise StageError("Claude SessionStart hook command is not valid shell syntax") from exc
            script_words = [word for word in words if word.endswith("/herdr-agent-state.sh")]
            if not script_words:
                continue
            if len(script_words) != 1 or not os.path.isabs(script_words[0]):
                raise StageError(
                    "Herdr SessionStart hook path must be exactly one absolute script path"
                )
            script = Path(script_words[0])
            validate_herdr_hook(script, source)
            matches.append((hook, group))

    if len(matches) != 1:
        raise StageError(
            f"expected exactly one Herdr SessionStart command, found {len(matches)}"
        )
    hook, group = matches[0]
    command = hook["command"]
    assert isinstance(command, str)
    script_words = [word for word in shlex.split(command) if word.endswith("/herdr-agent-state.sh")]
    hook_path = Path(script_words[0])

    clean_group: dict[str, object] = {"hooks": [{"type": "command", "command": command}]}
    if isinstance(group.get("matcher"), str):
        clean_group = {"matcher": group["matcher"], **clean_group}
    clean_settings = {
        "theme": "dark",
        "skipDangerousModePermissionPrompt": True,
        "hooks": {"SessionStart": [clean_group]},
    }
    files = {
        ".credentials.json": credentials_path.read_bytes(),
        "remote-settings.json": remote_settings_path.read_bytes(),
        "settings.json": (json.dumps(clean_settings, indent=2) + "\n").encode("utf-8"),
        ".claude.json": (
            json.dumps(
                {
                    "hasCompletedOnboarding": True,
                    "projects": {
                        str(workspace): {
                            "hasTrustDialogAccepted": True,
                            "hasClaudeMdExternalIncludesApproved": False,
                        }
                    },
                },
                indent=2,
            )
            + "\n"
        ).encode("utf-8"),
    }
    created: list[Path] = []
    try:
        for name, data in files.items():
            path = staged / name
            write_private(path, data)
            created.append(path)
        for path in staged.iterdir():
            if not path.is_file() or path.is_symlink():
                raise StageError(f"unexpected staged entry: {path}")
            os.chmod(path, 0o600)
    except (OSError, StageError) as exc:
        for path in created:
            path.unlink(missing_ok=True)
        if isinstance(exc, OSError):
            raise StageError(f"could not finish staged Claude config: {exc}") from exc
        raise
    return hook_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-config-dir", required=True, type=Path)
    parser.add_argument("--workspace-dir", required=True, type=Path)
    parser.add_argument("--staged-config-dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        hook_path = stage(args.source_config_dir, args.workspace_dir, args.staged_config_dir)
    except (StageError, OSError) as exc:
        print(f"stage_claude_config: {exc}", file=sys.stderr)
        return 1
    print(json.dumps({"hook_path": str(hook_path)}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
