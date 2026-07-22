#!/usr/bin/env python3
"""No-provider terminal shim for protocol-17 managed-agent E2E fixtures.

The real Herdr `agent.prompt` call writes the card task to the managed process's
terminal.  This helper keeps that terminal interactive, captures the bytes from
stdin, normalizes only terminal transport framing, and appends the evidence to
the fake harness's JSON record.  An absent prompt is a hard fixture failure.
"""

import json
import os
import select
import subprocess
import sys
import termios
import time
import tty


def update(path: str, **fields: object) -> None:
    with open(path, encoding="utf-8") as source:
        record = json.load(source)
    record.update(fields)
    temporary = path + ".tmp"
    with open(temporary, "w", encoding="utf-8") as target:
        json.dump(record, target, ensure_ascii=False, indent=2)
        target.write("\n")
    os.replace(temporary, path)


def normalize_terminal_prompt(raw: bytes) -> str:
    # Herdr may use bracketed paste for multiline text and the PTY represents
    # Enter as CR. Those are transport framing, not part of the card prompt.
    raw = raw.replace(b"\x1b[200~", b"").replace(b"\x1b[201~", b"")
    text = raw.decode("utf-8", errors="strict").replace("\r\n", "\n").replace("\r", "\n")
    if text.endswith("\n"):
        text = text[:-1]
    return text


def main() -> int:
    if len(sys.argv) != 2:
        print("managed terminal shim: expected RECORD path", file=sys.stderr)
        return 2
    record_path = sys.argv[1]
    is_tty = sys.stdin.isatty()
    update(record_path, stdin_isatty=is_tty)
    if not is_tty:
        update(record_path, prompt_error="stdin is not an interactive tty")
        print("managed terminal shim: stdin is not an interactive tty", file=sys.stderr)
        return 2

    fd = sys.stdin.fileno()
    old_attributes = termios.tcgetattr(fd)
    timeout = float(os.environ.get("FAKE_MANAGED_PROMPT_TIMEOUT", "35"))
    idle = float(os.environ.get("FAKE_MANAGED_PROMPT_IDLE", "0.75"))
    deadline = time.monotonic() + timeout
    data = bytearray()

    # This visible marker plus the harness's pane report makes the executable as
    # readiness-capable as a provider-free terminal process can be. The E2E does
    # not treat the marker itself as proof: it requires bytes delivered here.
    print("HERDR_FAKE_MANAGED_INTERACTIVE_READY", flush=True)
    try:
        tty.setraw(fd)
        while True:
            remaining = (idle if data else deadline - time.monotonic())
            if remaining <= 0:
                break
            readable, _, _ = select.select([fd], [], [], remaining)
            if not readable:
                break
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            data.extend(chunk)
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_attributes)

    if not data:
        update(record_path, prompt_error=f"no agent.prompt bytes within {timeout:g}s")
        print(
            f"managed terminal shim: no agent.prompt bytes within {timeout:g}s",
            file=sys.stderr,
        )
        return 2

    try:
        prompt = normalize_terminal_prompt(bytes(data))
    except UnicodeDecodeError as error:
        update(record_path, prompt_raw_hex=bytes(data).hex(), prompt_error=str(error))
        print(f"managed terminal shim: prompt was not UTF-8: {error}", file=sys.stderr)
        return 2
    if not prompt:
        update(record_path, prompt_raw_hex=bytes(data).hex(), prompt_error="normalized prompt empty")
        print("managed terminal shim: normalized prompt is empty", file=sys.stderr)
        return 2

    update(
        record_path,
        prompt=prompt,
        prompt_raw_hex=bytes(data).hex(),
        prompt_received_via_stdin=True,
    )

    # Never let the fake harness reach board done merely because some terminal
    # bytes arrived. Wait until the daemon has committed this exact run, then
    # compare against its authoritative prompt_snapshot first.
    board_bin = os.environ.get("BOARD_BIN", "")
    card_id = os.environ.get("BOARD_CARD_ID", "")
    run_id = int(os.environ.get("BOARD_RUN_ID", "0"))
    verify_deadline = time.monotonic() + 10.0
    expected = None
    while board_bin and card_id and time.monotonic() < verify_deadline:
        result = subprocess.run(
            [board_bin, "card", "show", card_id, "--json"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=False,
        )
        if result.returncode == 0:
            try:
                show = json.loads(result.stdout)
                run = next((item for item in show.get("runs", []) if item.get("id") == run_id), None)
                if run is not None:
                    expected = run.get("prompt_snapshot")
                    break
            except (json.JSONDecodeError, TypeError):
                pass
        time.sleep(0.1)
    if expected is None:
        update(record_path, prompt_error="run prompt_snapshot was not committed within 10s")
        print("managed terminal shim: authoritative run prompt unavailable", file=sys.stderr)
        return 2
    if prompt != expected:
        update(record_path, expected_prompt=expected, prompt_error="agent.prompt did not match run snapshot")
        print("managed terminal shim: agent.prompt did not match run snapshot", file=sys.stderr)
        return 2
    update(record_path, prompt_matches_run_snapshot=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
