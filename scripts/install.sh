#!/usr/bin/env bash
# install.sh — set up herdr-board on this machine.
#
# By default this is SAFE: it builds the binary and PRINTS the mutating steps
# (plugin link, skill copy) for you to run, but does not perform them. Pass
# --yes to actually link the plugin and copy the agent skill.
#
#   scripts/install.sh            # build + print the commands to run
#   scripts/install.sh --yes      # build + link plugin + copy skill (mutating)
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

APPLY=0
for arg in "$@"; do
  case "$arg" in
    --yes|-y) APPLY=1 ;;
    *) echo "install.sh: unknown arg: $arg" >&2; exit 2 ;;
  esac
done

herdr_bin="${HERDR_BIN_PATH:-herdr}"
skill_src="$repo_root/skill"
skill_dst="$HOME/.claude/skills/herdr-board"

echo "==> Building the board binary"
bash "$repo_root/scripts/build.sh"

echo
echo "==> Plugin link"
link_cmd="$herdr_bin plugin link \"$repo_root\""
echo "    $link_cmd"

echo
echo "==> Agent skill (copied into your Claude Code skills dir)"
echo "    mkdir -p \"$skill_dst\" && cp \"$skill_src/SKILL.md\" \"$skill_dst/SKILL.md\""

echo
echo "==> PATH symlink (agents inside runs call \`board comment/done\` by name)"
echo "    ln -sf \"$repo_root/target/release/board\" \"$HOME/.local/bin/board\""

if [ "$APPLY" -eq 1 ]; then
  echo
  echo "==> --yes given: applying mutating steps"
  eval "$link_cmd"
  mkdir -p "$skill_dst"
  cp "$skill_src/SKILL.md" "$skill_dst/SKILL.md"
  mkdir -p "$HOME/.local/bin"
  ln -sf "$repo_root/target/release/board" "$HOME/.local/bin/board"
  echo "    linked plugin, copied skill -> $skill_dst/SKILL.md, symlinked ~/.local/bin/board"
else
  echo
  echo "(dry run — re-run with --yes to apply the two steps above)"
fi

cat <<EOF

==> Keybinding — add to ~/.config/herdr/config.toml to summon the board (e.g. prefix+shift+k):

    [[keys.command]]
    key = "prefix+shift+k"
    type = "shell"
    command = "$herdr_bin plugin action invoke open-board --plugin herdr-board"

    # Or open directly as an overlay without the launcher's focus/toggle logic:
    # command = "$herdr_bin plugin pane open --plugin herdr-board --entrypoint board --placement overlay --focus"

==> Recommended: install herdr's Claude integration so agent status (idle/working/
    blocked) and session refs are reported precisely to herdr:

    $herdr_bin integration install claude

Done. Start the board with your keybinding, or: $repo_root/target/release/board tui
EOF
