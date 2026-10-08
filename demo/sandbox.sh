#!/usr/bin/env bash
# Build a throwaway Thurbox with auto-continue and its plugin installed, and
# print its root. demo/record.sh records the GIF in it; you can also look
# around in it yourself (see the end of this file).
#
# Nothing here can touch a real Thurbox, Claude or tmux: HOME and every XDG
# root point at a fresh directory, TMUX_TMPDIR gives it its own tmux socket
# directory, and `claude` is examples/fake_claude.rs — the stand-in the
# end-to-end tests use, which draws Claude's screens and writes its transcript
# rows without an account. `codex` is a stub that prints one line.
#
# Both halves are installed the way the README says: the extension with
# `./install.sh --binary`, the plugin with `thurbox-cli plugin install` from a
# clone of this repository's COMMITTED state, so what a recording shows is what
# a user gets rather than the working tree.
#
#   demo/sandbox.sh        prints the root; the caller tears it down:
#                          TMUX_TMPDIR=<root>/tmux tmux -L thurbox kill-server
#                          rm -f "$(cat <root>/home-link)"; rm -rf <root>
#
# Needs: cargo, git, sqlite3, tmux, and thurbox-cli on PATH (or THURBOX_CLI).
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
CLI=${THURBOX_CLI:-$(command -v thurbox-cli || true)}
[ -x "$CLI" ] || { echo "no thurbox-cli on PATH (set THURBOX_CLI)" >&2; exit 2; }
for tool in cargo git sqlite3 tmux; do
    command -v "$tool" >/dev/null || { echo "demo/sandbox.sh needs $tool" >&2; exit 2; }
done

# Not /tmp: it is RAM on plenty of machines, and a recording leaves a cast behind.
ROOT=${DEMO_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/thurbox-auto-continue-demo}
mkdir -p "$ROOT"
S=$(mktemp -d "$ROOT/sandbox.XXXXXX")
# HOME is a short symlink to the sandbox's home: Thurbox's UI control socket
# lives under it, and a socket path longer than the OS allows (about 104
# bytes) would be refused, with a notice in the recording.
mkdir -p "$S/home"
LINK=$(mktemp -u "${TMPDIR:-/tmp}/tac-XXXXXX")
ln -s "$S/home" "$LINK"
echo "$LINK" >"$S/home-link"
trap 'TMUX_TMPDIR="$S/tmux" tmux -L thurbox kill-server 2>/dev/null; rm -f "$LINK"; rm -rf "$S"' ERR

(cd "$REPO" && cargo build --release --locked --quiet --bin thurbox-auto-continue --example fake_claude)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}/release

export HOME="$LINK"
export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state" XDG_CACHE_HOME="$HOME/.cache"
export TMUX_TMPDIR="$S/tmux" FAKE_CLAUDE_CTL="$S/ctl"
# These win over XDG, so an inherited one would point at a real config.
unset THURBOX_CONFIG_DIR THURBOX_DATA_DIR THURBOX_UI_DIR THURBOX_AUTO_CONTINUE_HOME
mkdir -p "$HOME/.claude" "$HOME/.local/bin" "$S/bin" "$TMUX_TMPDIR" "$FAKE_CLAUDE_CTL"
ln -s "$CLI" "$S/bin/thurbox-cli"
ln -s "$(command -v tmux)" "$S/bin/tmux"
export PATH="$S/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"

# Thurbox's own status hooks, and its agents with the stand-ins in place.
thurbox-cli extension activate hooks >/dev/null
# Offline and quiet: no update check or self-update (a recording must show the
# release it was made with), and no desktop notifications from a sandbox.
cat >> "$XDG_CONFIG_HOME/thurbox/settings.toml" <<'SETTINGS'

[features]
version_check = false
auto_update = false
notifications = false
SETTINGS
cat > "$S/bin/fake-codex" <<'CODEX'
#!/bin/sh
printf 'codex (a stub for the demo: not Claude, so auto-continue leaves it alone)\n'
exec cat >/dev/null
CODEX
chmod +x "$S/bin/fake-codex"
agents="$XDG_CONFIG_HOME/thurbox/agents.toml"
sed -i.bak -e "s|^command = \"claude\"|command = \"$TARGET/examples/fake_claude\"|" \
    -e "s|^command = \"codex\"|command = \"$S/bin/fake-codex\"|" "$agents"
rm -f "$agents.bak"

# The extension, as the README installs it.
"$REPO/install.sh" --binary "$TARGET/thurbox-auto-continue" >/dev/null

# A small project to run sessions in.
P="$S/weather-cli"
mkdir -p "$P"
printf '# weather-cli\n\nPrints the forecast for a city.\n' > "$P/README.md"
git -C "$P" init -q -b main
git -C "$P" -c user.email=demo@example.com -c user.name=demo -c commit.gpgsign=false add -A
git -C "$P" -c user.email=demo@example.com -c user.name=demo -c commit.gpgsign=false commit -qm "a forecast CLI"
for spec in parser:claude docs:claude review:codex; do
    thurbox-cli session create --name "${spec%%:*}" --repo-path "$P" --agent "${spec#*:}" >/dev/null
done

# The plugin, as the README installs it, from committed state and under the
# name it requires its modules by.
git clone -q "$REPO" "$S/src/thurbox-auto-continue"
for file in ui/plugins/85_auto_continue.lua ui/plugins/86_auto_continue_badge.lua; do
    thurbox-cli plugin install "git+file://$S/src/thurbox-auto-continue" --as "$file" >/dev/null
done

# The first launch asks whether to continue to v2 and waits for an answer. The
# recording is about the pane, so that answer is given up front.
sqlite3 "$XDG_DATA_HOME/thurbox/thurbox.db" \
    "INSERT INTO metadata (key, value) VALUES ('v2_interface_acknowledged', '1')
     ON CONFLICT(key) DO UPDATE SET value = '1';"

thurbox-cli plugin check --text >&2

# A shell into the sandbox, for looking around by hand:
cat > "$S/enter.sh" <<ENTER
#!/bin/sh
# Every variable the sandbox needs; then \`thurbox\` opens its TUI.
export HOME="$HOME" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" XDG_DATA_HOME="$XDG_DATA_HOME"
export XDG_STATE_HOME="$XDG_STATE_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME"
export TMUX_TMPDIR="$TMUX_TMPDIR" FAKE_CLAUDE_CTL="$FAKE_CLAUDE_CTL" PATH="$PATH"
unset THURBOX_CONFIG_DIR THURBOX_DATA_DIR THURBOX_UI_DIR THURBOX_AUTO_CONTINUE_HOME
exec "\${@:-\${SHELL:-sh}}"
ENTER
chmod +x "$S/enter.sh"
echo "$S"
