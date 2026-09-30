#!/bin/sh
# Install or remove the auto-continue extension on this machine (POSIX).
#
#   ./install.sh                     build from this checkout and install
#   ./install.sh --binary <path>     install an already-built binary
#   ./install.sh --uninstall         remove it, and every trace of it in Thurbox
#
# Run it on every machine that runs Claude sessions, including each shared host.
# Auto-continue stays OFF until you turn it on (see README.md).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
home_dir="${THURBOX_AUTO_CONTINUE_HOME:-$HOME/.config/thurbox/auto-continue}"
path_dir="$HOME/.local/bin"
name=thurbox-auto-continue

die() { echo "install.sh: $*" >&2; exit 1; }
command -v thurbox-cli >/dev/null 2>&1 || die "thurbox-cli is not on PATH"

case "${1:-}" in
--uninstall)
    if [ -x "$home_dir/bin/$name" ]; then
        "$home_dir/bin/$name" forget --all --home "$home_dir" || true
    fi
    thurbox-cli extension uninstall auto-continue --purge >/dev/null
    [ -L "$path_dir/$name" ] || [ -f "$path_dir/$name" ] && rm -f "$path_dir/$name"
    echo "auto-continue removed"
    exit 0
    ;;
--binary)
    [ -n "${2:-}" ] || die "--binary needs a path"
    binary=$2
    ;;
"")
    command -v cargo >/dev/null 2>&1 || die "cargo is not on PATH; build elsewhere and pass --binary <path>"
    (cd "$here" && cargo build --release --locked --quiet)
    binary="${CARGO_TARGET_DIR:-$here/target}/release/$name"
    ;;
*)
    die "unknown option: $1 (try --binary <path> or --uninstall)"
    ;;
esac
[ -x "$binary" ] || die "no executable at $binary"

thurbox-cli extension install "$here" --home "$home_dir" >/dev/null
mkdir -p "$home_dir/bin" "$path_dir"
cp "$binary" "$home_dir/bin/$name.tmp" && mv "$home_dir/bin/$name.tmp" "$home_dir/bin/$name"
# Claude's hook calls the binary by name, so it has to be on PATH.
ln -sf "$home_dir/bin/$name" "$path_dir/$name"

# `extension install` activates, but Thurbox records its active set
# read-modify-write, so a heartbeat tick at the same moment can drop it.
thurbox-cli extension activate auto-continue >/dev/null
thurbox-cli --json extension list | grep -q '"name":"auto-continue"' || die "the extension did not register"

case ":$PATH:" in
*":$path_dir:"*) ;;
*) echo "note: $path_dir is not on PATH; Claude's hook will not find $name until it is" >&2 ;;
esac
[ -d "$HOME/.claude" ] || echo "note: no ~/.claude yet; run install again once Claude Code is installed" >&2
echo "auto-continue installed (off until enabled): $home_dir"
echo "  turn on for one session:  $name enable <session>"
echo "  turn on everywhere:       $name config set enabled on"
