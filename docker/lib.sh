#!/usr/bin/env bash
# Shared helpers for the herdr-board sandbox in-container scripts.
# Sourced by docker/selfcheck.sh, docker/gates.sh, docker/prepare.sh.
# Not executable on its own.

hb_fail() {
  echo "selfcheck: FAIL: $*" >&2
  exit 1
}

hb_pass() {
  echo "selfcheck: PASS: $*"
}

# Allowed non-standard mount points. Anything mounted into the container
# outside this set (plus Docker's own pseudo-filesystems and /etc/hosts,
# /etc/hostname, /etc/resolv.conf) is a hard failure: the sandbox must never
# carry host sockets, host config, or user data directories.
hb_mount_allowlist() {
  cat <<'EOF'
/repo
/repo/target
/opt/cargo
/home/board
/artifacts
/secrets
EOF
}

# Audit /proc/self/mountinfo: every mount point must be system-standard or
# allowlisted, and the repo mount must be read-only.
hb_audit_mounts() {
  local bad=0 mountpoint opts repo_opts=""
  # field 5 is the mount point (octal-escaped), field 6 the options.
  while IFS= read -r line; do
    mountpoint="$(awk '{print $5}' <<<"$line")"
    opts="$(awk '{print $6}' <<<"$line")"
    case "$mountpoint" in
      /|/proc|/proc/*|/dev|/dev/*|/sys|/sys/*|/tmp|/run|/run/*)
        ;;
      /etc/hosts|/etc/hostname|/etc/resolv.conf)
        ;;
      /repo|/repo/target|/opt/cargo|/home/board|/artifacts|/secrets|/secrets/*)
        if [ "$mountpoint" = "/repo" ]; then repo_opts="$opts"; fi
        ;;
      *)
        echo "mount audit: unexpected mount point: $mountpoint ($opts)" >&2
        bad=1
        ;;
    esac
  done < /proc/self/mountinfo

  if grep -qiE 'docker\.sock|/var/run/docker' /proc/self/mountinfo; then
    echo "mount audit: docker socket is mounted" >&2
    bad=1
  fi

  if [ -z "$repo_opts" ]; then
    echo "mount audit: /repo is not mounted" >&2
    bad=1
  elif [[ ",$repo_opts," != *",ro,"* && "$repo_opts" != "ro"* ]]; then
    echo "mount audit: /repo is writable ($repo_opts)" >&2
    bad=1
  fi

  [ "$bad" -eq 0 ]
}
