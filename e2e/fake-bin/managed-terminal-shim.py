#!/usr/bin/env python3
"""No-provider terminal shim for protocol-19 managed-agent E2E fixtures.

The real Herdr `agent.prompt` call writes the card task to the managed process's
terminal.  This helper keeps that terminal interactive, captures the bytes from
stdin, normalizes only terminal transport framing, and appends the evidence to
the fake harness's JSON record.  An absent prompt is a hard fixture failure.
"""

import json
import os
import select
import socket
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


def report_agent_state(record_path: str, state: str) -> None:
    pane_id = os.environ["HERDR_PANE_ID"]
    socket_path = os.environ["HERDR_SOCKET_PATH"]
    agent = os.environ.get("FAKE_MANAGED_KIND", "pi")
    with open(record_path, encoding="utf-8") as source:
        record = json.load(source)
    params = {
        "pane_id": pane_id,
        "source": f"herdr:{agent}",
        "agent": agent,
        "seq": time.time_ns(),
        "state": state,
    }
    if record.get("agent_session_id"):
        params["agent_session_id"] = record["agent_session_id"]
    if record.get("agent_session_path"):
        params["agent_session_path"] = record["agent_session_path"]
    envelope = {
        "id": f"e2e:turn:{agent}:{os.getpid()}:{state}",
        "method": "pane.report_agent",
        "params": params,
    }
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(2.0)
    try:
        client.connect(socket_path)
        client.sendall((json.dumps(envelope, separators=(",", ":")) + "\n").encode())
        chunks = bytearray()
        while b"\n" not in chunks:
            chunk = client.recv(65536)
            if not chunk:
                break
            chunks.extend(chunk)
    finally:
        client.close()
    line = bytes(chunks).splitlines()[0] if chunks else b""
    reply = json.loads(line) if line else {}
    if "error" in reply or "result" not in reply:
        raise RuntimeError(f"pane.report_agent {state} failed: {reply!r}")


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
    multi_turn = os.environ.get("FAKE_PI_LOOP", "0") == "1"
    timeout = float(os.environ.get("FAKE_MANAGED_PROMPT_TIMEOUT", "35"))
    idle = float(os.environ.get("FAKE_MANAGED_PROMPT_IDLE", "0.75"))
    deadline = time.monotonic() + timeout
    prompt = None
    prompt_raw = b""
    saw_transport_bytes = False

    # This visible marker plus the harness's pane report makes the executable as
    # readiness-capable as a provider-free terminal process can be. The E2E does
    # not treat the marker itself as proof: it requires bytes delivered here.
    print("HERDR_FAKE_MANAGED_INTERACTIVE_READY", flush=True)
    try:
        # TCSADRAIN (not the tty.setraw default TCSAFLUSH): a second prompt can
        # arrive while this shim is still starting (the daemon's reuse hop races
        # the loop's next shim), and TCSAFLUSH would DISCARD those pending bytes
        # before select ever sees them. Draining output only keeps the queue.
        tty.setraw(fd, when=termios.TCSADRAIN)
        if multi_turn:
            try:
                # A real managed client is only idle once its input loop is
                # ready. Multi-turn reuse needs this explicit lifecycle so the
                # next prompt cannot race the prior stage's still-foreground
                # `board done` subprocess. Single-turn fixtures retain their
                # historical integration-driven lifecycle.
                report_agent_state(record_path, "idle")
            except Exception as error:
                update(record_path, prompt_error=f"idle report failed: {error}")
                print(f"managed terminal shim: idle report failed: {error}", file=sys.stderr)
                return 2
        while prompt is None and time.monotonic() < deadline:
            data = bytearray()
            while True:
                remaining = idle if data else deadline - time.monotonic()
                if remaining <= 0:
                    break
                readable, _, _ = select.select([fd], [], [], remaining)
                if not readable:
                    break
                chunk = os.read(fd, 65536)
                if not chunk:
                    break
                data.extend(chunk)
            if not data:
                break
            saw_transport_bytes = True
            prompt_raw = bytes(data)
            try:
                candidate = normalize_terminal_prompt(prompt_raw)
            except UnicodeDecodeError as error:
                update(record_path, prompt_raw_hex=prompt_raw.hex(), prompt_error=str(error))
                print(f"managed terminal shim: prompt was not UTF-8: {error}", file=sys.stderr)
                return 2
            # A subsequent turn can begin with a residual Enter/framing batch
            # from the prior prompt. Ignore transport-only empty batches and
            # keep waiting for this turn's substantive agent.prompt payload.
            if candidate:
                prompt = candidate
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_attributes)

    if prompt is None:
        reason = (
            "only empty terminal framing received"
            if saw_transport_bytes
            else f"no agent.prompt bytes within {timeout:g}s"
        )
        fields: dict[str, object] = {"prompt_error": reason}
        if saw_transport_bytes:
            fields["prompt_raw_hex"] = prompt_raw.hex()
        update(record_path, **fields)
        print(f"managed terminal shim: {reason}", file=sys.stderr)
        return 2

    update(
        record_path,
        prompt=prompt,
        prompt_raw_hex=prompt_raw.hex(),
        prompt_received_via_stdin=True,
    )
    if multi_turn:
        try:
            report_agent_state(record_path, "working")
        except Exception as error:
            update(record_path, prompt_error=f"working report failed: {error}")
            print(f"managed terminal shim: working report failed: {error}", file=sys.stderr)
            return 2

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
                runs = show.get("runs", [])
                # A reused pane keeps the process's original BOARD_RUN_ID. The
                # authoritative target for each later agent.prompt is therefore
                # the card's sole open run; use the startup id only as fallback
                # while the first launch is being committed.
                open_runs = [
                    item for item in runs if item.get("outcome") in (None, "null")
                ]
                run = open_runs[-1] if open_runs else next(
                    (item for item in runs if item.get("id") == run_id), None
                )
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
    if prompt == expected:
        update(record_path, prompt_matches_run_snapshot=True)
        return 0
    # A codex/opencode/agy Mint receives ONE delimited block: `## herdr-board
    # system instructions` + non-empty system text + `## herdr-board card
    # task` + the exact run snapshot (the opencode and agy adapters share the
    # codex delimiter convention). The system half is private DB state (never
    # exposed by `card show`), so the fixture pins the block structure and
    # the exact task half here, records the system half verbatim, and the
    # scenario reconstructs the full block from its own sources. Resume,
    # fork, and same-pane reuse deliver the task alone and matched above.
    mint_block = None
    if os.environ.get("FAKE_MANAGED_KIND", "pi") in ("codex", "opencode", "agy"):
        prefix = "## herdr-board system instructions\n"
        task_delim = "\n\n## herdr-board card task\n"
        if prompt.startswith(prefix) and task_delim in prompt[len(prefix):]:
            system_half = prompt[len(prefix):].split(task_delim, 1)[0]
            if system_half and prompt == prefix + system_half + task_delim + expected:
                update(record_path, prompt_matches_mint_block=True, mint_system_half=system_half)
                return 0
    update(record_path, expected_prompt=expected, prompt_error="agent.prompt did not match run snapshot")
    print("managed terminal shim: agent.prompt did not match run snapshot", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
