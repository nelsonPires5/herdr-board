# Operations and maintenance

Updating, uninstalling, and installing from a local checkout. The [root README](../README.md)
links here; release *policy* (how a version is cut and tagged) lives in
[`releasing.md`](releasing.md).

## Update

Re-run the install command to update — Herdr has no separate update command, so reinstall over the
existing plugin:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.9.1 --yes
```

The build step requests a graceful stop (`board daemon --stop`) before recompiling, so the new
binary replaces a stopped process instead of overwriting one the old daemon still has mapped in
memory. The command succeeds only after the daemon listener disappears. Stop failures and timeouts
are non-zero and preserve the socket; stale-socket cleanup is only performed after a fresh failed
connect and an identity check. The next `board` command auto-starts a fresh daemon from the new
binary.

Run the install once from each named Herdr session where the plugin is registered.

If you are updating from a version older than the `--stop` flag and a stale daemon is still
serving the old code, use your platform's process manager to stop that specific board process
(after verifying its PID and command) before reinstalling. Do not remove the socket or use a broad
process-name kill.

## Uninstall

Herdr's plugin uninstall has no lifecycle hook and does not stop the board daemon — boardd is a
detached process Herdr does not track, so uninstalling the plugin leaves it running (and, after a
reinstall, serving stale code). Stop it first, then remove the CLI Herdr can't manage (only when
its checksum still matches the managed marker), then unregister the plugin:

```bash
if ! board daemon --stop; then
  echo "board daemon did not stop safely; socket preserved" >&2
  exit 1
fi
(
  if [ "${HERDR_BOARD_CLI_INSTALL_DIR+x}" = x ]; then
    install_dir="$HERDR_BOARD_CLI_INSTALL_DIR"
  else
    install_dir="${HOME:?HOME must be set}/.local/bin"
  fi
  case "$install_dir" in /*) ;; *) echo "Install directory must be absolute" >&2; exit 1;; esac

  board="$install_dir/board"
  marker="$install_dir/.herdr-board-cli-managed"
  prefix="herdr-board install-cli.sh managed board sha256:"
  if [ -f "$board" ] && [ ! -L "$board" ] && [ -f "$marker" ] && [ ! -L "$marker" ]; then
    checksum=""
    if command -v sha256sum >/dev/null 2>&1; then
      checksum_output="$(sha256sum <"$board")" && checksum="${checksum_output%% *}"
    elif command -v shasum >/dev/null 2>&1; then
      checksum_output="$(shasum -a 256 <"$board")" && checksum="${checksum_output%% *}"
    fi
    if [[ "$checksum" =~ ^[0-9a-f]{64}$ ]] && printf '%s\n' "$prefix$checksum" | cmp -s - "$marker"; then
      rm -- "$board" "$marker"
    else
      echo "board CLI was changed or is unrecognized; retaining $board and $marker" >&2
    fi
  else
    echo "board CLI was changed or is unrecognized; retaining $board and $marker" >&2
  fi
)
herdr plugin uninstall herdr-board
```

If `HERDR_BOARD_CLI_INSTALL_DIR` was used, use the same directory for every update and cleanup.
Uninstall the plugin from each named session where it was registered.

To remove all board data (cards, columns, runs), delete the data directory — `BOARD_DB`'s default
(`~/Library/Application Support/herdr-board` on macOS, `~/.local/share/herdr-board` on Linux).
This is optional and never needed for a normal reinstall.

## Local development / source install

For a checkout you plan to edit, use `scripts/install.sh`. It prints proposed plugin links, skill
copies, PATH symlinks, and keybinding changes by default; `--yes` applies them.

```bash
git clone https://github.com/nelsonPires5/herdr-board
cd herdr-board
./scripts/install.sh                         # dry run
./scripts/install.sh --yes                   # default key: prefix+shift+k
./scripts/install.sh --yes --key prefix+shift+b
```

This broader development installer is intentionally separate from the GitHub plugin install flow.
