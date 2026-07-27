"""Shared plumbing for the `scripts/tests` suite.

The Python tier is a CI gate (`.github/workflows/ci.yml` → `Python tests`) made
of independent contract suites. They all need the same three things — the repo
root, a way to drop an executable stub into a temp directory, and a way to run a
bash snippet with a controlled environment — which used to be recomputed and
re-implemented in each file under two different names (`ROOT` / `REPO_ROOT`).

This module is deliberately dependency-free and is not collected by the
`test_*.py` discovery pattern.
"""
from __future__ import annotations

import os
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Iterable, Mapping

# scripts/tests/_support.py -> scripts/tests -> scripts -> <repo root>
REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_DIR = REPO_ROOT / "scripts"
E2E_DIR = REPO_ROOT / "e2e"

# Absolute path to the interpreter running the suite. Stubs use it in their
# shebang so they still work inside an isolated PATH that lacks /usr/bin.
PYTHON3 = sys.executable

SH_SHEBANG = "#!/bin/sh"
PYTHON_SHEBANG = f"#!{PYTHON3}"


def write_executable(
    directory: Path,
    name: str,
    body: str,
    *,
    shebang: str = SH_SHEBANG,
    mode: int = 0o755,
    replace: bool = False,
) -> Path:
    """Write `directory/name` as an executable script and return its path.

    `replace=True` unlinks any existing entry first, which is what lets a test
    overwrite a symlink it planted earlier.
    """
    path = Path(directory) / name
    if replace:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
    path.write_text(f"{shebang}\n{body}", encoding="utf-8")
    path.chmod(mode)
    return path


def run_bash(
    script: str,
    *,
    env: Mapping[str, str],
    timeout: float = 5,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run one dedented bash snippet, capturing output and never raising."""
    return subprocess.run(
        ["bash", "-c", textwrap.dedent(script)],
        env=dict(env),
        cwd=None if cwd is None else str(cwd),
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout,
    )


def clean_env(*, drop: Iterable[str] = (), **overrides: str) -> dict[str, str]:
    """A copy of the process environment with `drop` removed and `overrides` set.

    Callers use it so a developer's own E2E session or managed-pane variables
    cannot leak into a harness under test.
    """
    env = os.environ.copy()
    for key in drop:
        env.pop(key, None)
    env.update(overrides)
    return env
