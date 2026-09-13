# Operations and maintenance

Updating, uninstalling, and installing from a local checkout. The [root README](../README.md)
links here; release *policy* (how a version is cut and tagged) lives in
[`releasing.md`](releasing.md).

## Update

Re-run the install command to update — Herdr has no separate update command, so reinstall over the
existing plugin:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.16.1 --yes
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

### Compatibility-safe upgrade

Treat a Herdr upgrade and a board upgrade as one compatibility change. Do not dispatch a live card
while changing either component.

1. Read the active-run counts first. Both must be zero before stopping the daemon:

   ```bash
   board daemon status --json
   ```

   Wait for or explicitly cancel any active or queued run through its card workflow. Never stop a
   daemon merely to clear an open run.
2. Stop the exact board daemon gracefully, then confirm the command succeeded:

   ```bash
   board daemon stop --json
   ```

3. Upgrade Herdr through its documented update/install path. Select the exact Herdr version and
   socket protocol supported by the board release you intend to install; the compatibility matrix is
   authoritative in [`herdr.md`](herdr.md).
4. Verify the installed Herdr binary and socket protocol against that exact compatibility contract.
   The binary and live schema are the evidence:

   ```bash
   herdr --version
   herdr api schema --json
   ```

   A mismatch means this board release must not dispatch cards. Install a compatible tagged board
   release; do not weaken or bypass the compatibility gate.
5. Check out the exact tag or commit whose artifact will be installed, then validate that checkout
   in the isolated, disposable Herdr session using the sandbox workflow in [`sandbox.md`](sandbox.md).
   The smoke must prove one disposable card can dispatch, complete, and clean up without touching a
   user session, workspace, or board database.
6. Install the artifact built from that same validated tag or commit in the operational session,
   then run:

   ```bash
   board daemon status --json
   ```

   Do not move a card into an automatic column until `herdr_connected` is `true` and the daemon
   reports no unexpected active or queued runs.

If only the board reinstall failed and Herdr is unchanged, leave the board stopped and return to the
previous tagged board release only when it supports the installed Herdr pair. After a Herdr change,
install a board release compatible with the current Herdr pair, or perform a documented, verified
rollback of the complete Herdr-and-board pair. Never downgrade Herdr, delete a daemon socket, or
broad-kill processes as a compatibility shortcut.

## Diagnostic logs

boardd writes one JSON object per line to private daily files in the XDG data directory:
`~/.local/share/herdr-board/logs/daemon.YYYY-MM-DD.ndjson` on Linux (the platform data
directory equivalent on macOS; override the directory with `BOARD_LOG_DIR`). The directory is
mode `0700` and regular log files are mode
`0600`. `board daemon --foreground` mirrors the same structured events to stderr.

Each board RPC and outbound Herdr RPC/subscription completion records its method, duration,
outcome, error category/code when applicable, and safe correlation IDs. Board request correlation
is the daemon-generated numeric `(conn, req_id)` pair; the arbitrary wire request ID is returned on
the protocol unchanged but never logged. Records exclude request parameters and results by construction. Prompts, descriptions, comments, system prompts, agent
output, argv/environment values, credentials, and terminal contents are never logged; there is no
raw/debug payload mode. Review before sharing even though the format is designed to be redacted:

```bash
jq 'select(.target == "board_rpc" or .target == "herdr_rpc")' \
  ~/.local/share/herdr-board/logs/daemon.*.ndjson
jq -r '[.timestamp,.target,.fields.method,.fields.outcome] | @tsv' \
  ~/.local/share/herdr-board/logs/daemon.*.ndjson
```

At startup and approximately daily while running, boardd removes only regular files with the exact
`daemon.YYYY-MM-DD.ndjson` name (four ASCII year digits and two ASCII month/day digits) whose
modification time is more than 30 days old. The exact 30-day boundary, symlinks, directories,
malformed names, future files, and unrelated files remain. The old append-only
`~/.local/share/herdr-board/daemon.log` is no longer opened or grown after upgrade; it may be
reviewed and removed manually. Auto-start bootstrap errors use private `logs/bootstrap.log`, which
is truncated on each start. If the daily writer repeatedly fails during that daemon lifetime,
bootstrap receives at most one fixed path-free notice and the unavailable records are dropped;
this deterministic fallback prevents runtime growth while preserving the private, symlink-safe file.

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
