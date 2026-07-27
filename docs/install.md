# Install and optional setup

The install steps the [root README](../README.md) summarizes, plus everything optional around
them: a custom CLI directory, a Herdr keybinding, the harness integration, the agent skill, and
named Herdr sessions.

Requires exactly **Herdr 0.7.5 (protocol 17)**, Git, and a Rust toolchain with `cargo`; Linux and
macOS are supported. See the README for the one-line install command itself.

## Installation details and a custom CLI directory

Herdr first shows an interactive trust preview of the plugin's build commands. After approval it
checks out the source, builds the release binary, registers the plugin, and copies the CLI to
`~/.local/bin/board` as a regular executable. After reviewing the manifest and scripts, a
noninteractive install is available:

```bash
herdr plugin install nelsonPires5/herdr-board --ref v0.9.1 --yes
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
Pi integration. Installation changes your personal Pi extension config, so herdr-board never does it
automatically — it is a user prerequisite. Without it (degraded mode), spawn, explicit `board done`,
timeout, and pane-exit handling still work, but Herdr's `working`/`blocked`/`done` signals do not
exist and a card can only reach `awaiting` (pending review) via the idle grace path.

```bash
herdr integration install pi
```

Claude users can similarly run `herdr integration install claude`. The repository's optional
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
