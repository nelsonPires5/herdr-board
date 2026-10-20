# Install and optional setup

The install steps the [root README](../README.md) summarizes, plus everything optional around
them: a custom CLI directory, a Herdr keybinding, the harness integration, the agent skill, and
named Herdr sessions.

Requires **Herdr >= 0.9.0 speaking socket protocol 22** (including 0.9.1), Git, and a Rust
toolchain with `cargo`; Linux, macOS, and Windows 10/11 x64 are supported (Windows specifics are
[below](#install-on-windows)). The board-side compatibility contract remains board protocol v1 and
SQLite schema v15. See the README for the one-line install command itself.

| Component | Required support level | How to verify |
|---|---|---|
| Herdr binary | >= 0.9.0 with protocol 22 | `herdr --version` (e.g. `herdr 0.9.0` or `herdr 0.9.1`) |
| Herdr socket | protocol 22 | `herdr api schema --json` → top-level `protocol: 22`; a running session's `herdr api snapshot` also reports `version` and `protocol` |
| Board socket | v1 | `docs/protocol.md` and `board-core::protocol` |
| SQLite | schema v15 | `schema.sql` and `board-core::db` migrations |
| Pi integration | v8 for precise Pi lifecycle/session signals | `herdr integration status` |
| Claude integration | v7 for precise Claude lifecycle/session signals | `herdr integration status` |
| Antigravity CLI integration | v1 for the `agy` conversation-id capture (resume/retry/rescue) | `herdr integration status` |

The board rejects a different socket protocol before workspace discovery or pane placement; a
version differing from the 0.9.0 reference is warned about but accepted when the socket speaks
protocol 22. It does not silently fall back to an older wire contract. The integration versions are
user-managed prerequisites, not plugin files installed by herdr-board.

## Verify the installed Herdr before installing

These are read-only checks against the binary and session you are about to use:

```bash
herdr --version  # check the installed release; 0.9.0 and 0.9.1 speak protocol 22
herdr api schema --json | python3 -c \
  'import json, sys; s=json.load(sys.stdin); assert s["protocol"] == 22, s'
herdr api snapshot
herdr integration status
```

Use `herdr api schema --output PATH` when you need a saved schema for review. Confirm that the
status output shows Pi **current (v8)** and Claude **current (v7)** before relying on precise
working/blocked/done or session-identity signals. Antigravity users: confirm the status shows
**antigravity-cli current (v1)** before relying on conversation reuse — without it an antigravity
run still executes, but its conversation id is never captured. `herdr integration install --help` is the
source of truth for the installable target names; install only the harness integrations you use.

## Installation details and a custom CLI directory

Herdr 0.9.0 first shows an interactive trust preview of the plugin's build commands. Relative
plugin commands resolve from the plugin root, so the manifest's build/action paths do not depend on
the caller's current directory. After approval Herdr checks out the source, builds the release
binary, registers the plugin, and copies the CLI to `~/.local/bin/board` as a regular executable.
After reviewing the manifest and scripts, a noninteractive install is available:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.19.1 --yes
```

Set `HERDR_BOARD_CLI_INSTALL_DIR` to an absolute user bin directory before installing to override
`~/.local/bin`; the installed command is `<that-directory>/board`. The installer records the
binary's SHA-256 checksum in `<that-directory>/.herdr-board-cli-managed`. Updates only overwrite a
regular, non-symlink `board` whose contents still match that marker.

## Install on Windows

Native Windows needs Windows 10 or 11 on x64, **Herdr's Windows build speaking socket protocol
22**, Git, and a Rust toolchain for the MSVC target (`rustup` with the Visual Studio C++ build
tools). The install command is the same:

```powershell
herdr plugin install nelsonPires5/herdr-board --ref v0.19.1
```

The manifest declares Windows-only entries next to the Linux/macOS ones, because Herdr rejects a
repeated pane or action id even when the entries target different platforms:

| What | Linux / macOS | Windows |
|---|---|---|
| Build steps | `scripts/build.sh`, `scripts/install-cli.sh` | `scripts/build.ps1`, `scripts/install-cli.ps1` (run with `powershell.exe -NoProfile -ExecutionPolicy Bypass -File`) |
| Board pane | `board` | `board-windows` |
| Open action | `open-board` | `open-board-windows` |

The Linux/macOS pane and action are not offered on Windows; Herdr reports them as unsupported on
that platform. So on Windows open the board with:

```powershell
herdr plugin action invoke open-board-windows --plugin herdr-board
herdr plugin pane open --plugin herdr-board --entrypoint board-windows --placement tab --focus
```

- **CLI location:** `scripts/install-cli.ps1` copies `board.exe` to
  `%USERPROFILE%\.local\bin\board.exe` (or `HERDR_BOARD_CLI_INSTALL_DIR`, which must be a drive or
  UNC absolute path). Add that directory to your user `PATH`. Its checksum marker,
  `.herdr-board-cli-managed`, follows the same ownership rule as on Linux/macOS; the marker text
  names `install-cli.ps1`.
- **Data, config, and logs:** `%APPDATA%\herdr-board\` holds `board.db`, `config.toml`, and
  `logs\`. The daemon's endpoint is the named pipe `\\.\pipe\` followed by the full `boardd.sock`
  path (for example `\\.\pipe\C:\Users\me\AppData\Roaming\herdr-board\boardd.sock`), with a small
  identity marker file at the `.sock` path itself.
- **Release zip:** each GitHub release also has `board-X.Y.Z-x86_64-pc-windows-msvc.zip` and its
  `.sha256`. It holds a prebuilt `target\release\board.exe`, `herdr-plugin.toml`, `skill\`, and
  the three PowerShell scripts. Check it with
  `(Get-FileHash -Algorithm SHA256 <zip>).Hash` against the `.sha256` file. Copying `board.exe`
  onto your `PATH` gives you the CLI without a Rust toolchain.
- **Agents in panes:** Herdr panes run your Herdr `default_shell` (for example PowerShell 7).
  Agents call the same `board` commands and read `$env:BOARD_CARD_ID` / `$env:BOARD_RUN_ID`.
  Claude runs are allowed to call `board` from both its Bash and PowerShell tools. Configured
  (config-defined) harnesses run through a generated PowerShell script, so `pwsh` (preferred; 7.3
  or later for exact argument passing) or Windows PowerShell must be on the pane's `PATH`,
  Group Policy must not block `-ExecutionPolicy Bypass`, and `%TEMP%` must not contain `"`, `$`,
  a backtick, `%`, or `!`. A configured harness whose program is a `.cmd`/`.bat` shim cannot be
  given arguments with line breaks, `"`, or empty strings (the launch is refused); point it at the
  real executable instead.
- **Editor:** `Ctrl+E` in the TUI opens `$EDITOR`, then `$VISUAL`, else `notepad`.

Verify the Herdr you are about to use (read-only):

```powershell
herdr --version
(herdr api schema --json | ConvertFrom-Json).protocol   # must print 22
herdr integration status
```

Not yet covered by automation on Windows: the live e2e suite and the Docker sandbox are
Linux/macOS (run the sandbox from WSL2), and CI runs only the Rust lint and test tiers on
`windows-latest`. Plugin install, the overlay pane, and agent dispatch against a real Windows
Herdr have no automated coverage; validate them by hand in a disposable Herdr session.

## Add a Herdr keybinding

Plugin installation deliberately does not edit `~/.config/herdr/config.toml`. Add a command such as
this yourself (do not reuse a Herdr default; `prefix+k` is `focus_pane_up`, so check
`herdr --default-config`):

```toml
[[keys.command]]
key = "prefix+shift+k"
type = "shell"
command = "herdr plugin action invoke open-board --plugin herdr-board"
```

On Windows, use `open-board-windows` in `command`.

## Install the harness integration and optional agent skill

For precise Pi status (`idle`, `working`, `blocked`, `done`) and session references, install Herdr's
**Pi v8** integration. Installation changes your personal Pi extension config, so herdr-board never
does it automatically — it is a user prerequisite. Without it (degraded mode), spawn, explicit
`board done`, timeout, and pane-exit handling still work, but Herdr's `working`/`blocked`/`done`
signals do not exist and a card can only reach `awaiting` (pending review) via the idle grace path.

```bash
herdr integration install pi
```

Claude users can similarly run `herdr integration install claude` (the supported Claude integration
is v7). Antigravity users run `herdr integration install antigravity-cli` (the Antigravity CLI
integration, current v1): it installs the hook that reports the `agy` conversation id, which the
daemon captures after launch so later stages, `card run focus`, and rescues can re-attach with
`--conversation <id>`; when the recorded conversation no longer exists the CLI starts a new one and
the daemon persists the new id with a visible card warning. Without the integration the run still
dispatches and `board done` still works, but the mint completes with no recorded conversation id (a
`system` card warning explains the missing integration) and reuse/rescue fail closed — see
[`herdr.md`](herdr.md) for the integration contract. The repository's optional
[`skill/SKILL.md`](../skill/SKILL.md) teaches interactive or dispatched agents to comment, call
`board done`, and queue work. GitHub plugin installation does not copy the skill; the
local-development installer below can do so.

## Use named Herdr sessions

Herdr keeps a plugin registry per session, while keybindings/configuration are global. Run the
GitHub install command once from every named session where the plugin should be registered.

A single board daemon serves every scoped board across every Herdr session. Each card carries a
`session` (the default session when unset), and dispatch resolves that session's socket through
`herdr session list`. Use `BOARD_SOCKET` and `BOARD_DB` overrides only when you want a completely
separate board stack.
