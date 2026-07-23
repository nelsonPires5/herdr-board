#!/usr/bin/env python3
"""Fail-closed process identity primitives for the provider-free E2E harness."""

from __future__ import annotations

import ctypes
import ctypes.util
import hashlib
import hmac
import json
import os
import platform
import stat
import struct
import sys
from dataclasses import dataclass
from typing import Any

SYSTEM = platform.system()
PLATFORM = {"Linux": "linux", "Darwin": "darwin"}.get(SYSTEM, "unsupported")
OWNER_ENVS = {"E2E_HERDR_OWNER_TOKEN", "E2E_BOARD_DAEMON_OWNER_TOKEN"}
TOKEN_KEYS = {
    "version",
    "platform",
    "proof",
    "parent_pid",
    "pid",
    "start_time",
    "exe",
    "session",
    "name",
    "expected_command",
    "owner_token",
    "cmdline",
    "signature",
}
UNSIGNED_KEYS = TOKEN_KEYS - {"signature"}


class IdentityError(Exception):
    """Identity cannot be captured or verified safely."""


@dataclass(frozen=True)
class Snapshot:
    pid: str
    start_time: str
    parent_pid: str
    state: str
    exe: str
    cmdline: list[str]
    environ: frozenset[bytes] | None


def _canonical(value: dict[str, Any]) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("ascii")


def _read_key() -> bytes:
    try:
        with os.fdopen(3, "rb", closefd=True) as stream:
            key = stream.read().rstrip(b"\n")
    except OSError as exc:
        raise IdentityError("identity signing key unavailable") from exc
    if len(key) < 32:
        raise IdentityError("identity signing key too short")
    return key


def sign(unsigned: dict[str, Any], key: bytes) -> dict[str, Any]:
    if set(unsigned) != UNSIGNED_KEYS:
        raise IdentityError("invalid unsigned identity fields")
    result = dict(unsigned)
    result["signature"] = hmac.new(key, _canonical(unsigned), hashlib.sha256).hexdigest()
    validate_token(result, key)
    return result


def validate_token(token: object, key: bytes) -> dict[str, Any]:
    if not isinstance(token, dict) or set(token) != TOKEN_KEYS:
        raise IdentityError("invalid identity token fields")
    if token.get("version") != 2 or token.get("platform") != PLATFORM:
        raise IdentityError("invalid identity token platform/version")
    allowed_proofs = {
        "linux-environment-direct-child",
        "linux-environment-transition",
        "darwin-direct-child",
        "darwin-direct-child-transition",
    }
    if token.get("proof") not in allowed_proofs or not str(token["proof"]).startswith(PLATFORM):
        raise IdentityError("invalid identity proof mode")
    scalar = UNSIGNED_KEYS - {"version", "cmdline"}
    if not all(isinstance(token.get(item), str) for item in scalar):
        raise IdentityError("invalid identity token scalars")
    if not isinstance(token.get("cmdline"), list) or not all(
        isinstance(item, str) for item in token["cmdline"]
    ):
        raise IdentityError("invalid identity argv")
    if not token["pid"].isdigit() or not token["parent_pid"].isdigit():
        raise IdentityError("invalid identity pid")
    signature = token.get("signature")
    if not isinstance(signature, str) or len(signature) != 64 or any(
        char not in "0123456789abcdef" for char in signature
    ):
        raise IdentityError("invalid identity signature")
    unsigned = {key_name: token[key_name] for key_name in UNSIGNED_KEYS}
    expected = hmac.new(key, _canonical(unsigned), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(signature, expected):
        raise IdentityError("identity signature mismatch")
    return token


def _linux_snapshot(pid: int) -> Snapshot:
    try:
        raw_stat = open(f"/proc/{pid}/stat", encoding="utf-8").read()
        fields = raw_stat[raw_stat.rfind(")") + 2 :].split()
        cmdline = [
            value.decode("utf-8", "surrogateescape")
            for value in open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")
            if value
        ]
        environ = frozenset(open(f"/proc/{pid}/environ", "rb").read().split(b"\0"))
        exe = os.readlink(f"/proc/{pid}/exe")
        return Snapshot(
            str(pid), fields[19], fields[1], fields[0], exe, cmdline, environ
        )
    except (IndexError, OSError, UnicodeError) as exc:
        raise IdentityError("cannot inspect Linux process") from exc


if SYSTEM == "Darwin":
    _libproc = ctypes.CDLL(ctypes.util.find_library("proc"), use_errno=True)
    _libc = ctypes.CDLL(ctypes.util.find_library("System"), use_errno=True)
    _libproc.proc_pidinfo.argtypes = [
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_uint64,
        ctypes.c_void_p,
        ctypes.c_int,
    ]
    _libproc.proc_pidinfo.restype = ctypes.c_int
    _libproc.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    _libproc.proc_pidpath.restype = ctypes.c_int
    _libc.sysctl.argtypes = [
        ctypes.POINTER(ctypes.c_int),
        ctypes.c_uint,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_size_t),
        ctypes.c_void_p,
        ctypes.c_size_t,
    ]
    _libc.sysctl.restype = ctypes.c_int

    class _ProcBsdInfo(ctypes.Structure):
        _fields_ = [
            ("pbi_flags", ctypes.c_uint32),
            ("pbi_status", ctypes.c_uint32),
            ("pbi_xstatus", ctypes.c_uint32),
            ("pbi_pid", ctypes.c_uint32),
            ("pbi_ppid", ctypes.c_uint32),
            ("pbi_uid", ctypes.c_uint32),
            ("pbi_gid", ctypes.c_uint32),
            ("pbi_ruid", ctypes.c_uint32),
            ("pbi_rgid", ctypes.c_uint32),
            ("pbi_svuid", ctypes.c_uint32),
            ("pbi_svgid", ctypes.c_uint32),
            ("rfu_1", ctypes.c_uint32),
            ("pbi_comm", ctypes.c_char * 16),
            ("pbi_name", ctypes.c_char * 32),
            ("pbi_nfiles", ctypes.c_uint32),
            ("pbi_pgid", ctypes.c_uint32),
            ("pbi_pjobc", ctypes.c_uint32),
            ("e_tdev", ctypes.c_uint32),
            ("e_tpgid", ctypes.c_uint32),
            ("pbi_nice", ctypes.c_int32),
            ("pbi_start_tvsec", ctypes.c_uint64),
            ("pbi_start_tvusec", ctypes.c_uint64),
        ]


