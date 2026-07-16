#!/usr/bin/env bash
# Copy the plugin-built CLI out of Herdr's managed checkout and into a user bin directory.
set -euo pipefail

if [ "$#" -ne 0 ]; then
  echo "install-cli.sh: no arguments expected; set HERDR_BOARD_CLI_INSTALL_DIR to override the destination directory" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
source_bin="$repo_root/target/release/board"

if [ "${HERDR_BOARD_CLI_INSTALL_DIR+x}" = x ]; then
  if [ -z "$HERDR_BOARD_CLI_INSTALL_DIR" ]; then
    echo "install-cli.sh: HERDR_BOARD_CLI_INSTALL_DIR must not be empty" >&2
    exit 2
  fi
  install_dir="$HERDR_BOARD_CLI_INSTALL_DIR"
else
  if [ -z "${HOME:-}" ]; then
    echo "install-cli.sh: HOME must be set when HERDR_BOARD_CLI_INSTALL_DIR is not provided" >&2
    exit 2
  fi
  install_dir="$HOME/.local/bin"
fi

case "$install_dir" in
  /*) ;;
  *)
    echo "install-cli.sh: install directory must be an absolute path: $install_dir" >&2
    exit 2
    ;;
esac

if [ ! -f "$source_bin" ] || [ ! -x "$source_bin" ]; then
  echo "install-cli.sh: expected executable source binary at $source_bin; run scripts/build.sh first" >&2
  exit 1
fi
sha256_file() {
  local path="$1"
  local output checksum tool_name

  if command -v sha256sum >/dev/null 2>&1; then
    tool_name="sha256sum"
    if ! output="$(sha256sum <"$path")"; then
      echo "install-cli.sh: failed to calculate SHA-256 checksum for $path" >&2
      return 1
    fi
  elif command -v shasum >/dev/null 2>&1; then
    tool_name="shasum -a 256"
    if ! output="$(shasum -a 256 <"$path")"; then
      echo "install-cli.sh: failed to calculate SHA-256 checksum for $path" >&2
      return 1
    fi
  else
    echo "install-cli.sh: sha256sum or shasum is required to validate managed CLI contents" >&2
    return 1
  fi

  checksum="${output%% *}"
  if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]]; then
    echo "install-cli.sh: $tool_name returned an invalid checksum for $path" >&2
    return 1
  fi
  printf '%s\n' "$checksum"
}

if [ -e "$install_dir" ] && [ ! -d "$install_dir" ]; then
  echo "install-cli.sh: install directory exists but is not a directory: $install_dir" >&2
  exit 1
fi

mkdir -p -- "$install_dir"
destination="$install_dir/board"
marker="$install_dir/.herdr-board-cli-managed"
marker_prefix="herdr-board install-cli.sh managed board sha256:"

# -e is false for a broken symlink, so test -L as well. Never read a marker
# through a symlink or from a special file.
if { [ -e "$marker" ] || [ -L "$marker" ]; } && { [ ! -f "$marker" ] || [ -L "$marker" ]; }; then
  echo "install-cli.sh: ownership marker path is not a regular non-symlink file: $marker" >&2
  exit 1
fi

managed=false
managed_checksum=""
if [ -f "$marker" ]; then
  marker_value="$(<"$marker")"
  if [[ "$marker_value" == "${marker_prefix}"* ]]; then
    marker_checksum="${marker_value#"$marker_prefix"}"
    if [[ "$marker_checksum" =~ ^[0-9a-f]{64}$ ]]; then
      managed=true
      managed_checksum="$marker_checksum"
    fi
  fi
fi

# A valid checksum marker is the only permission this script gives itself to
# replace an existing command. Before every managed update, ensure the command
# is still the exact regular file installed previously.
if { [ -e "$destination" ] || [ -L "$destination" ]; } && [ "$managed" != true ]; then
  echo "install-cli.sh: refusing to overwrite unmanaged destination: $destination" >&2
  echo "install-cli.sh: move it aside or set HERDR_BOARD_CLI_INSTALL_DIR to a different absolute directory" >&2
  exit 1
fi
if [ "$managed" = true ]; then
  if [ ! -f "$destination" ] || [ -L "$destination" ]; then
    echo "install-cli.sh: managed destination is not a regular non-symlink file; refusing to overwrite it: $destination" >&2
    exit 1
  fi
  if ! destination_checksum="$(sha256_file "$destination")"; then
    exit 1
  fi
  if [ "$destination_checksum" != "$managed_checksum" ]; then
    echo "install-cli.sh: managed destination checksum does not match its ownership marker; refusing to overwrite it: $destination" >&2
    exit 1
  fi
fi

temporary="$(mktemp "$install_dir/.board.XXXXXX")"
marker_temporary="$(mktemp "$install_dir/.herdr-board-cli-managed.XXXXXX")"
cleanup() {
  rm -f -- "$temporary" "$marker_temporary"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

# Build both files beside their destinations so readers never observe partial
# contents. On a first install, hard-linking is an atomic no-clobber operation;
# managed updates use an atomic rename over the owned destination.
cp -p -- "$source_bin" "$temporary"
if ! installed_checksum="$(sha256_file "$temporary")"; then
  exit 1
fi
marker_value="${marker_prefix}${installed_checksum}"
printf '%s\n' "$marker_value" >"$marker_temporary"
chmod 0644 "$marker_temporary"
if [ "$managed" = true ]; then
  # Portable replacement for mv -T: the destination was already verified as a
  # regular file with the expected checksum.  Replace it and verify the result;
  # clean up leaks when a raced directory swallowed the move.
  mv -f -- "$temporary" "$destination"

  # Postcondition: destination must be a regular non-symlink file whose
  # content matches the expected checksum.
  if [ ! -f "$destination" ] || [ -L "$destination" ]; then
    leaked="$destination/${temporary##*/}"
    if [ -f "$leaked" ]; then
      if leaked_checksum="$(sha256_file "$leaked")" && [ "$leaked_checksum" = "$installed_checksum" ]; then
        rm -f -- "$leaked"
      fi
    fi
    echo "install-cli.sh: managed destination unexpectedly became a directory: $destination" >&2
    exit 1
  fi
  if ! actual_checksum="$(sha256_file "$destination")"; then
    exit 1
  fi
  if [ "$actual_checksum" != "$installed_checksum" ]; then
    echo "install-cli.sh: managed destination checksum mismatch after update" >&2
    exit 1
  fi

  mv -f -- "$marker_temporary" "$marker"
  # Postcondition: marker must be a regular non-symlink file whose
  # content matches the expected value exactly.
  if [ ! -f "$marker" ] || [ -L "$marker" ]; then
    leaked_marker="$marker/${marker_temporary##*/}"
    if [ -f "$leaked_marker" ]; then
      leaked_value="$(<"$leaked_marker")"
      if [ "$leaked_value" = "$marker_value" ]; then
        rm -f -- "$leaked_marker"
      fi
    fi
    echo "install-cli.sh: ownership marker path unexpectedly became a directory: $marker" >&2
    exit 1
  fi
  actual_marker_value="$(<"$marker")"
  if [ "$actual_marker_value" != "$marker_value" ]; then
    echo "install-cli.sh: ownership marker content mismatch after placement" >&2
    exit 1
  fi
