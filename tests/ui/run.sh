#!/bin/sh
# The Thurbox plugin's tests, under Lua 5.4 (the kernel's own Lua), against the
# real `lib/` of the thurbox-cli being tested.
#
#   tests/ui/run.sh              every suite
#   tests/ui/run.sh model        one of: model, pane, badge
#
# The stock interface comes from `thurbox-cli plugin new`, which writes the
# release's own `ui/` into an empty directory, so the pane is always tested
# against the lib/ of the binary it will run on — $TAC_THURBOX_CLI, else the
# one on PATH. Nothing outside target/ is written.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
cli=${TAC_THURBOX_CLI:-thurbox-cli}
lua=${LUA:-}
if [ -z "$lua" ]; then
    for candidate in lua5.4 lua54 lua; do
        if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -v 2>&1 | grep -q 'Lua 5\.4'; then
            lua=$candidate
            break
        fi
    done
fi
[ -n "$lua" ] || { echo "tests/ui/run.sh: Lua 5.4 not found (set LUA)" >&2; exit 1; }
command -v "$cli" >/dev/null 2>&1 || { echo "tests/ui/run.sh: no thurbox-cli (set TAC_THURBOX_CLI)" >&2; exit 1; }

version=$("$cli" --version | awk '{print $2}')
target="${CARGO_TARGET_DIR:-$repo/target}/ui-tests"
stock="$target/stock-$version"
if [ ! -f "$stock/ui/lib/widgets.lua" ]; then
    rm -rf "$stock"
    mkdir -p "$stock/ui" "$stock/home"
    # Its own HOME and config, so the seeding reads and writes nothing real.
    env -u THURBOX_CONFIG_DIR -u THURBOX_DATA_DIR \
        HOME="$stock/home" XDG_CONFIG_HOME="$stock/home/.config" XDG_DATA_HOME="$stock/home/.local/share" \
        THURBOX_UI_DIR="$stock/ui" "$cli" plugin new seed --text >/dev/null
    rm -f "$stock"/ui/plugins/*_seed.lua
fi

scratch="$target/scratch"
rm -rf "$scratch"
mkdir -p "$scratch"

status=0
for suite in ${1:-model pane badge}; do
    echo "== $suite (thurbox $version)"
    HERE="$here" UI="$stock/ui" PLUGIN_ROOT="$repo" SCRATCH="$scratch" \
        "$lua" "$here/test_$suite.lua" || status=1
done
rm -rf "$scratch"
exit "$status"