def _darwin_info(pid: int) -> _ProcBsdInfo:
    info = _ProcBsdInfo()
    size = ctypes.sizeof(info)
    result = _libproc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), size)
    if result != size or info.pbi_pid != pid:
        raise IdentityError("cannot inspect Darwin process")
    return info


def _darwin_exe(pid: int) -> str:
    buffer = ctypes.create_string_buffer(4096)
    result = _libproc.proc_pidpath(pid, buffer, len(buffer))
    if result <= 0 or not buffer.value:
        raise IdentityError("cannot inspect Darwin executable")
    return os.path.realpath(buffer.value.decode("utf-8", "surrogateescape"))


def _darwin_argv(pid: int) -> list[str]:
    mib = (ctypes.c_int * 3)(1, 49, pid)
    size = ctypes.c_size_t(0)
    if _libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) != 0 or size.value < 5:
        raise IdentityError("cannot size Darwin argv")
    buffer = ctypes.create_string_buffer(size.value)
    actual = ctypes.c_size_t(size.value)
    if _libc.sysctl(mib, 3, buffer, ctypes.byref(actual), None, 0) != 0:
        raise IdentityError("cannot read Darwin argv")
    data = buffer.raw[: actual.value]
    argc = struct.unpack_from("=i", data, 0)[0]
    if argc <= 0 or argc > 4096:
        raise IdentityError("invalid Darwin argc")
    offset = 4
    executable_end = data.find(b"\0", offset)
    if executable_end < 0:
        raise IdentityError("truncated Darwin executable path")
    offset = executable_end + 1
    while offset < len(data) and data[offset] == 0:
        offset += 1
    argv: list[str] = []
    for _ in range(argc):
        end = data.find(b"\0", offset)
        if end < 0:
            raise IdentityError("truncated Darwin argv")
        argv.append(data[offset:end].decode("utf-8", "surrogateescape"))
        offset = end + 1
    if len(argv) != argc or not argv[0]:
        raise IdentityError("invalid Darwin argv")
    return argv