else
  # Hard-link is a no-clobber atomic creation on first install.  After ln
  # succeeds, verify the destination is a regular non-symlink hard link to
  # the temporary file.  If ln interpreted a raced directory it silently
  # linked inside it; detect and clean up the leaked candidate.
  if ! ln -- "$temporary" "$destination"; then
    echo "install-cli.sh: refusing to overwrite destination created during install: $destination" >&2
    exit 1
  fi

  if [ ! -f "$destination" ] || [ -L "$destination" ]; then
    leaked="$destination/${temporary##*/}"
    if [ -f "$leaked" ] && [ "$leaked" -ef "$temporary" ]; then
      rm -f -- "$leaked"
    fi
    rm -f -- "$temporary"
    echo "install-cli.sh: destination unexpectedly became a directory during install: $destination" >&2
    exit 1
  fi
  if [ ! "$destination" -ef "$temporary" ]; then
    rm -f -- "$temporary"
    echo "install-cli.sh: installed file is not a link to the staged temporary: $destination" >&2
    exit 1
  fi

  # Keep the staged temporary hardlink alive until ownership marker
  # placement and postcondition succeed so we can safely roll back.
  if ! mv -f -- "$marker_temporary" "$marker"; then
    if [ -f "$destination" ] && [ ! -L "$destination" ] && [ "$destination" -ef "$temporary" ]; then
      rm -f -- "$destination"
    fi
    echo "install-cli.sh: could not create ownership marker: $marker" >&2
    exit 1
  fi
  # Postcondition: marker must be a regular non-symlink file whose
  # content matches the expected value exactly.
  if [ ! -f "$marker" ] || [ -L "$marker" ]; then
    leaked_marker="$marker/${marker_temporary##*/}"
    if [ -f "$leaked_marker" ]; then
      leaked_value="$(<"$leaked_marker")"
      if [ "$leaked_value" = "$marker_value" ]; then
        rm -f -- "$leaked_marker"
      fi
    fi
    if [ -f "$destination" ] && [ ! -L "$destination" ] && [ "$destination" -ef "$temporary" ]; then
      rm -f -- "$destination"
    fi
    echo "install-cli.sh: ownership marker path unexpectedly became a directory: $marker" >&2
    exit 1
  fi
  actual_marker_value="$(<"$marker")"
  if [ "$actual_marker_value" != "$marker_value" ]; then
    if [ -f "$destination" ] && [ ! -L "$destination" ] && [ "$destination" -ef "$temporary" ]; then
      rm -f -- "$destination"
    fi
    echo "install-cli.sh: ownership marker content mismatch after placement" >&2
    exit 1
  fi
  rm -f -- "$temporary"
fi
trap - EXIT HUP INT TERM

echo "install-cli.sh: installed managed CLI at $destination"
