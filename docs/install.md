# Install and optional setup

The install steps the [root README](../README.md) summarizes, plus everything optional around
them: a custom CLI directory, a Herdr keybinding, the harness integration, the agent skill, and
named Herdr sessions.

Requires exactly **Herdr 0.8.0 (socket protocol 19)**, Git, and a Rust toolchain with `cargo`; Linux
and macOS are supported. The board-side compatibility contract remains board protocol v1 and
SQLite schema v13. See the README for the one-line install command itself.

| Component | Required support level | How to verify |
|---|---|---|
| Herdr binary | 0.8.0 | `herdr --version` → `herdr 0.8.0` |
| Herdr socket | protocol 19 | `herdr api schema --json` → top-level `protocol: 19`; a running session's `herdr api snapshot` also reports `version` and `protocol` |
| Board socket | v1 | `docs/protocol.md` and `board-core::protocol` |
| SQLite | schema v13 | `schema.sql` and `board-core::db` migrations |
| Pi integration | v8 for precise Pi lifecycle/session signals | `herdr integration status` |
| Claude integration | v7 for precise Claude lifecycle/session signals | `herdr integration status` |

The board rejects a different Herdr version or socket protocol before workspace discovery or pane
placement; it does not silently fall back to an older wire contract. The integration versions are
user-managed prerequisites, not plugin files installed by herdr-board.

## Verify the installed Herdr before installing

These are read-only checks against the binary and session you are about to use:

```bash
test "$(herdr --version)" = "herdr 0.8.0"
herdr api schema --json | python3 -c \
  'import json, sys; s=json.load(sys.stdin); assert s["protocol"] == 19, s'
herdr api snapshot
herdr integration status
```

Use `herdr api schema --output PATH` when you need a saved schema for review. Confirm that the
status output shows Pi **current (v8)** and Claude **current (v7)** before relying on precise
working/blocked/done or session-identity signals. `herdr integration install --help` is the
source of truth for the installable target names; install only the harness integrations you use.

## Installation details and a custom CLI directory

Herdr 0.8.0 first shows an interactive trust preview of the plugin's build commands. Relative
plugin commands resolve from the plugin root, so the manifest's build/action paths do not depend on
the caller's current directory. After approval Herdr checks out the source, builds the release
binary, registers the plugin, and copies the CLI to `~/.local/bin/board` as a regular executable.
After reviewing the manifest and scripts, a noninteractive install is available:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.11.1 --yes
```

Set `HERDR_BOARD_CLI_INSTALL_DIR` to an absolute user bin directory before installing to override
`~/.local/bin`; the installed command is `<that-directory>/board`. The installer records the
binary's SHA-256 checksum in `<that-directory>/.herdr-board-cli-managed`. Updates only overwrite a
regular, non-symlink `board` whose contents still match that marker.

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
is v7). The repository's optional
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