def _darwin_snapshot(pid: int) -> Snapshot:
    before = _darwin_info(pid)
    start = f"{before.pbi_start_tvsec}:{before.pbi_start_tvusec}"
    argv = _darwin_argv(pid)
    exe = _darwin_exe(pid)
    after = _darwin_info(pid)
    after_start = f"{after.pbi_start_tvsec}:{after.pbi_start_tvusec}"
    if before.pbi_pid != after.pbi_pid or start != after_start:
        raise IdentityError("Darwin process changed during inspection")
    states = {1: "I", 2: "R", 3: "S", 4: "T", 5: "Z"}
    return Snapshot(
        str(pid),
        start,
        str(after.pbi_ppid),
        states.get(after.pbi_status, "?"),
        exe,
        argv,
        None,
    )


def snapshot(pid: int) -> Snapshot:
    if PLATFORM == "linux":
        return _linux_snapshot(pid)
    if PLATFORM == "darwin":
        return _darwin_snapshot(pid)
    raise IdentityError("unsupported E2E platform")


def process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except (OSError, ValueError):
        return False


def _owner_present(current: Snapshot, owner_env: str, owner_token: str) -> bool:
    if owner_env not in OWNER_ENVS or not owner_token:
        return False
    if PLATFORM == "linux":
        assert current.environ is not None
        return f"{owner_env}={owner_token}".encode() in current.environ
    return True


def _semantic_argv(
    current: Snapshot, session: str, name: str, expected_command: str, owner_token: str
) -> bool:
    if expected_command and session == name and owner_token:
        return current.cmdline == [expected_command, "--session", session, "server"]
    if expected_command and session == "daemon" and name == "--foreground" and owner_token:
        return current.cmdline == [expected_command, "daemon", "--foreground"]
    expected_ok = (
        not expected_command
        or expected_command in current.cmdline[:2]
        # Framework launchers on Darwin re-exec while preserving PID/start/PPID;
        # the signed direct-child capability authorizes that transition.
        or PLATFORM == "darwin"
    )
    return session in current.cmdline and name in current.cmdline and expected_ok


def _unsigned(
    current: Snapshot,
    proof: str,
    session: str,
    name: str,
    expected_command: str,
    owner_token: str,
) -> dict[str, Any]:
    return {
        "version": 2,
        "platform": PLATFORM,
        "proof": proof,
        "parent_pid": current.parent_pid,
        "pid": current.pid,
        "start_time": current.start_time,
        "exe": current.exe,
        "session": session,
        "name": name,
        "expected_command": expected_command,
        "owner_token": owner_token,
        "cmdline": current.cmdline,
    }


def provisional_capture(
    pid: int, owner_token: str, parent_pid: str, owner_env: str, key: bytes
) -> dict[str, Any]:
    current = snapshot(pid)
    if current.parent_pid != parent_pid or not current.cmdline:
        raise IdentityError("process is not the exact direct child")
    if not _owner_present(current, owner_env, owner_token):
        raise IdentityError("process owner evidence mismatch")
    proof = "linux-environment-direct-child" if PLATFORM == "linux" else "darwin-direct-child"
    return sign(_unsigned(current, proof, "", "", "", owner_token), key)


def stable_capture(
    pid: int,
    session: str,
    name: str,
    expected_command: str,
    owner_token: str,
    owner_env: str,
    provisional: object,
    key: bytes,
) -> dict[str, Any]:
    capability = validate_token(provisional, key)
    if capability["proof"] not in {
        "linux-environment-direct-child",
        "darwin-direct-child",
    }:
        raise IdentityError("stable capture lacks provisional capability")
    current = snapshot(pid)
    if (
        capability["pid"] != current.pid
        or capability["start_time"] != current.start_time
        or capability["parent_pid"] != current.parent_pid
        or capability["owner_token"] != owner_token
    ):
        raise IdentityError("stable process differs from provisional child")
    if not _owner_present(current, owner_env, owner_token):
        raise IdentityError("stable owner evidence mismatch")
    if not _semantic_argv(current, session, name, expected_command, owner_token):
        raise IdentityError("stable argv mismatch")
    proof = "linux-environment-transition" if PLATFORM == "linux" else "darwin-direct-child-transition"
    return sign(
        _unsigned(current, proof, session, name, expected_command, owner_token), key
    )


