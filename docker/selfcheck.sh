#!/usr/bin/env bash
# herdr-board sandbox safety self-check. Runs INSIDE the container and proves
# the isolation profile: non-root process, read-only source mount, no host
# sockets, allowlisted mounts only, and no network egress in deterministic
# modes. Any failure exits non-zero.
#
# HB_SELFCHECK_NETWORK=on flips the network expectation for modes that
# legitimately enable the network (the explicit real-provider agent opt-in);
# every mount and identity check still applies there.
set -euo pipefail
selfcheck_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
# shellcheck source=lib.sh
source "$selfcheck_dir/lib.sh"

# 1. Non-root process.
if [ "$(id -u)" -eq 0 ]; then
  hb_fail "running as root (uid 0); the sandbox must run as a non-root user"
fi
hb_pass "non-root process (uid $(id -u))"

# 2. Source mount is present and read-only. /repo/target is the build-output
#    volume (the one deliberate writable spot under /repo, outside the
#    repository on the host); everything else under /repo must be read-only.
if [ ! -d /repo ]; then
  hb_fail "/repo is not mounted"
fi
if touch /repo/.hb-sandbox-ro-probe 2>/dev/null; then
  rm -f /repo/.hb-sandbox-ro-probe
  hb_fail "/repo is writable; the worktree must be mounted read-only"
fi
hb_pass "/repo mounted read-only (build output only at /repo/target)"

# 3. Mount audit: allowlisted mount points only, no docker socket anywhere.
if hb_audit_mounts; then
  hb_pass "mount audit: only allowlisted mounts, no docker socket"
else
  hb_fail "mount audit failed (unexpected mounts or docker socket present)"
fi

# 4. Host board/Herdr state must not leak in. The container-local state lives
# under /home/board (a named volume), so a stray host database or socket at
# the default host paths would mean a bad mount.
for p in /root/.config/herdr /root/.local/share/herdr-board; do
  [ ! -e "$p" ] || hb_fail "host state directory visible: $p"
done
hb_pass "no host herdr-board state directories visible"

# 5. Network egress probe. Deterministic modes run with --network none: both
# DNS resolution and a direct TCP connect must fail.
expect_network="${HB_SELFCHECK_NETWORK:-off}"
probe="$(python3 - <<'PY'
import socket
import sys

results = {}
try:
    socket.getaddrinfo("github.com", 443)
    results["dns"] = "resolved"
except Exception:
    results["dns"] = "unresolved"
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(3)
try:
    s.connect(("1.1.1.1", 443))
    results["tcp"] = "connected"
except Exception:
    results["tcp"] = "unreachable"
finally:
    s.close()
print(f"{results['dns']} {results['tcp']}")
PY
)"
dns="${probe% *}"
tcp="${probe#* }"
if [ "$expect_network" = off ]; then
  if [ "$dns" != unresolved ] || [ "$tcp" != unreachable ]; then
    hb_fail "network is reachable in a deterministic mode (dns=$dns tcp=$tcp)"
  fi
  hb_pass "network disabled (dns unresolved, tcp unreachable)"
else
  if [ "$dns" = resolved ] || [ "$tcp" = connected ]; then
    hb_pass "network reachable as opted in (dns=$dns tcp=$tcp)"
  else
    hb_fail "opted-in network is not reachable (dns=$dns tcp=$tcp)"
  fi
fi

echo "selfcheck: all checks passed"
