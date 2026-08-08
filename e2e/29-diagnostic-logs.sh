#!/usr/bin/env bash
# 29-diagnostic-logs.sh — private daily NDJSON, retention, metadata, and redaction.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

export E2E_FAKE_ENV="FAKE_DIAGNOSTIC_CREDENTIAL=credential-sentinel-4ab973"
# Debug must not re-enable socket/path fields excluded by the diagnostic policy.
export RUST_LOG=debug
e2e_init
e2e_build
e2e_isolate

step "Create retention fixtures before boardd starts"
LOG_DIR="$E2E_TMP/logs"
export BOARD_LOG_DIR="$LOG_DIR"
mkdir -p "$LOG_DIR"
chmod 700 "$LOG_DIR"
python3 - "$LOG_DIR" <<'PY'
from datetime import date, timedelta
from pathlib import Path
import os, sys, time
root = Path(sys.argv[1])
today = date.today()
now = time.time()
# Retention is mtime-based. Give the live startup ample margin around the exact
# boundary; the injected Rust test pins equality to the second.
for age, mtime_age, label in [
    (31, 31 * 86400, "expired"),
    (30, 30 * 86400 - 60, "boundary"),
    (29, 29 * 86400, "recent"),
]:
    path = root / f"daemon.{today-timedelta(days=age):%Y-%m-%d}.ndjson"
    path.write_text("{}\n")
    os.chmod(path, 0o600)
    os.utime(path, (now - mtime_age, now - mtime_age))
    (root / label).write_text(str(path))
unrelated = root / "application.log"
unrelated.write_text("unrelated\n")
(root / "daemon.bad-date.ndjson").write_text("{}\n")
(root / "daemon.+123-01-01.ndjson").write_text("{}\n")
(root / "daemon.-123-01-01.ndjson").write_text("{}\n")
(root / "daemon.2x23-01-01.ndjson").write_text("{}\n")
(root / "daemon.1990-01-01.ndjson.directory").mkdir()
os.symlink(unrelated, root / "daemon.1990-01-01.ndjson")
PY
EXPIRED="$(cat "$LOG_DIR/expired")"
BOUNDARY="$(cat "$LOG_DIR/boundary")"
RECENT="$(cat "$LOG_DIR/recent")"
rm "$LOG_DIR/expired" "$LOG_DIR/boundary" "$LOG_DIR/recent"

e2e_daemon_start
[ ! -e "$EXPIRED" ] || fail "expired exact-owned log was not pruned"
[ -f "$BOUNDARY" ] || fail "30-day boundary log was pruned"
[ -f "$RECENT" ] || fail "recent log was pruned"
[ -f "$LOG_DIR/application.log" ] || fail "unrelated file was pruned"
[ -f "$LOG_DIR/daemon.+123-01-01.ndjson" ] || fail "positive-signed-year malformed name was pruned"
[ -f "$LOG_DIR/daemon.-123-01-01.ndjson" ] || fail "negative-signed-year malformed name was pruned"
[ -f "$LOG_DIR/daemon.2x23-01-01.ndjson" ] || fail "non-digit-year malformed name was pruned"
[ -L "$LOG_DIR/daemon.1990-01-01.ndjson" ] || fail "owned-looking symlink was touched"
[ -d "$LOG_DIR/daemon.1990-01-01.ndjson.directory" ] || fail "directory was touched"

step "Emit successful/failing board and Herdr calls without payload logging"
e2e_ws_standard diagnostic-logs
EXEC_ID="$(col_create '{"name":"Execute","trigger":"auto"}')"
CARD_JSON="$($BOARD_BIN card new --title "diagnostic card" \
  -d "prompt-description-sentinel-17d8a4" --harness fake \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$CARD_JSON" | jget id)"
# The untrusted wire request id must round-trip in the protocol but never be
# used as diagnostic correlation metadata.
python3 - "$BOARD_SOCKET" <<'PY'
import json, socket, sys
secret = "credential-in-request-id-S3CR3T"
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((json.dumps({"id": secret, "method": "board.list", "params": {}})+"\n").encode())
response = json.loads(s.makefile().readline())
assert response["id"] == secret
PY
# Unknown method: board completion error metadata; params must never appear.
brpc diagnostic.unknown '{"comment":"board-params-sentinel-72c909"}' >/dev/null || true
# Invalid pane: board error plus a real outbound Herdr protocol error.
brpc pane.set_title "{\"pane_id\":\"missing-pane-sentinel\",\"title\":\"result-sentinel-35ae81\",\"origin_socket\":\"$HERDR_SOCKET_PATH\"}" >/dev/null || true
# Provider-free configured harness exercises successful outbound Herdr placement calls.
e2e_board_herdr_mutate -- move "$CARD_ID" Execute --json >/dev/null
outcome="$(wait_ok "$CARD_ID")" || fail "expected configured harness outcome ok, got $outcome"