def verify_identity(
    pid: int, token: object, key: bytes, *, audit: bool = False
) -> bool:
    try:
        recorded = validate_token(token, key)
        if recorded["pid"] != str(pid):
            return False
        current = snapshot(pid)
        if (
            current.start_time != recorded["start_time"]
            or current.exe != recorded["exe"]
            or current.cmdline != recorded["cmdline"]
        ):
            return False
        if not audit and current.parent_pid != recorded["parent_pid"]:
            return False
        if recorded["proof"].startswith("linux-"):
            owner_env = (
                "E2E_BOARD_DAEMON_OWNER_TOKEN"
                if recorded["session"] == "daemon" and recorded["name"] == "--foreground"
                else "E2E_HERDR_OWNER_TOKEN"
            )
            if not _owner_present(current, owner_env, recorded["owner_token"]):
                return False
        if recorded["session"] or recorded["name"]:
            if not _semantic_argv(
                current,
                recorded["session"],
                recorded["name"],
                recorded["expected_command"],
                recorded["owner_token"],
            ):
                return False
        return True
    except (IdentityError, TypeError, ValueError):
        return False


def provisional_transition_verify(
    pid: int,
    token: object,
    parent_pid: str,
    expected_command: str,
    name: str,
    transition: str,
    owner_env: str,
    key: bytes,
) -> bool:
    try:
        recorded = validate_token(token, key)
        if recorded["proof"] not in {
            "linux-environment-direct-child",
            "darwin-direct-child",
        }:
            return False
        current = snapshot(pid)
        if (
            current.pid != recorded["pid"]
            or current.start_time != recorded["start_time"]
            or current.parent_pid != parent_pid
            or current.parent_pid != recorded["parent_pid"]
            or not _owner_present(current, owner_env, recorded["owner_token"])
        ):
            return False
        original = current.exe == recorded["exe"] and current.cmdline == recorded["cmdline"]
        if transition == "daemon":
            transitioned = current.exe == os.path.realpath(expected_command) and current.cmdline == [
                expected_command,
                "daemon",
                "--foreground",
            ]
        else:
            transitioned = (
                bool(expected_command and name)
                and current.exe == os.path.realpath(expected_command)
                and current.cmdline == [expected_command, "--session", name, "server"]
            )
        return original or transitioned
    except (IdentityError, TypeError, ValueError):
        return False


def _json_argument(value: str) -> object:
    try:
        return json.loads(value)
    except json.JSONDecodeError as exc:
        raise IdentityError("invalid identity JSON") from exc


def _emit(value: object) -> None:
    print(json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True))


def main() -> int:
    if len(sys.argv) < 2:
        return 2
    command = sys.argv[1]
    try:
        if command == "mode":
            print(f"{stat.S_IMODE(os.stat(sys.argv[2]).st_mode):o}")
            return 0
        if command == "realpath":
            print(os.path.realpath(sys.argv[2]))
            return 0
        if command == "exists":
            return 0 if process_exists(int(sys.argv[2])) else 1
        if command == "state":
            print(snapshot(int(sys.argv[2])).state)
            return 0
        if command == "snapshot":
            current = snapshot(int(sys.argv[2]))
            _emit(
                {
                    "pid": current.pid,
                    "start_time": current.start_time,
                    "parent_pid": current.parent_pid,
                    "state": current.state,
                    "exe": current.exe,
                    "cmdline": current.cmdline,
                }
            )
            return 0
        key = _read_key()
        if command == "provisional-capture":
            pid, owner, parent, owner_env = sys.argv[2:6]
            _emit(provisional_capture(int(pid), owner, parent, owner_env, key))
            return 0
        if command == "stable-capture":
            pid, session, name, expected, owner, owner_env, raw = sys.argv[2:9]
            _emit(
                stable_capture(
                    int(pid),
                    session,
                    name,
                    expected,
                    owner,
                    owner_env,
                    _json_argument(raw),
                    key,
                )
            )
            return 0
        if command == "validate":
            validate_token(_json_argument(sys.argv[2]), key)
            return 0
        if command in {"verify", "audit"}:
            pid, raw = sys.argv[2:4]
            return 0 if verify_identity(
                int(pid), _json_argument(raw), key, audit=command == "audit"
            ) else 1
        if command == "transition-verify":
            pid, raw, parent, expected, name, transition, owner_env = sys.argv[2:9]
            return 0 if provisional_transition_verify(
                int(pid),
                _json_argument(raw),
                parent,
                expected,
                name,
                transition,
                owner_env,
                key,
            ) else 1
        if command == "sign":
            unsigned = _json_argument(sys.argv[2])
            if not isinstance(unsigned, dict):
                raise IdentityError("unsigned token is not an object")
            _emit(sign(unsigned, key))
            return 0
        return 2
    except (IdentityError, IndexError, OSError, TypeError, ValueError):
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
