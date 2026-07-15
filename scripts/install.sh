#!/usr/bin/env bash
# install.sh — set up herdr-board on this machine.
#
# By default this is SAFE: it builds the binary and PRINTS the mutating steps
# (plugin link, skill copy, keybinding) for you to run, but does not perform
# them. Pass --yes to actually link the plugin, copy the agent skill, and add the
# keybinding to ~/.config/herdr/config.toml.
#
#   scripts/install.sh                    # build + print the commands to run
#   scripts/install.sh --yes              # build + link plugin + copy skill + add keybinding
#   scripts/install.sh --yes --key prefix+shift+b   # same, custom key combo
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

APPLY=0
KEY="prefix+shift+k"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --yes|-y) APPLY=1 ;;
    --key) shift; [ "$#" -gt 0 ] || { echo "install.sh: --key needs a value" >&2; exit 2; }; KEY="$1" ;;
    --key=*) KEY="${1#--key=}" ;;
    *) echo "install.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

herdr_bin="${HERDR_BIN_PATH:-herdr}"
skill_src="$repo_root/skill"
skill_dst="$HOME/.claude/skills/herdr-board"
herdr_config="$HOME/.config/herdr/config.toml"

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

# The [[keys.command]] block that summons the board, using the chosen key combo.
keybinding_block() {
  cat <<EOF
[[keys.command]]
key = "$KEY"
type = "shell"
command = "$herdr_bin plugin action invoke open-board --plugin herdr-board"
EOF
}

echo
echo "==> Keybinding ($KEY -> open the board) in $herdr_config:"
keybinding_block | sed 's/^/    /'

if [ "$APPLY" -eq 1 ]; then
  echo
  echo "==> --yes given: applying mutating steps"
  eval "$link_cmd"
  mkdir -p "$skill_dst"
  cp "$skill_src/SKILL.md" "$skill_dst/SKILL.md"
  mkdir -p "$HOME/.local/bin"
  ln -sf "$repo_root/target/release/board" "$HOME/.local/bin/board"
  echo "    linked plugin, copied skill -> $skill_dst/SKILL.md, symlinked ~/.local/bin/board"

  # Keybinding. Idempotency heuristic: if the config already invokes open-board
  # (any keys context), assume the binding is present and DO NOT touch the file.
  if [ -f "$herdr_config" ] && grep -q 'invoke open-board' "$herdr_config"; then
    echo "    keybinding already configured — skipping"
  elif [ -f "$herdr_config" ]; then
    bak="$herdr_config.bak.$(date +%s)"
    cp "$herdr_config" "$bak"
    printf '\n%s\n' "$(keybinding_block)" >> "$herdr_config"
    echo "    added keybinding ($KEY) to $herdr_config (backup: $bak)"
    "$herdr_bin" server reload-config >/dev/null 2>&1 \
      && echo "    reloaded running server config" \
      || echo "    (no running server to reload — binding applies next start)"
  else
    mkdir -p "$(dirname "$herdr_config")"
    keybinding_block > "$herdr_config"
    echo "    created $herdr_config with the keybinding ($KEY)"
    "$herdr_bin" server reload-config >/dev/null 2>&1 \
      && echo "    reloaded running server config" \
      || echo "    (no running server to reload — binding applies next start)"
  fi
else
  echo
  echo "(dry run — re-run with --yes to apply the steps above, including the keybinding)"
fi

cat <<EOF

    # Alternatively, open directly as an overlay without the launcher's focus/toggle logic:
    # command = "$herdr_bin plugin pane open --plugin herdr-board --entrypoint board --placement overlay --focus"

==> Recommended: install herdr's Claude integration so agent status (idle/working/
    blocked) and session refs are reported precisely to herdr:

    $herdr_bin integration install claude

Done. Start the board with your keybinding, or: $repo_root/target/release/board tui
EOF