step "Validate every NDJSON line, metadata, secrecy, and private modes"
python3 - "$LOG_DIR" "$E2E_TMP" "${DIAGNOSTIC_EVIDENCE_DIR:-}" "$HERDR_SOCKET_PATH" <<'PY'
import json, os, stat, sys, time
from pathlib import Path
root = Path(sys.argv[1])
files = sorted(root.glob("daemon.*.ndjson"))
# Symlinks are preserved retention fixtures, never diagnostic inputs.
files = [p for p in files if p.is_file() and not p.is_symlink()]
deadline = time.time() + 10
records = []
while time.time() < deadline:
    records = []
    for path in files:
        for line in path.read_text().splitlines():
            if line.strip(): records.append(json.loads(line))
    board = [r for r in records if r.get("target") == "board_rpc"]
    herdr = [r for r in records if r.get("target") == "herdr_rpc"]
    bf = [r.get("fields", {}) for r in board]
    hf = [r.get("fields", {}) for r in herdr]
    if (any(f.get("outcome") == "ok" for f in bf)
        and any(f.get("outcome") == "error" and "error_code" in f for f in bf)
        and any(f.get("outcome") == "ok" for f in hf)
        and any(f.get("outcome") == "error" and f.get("error_category") == "protocol" for f in hf)
        and any(f.get("method") == "events.subscribe" for f in hf)):
        break
    time.sleep(.1)
else:
    raise SystemExit("missing board/Herdr success/error/subscription completion metadata")
for record in board + herdr:
    fields = record.get("fields", {})
    assert "timestamp" in record and "level" in record and "target" in record
    assert "method" in fields and "outcome" in fields and isinstance(fields.get("duration_ms"), int)
for fields in bf:
    assert "conn" in fields and "req_id" in fields
all_text = "\n".join(p.read_text() for p in files)
for forbidden in [
    "prompt-description-sentinel-17d8a4", "board-params-sentinel-72c909",
    "result-sentinel-35ae81", "credential-sentinel-4ab973",
    "credential-in-request-id-S3CR3T", "fake: finished", sys.argv[2], sys.argv[4],
    '"params"', '"result"'
]:
    assert forbidden not in all_text
assert stat.S_IMODE(root.stat().st_mode) == 0o700
for path in files:
    assert stat.S_IMODE(path.stat().st_mode) == 0o600
sample_records = [
    next(r for r in board if r.get("fields", {}).get("outcome") == "ok"),
    next(r for r in board if r.get("fields", {}).get("outcome") == "error"),
    next(r for r in herdr if r.get("fields", {}).get("outcome") == "ok"),
    next(r for r in herdr if r.get("fields", {}).get("outcome") == "error"),
    next(r for r in herdr if r.get("fields", {}).get("method") == "events.subscribe"),
]
sample_text = "".join(json.dumps(record)+"\n" for record in sample_records)
(Path(sys.argv[2]) / "diagnostic-sample-redacted.ndjson").write_text(sample_text)
if sys.argv[3]:
    evidence = Path(sys.argv[3])
    evidence.mkdir(parents=True, exist_ok=True)
    os.chmod(evidence, 0o700)
    (evidence / "sample-redacted.ndjson").write_text(sample_text)
    (evidence / "pane-log-evidence.txt").write_text(
        "provider_free_configured_pane=true\n"
        + "herdr_methods=" + ",".join(sorted({str(f.get("method")) for f in hf})) + "\n")
    (evidence / "retention-permissions.txt").write_text(
        "expired_removed=true\nboundary_retained=true\nrecent_retained=true\n"
        "unrelated_retained=true\nmalformed_signed_year_retained=true\n"
        "malformed_nondigit_year_retained=true\nsymlink_retained=true\n"
        "log_dir_mode=0700\nlog_file_mode=0600\n")
print(f"validated {len(records)} JSON records across {len(files)} regular owned files")
PY

step "29-diagnostic-logs: ALL CHECKS PASSED"
