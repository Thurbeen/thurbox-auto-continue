#!/usr/bin/env bash
# Record media/demo.gif: the real Thurbox TUI with the plugin installed, in a
# throwaway sandbox (demo/sandbox.sh) with a fake Claude and no account.
#
# asciinema records the terminal and agg renders the cast; tmux presses the
# keys, so what the GIF shows is the interface answering real key presses. The
# one-time `run` grant is pressed in Settings → Interface too, in a pre-roll
# that is not recorded (that tab prints the sandbox's absolute path).
#
# What it shows: auto-continue off by default; turning it on for one session;
# a custom continue message and delay; a usage limit (the fake Claude's), the
# episode armed with its countdown; the send, and the message arriving.
#
# Needs: asciinema, agg, tmux, python3, plus what demo/sandbox.sh needs, and
# thurbox + thurbox-cli (THURBOX / THURBOX_CLI, else on PATH).
#
#   demo/record.sh [output.gif]
#
# SNAP=<dir> saves the screen at each step, for finding out why a frame looks
# wrong after the sandbox is gone.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$REPO/media/demo.gif}
THURBOX=${THURBOX:-$(command -v thurbox || true)}
export THURBOX_CLI=${THURBOX_CLI:-$(command -v thurbox-cli || true)}
COLS=${COLS:-110}
ROWS=${ROWS:-28}
SNAP=${SNAP:-}

missing=
for tool in asciinema agg tmux python3 "$THURBOX" "$THURBOX_CLI"; do
    command -v "$tool" >/dev/null || missing="$missing ${tool:-thurbox}"
done
[ -n "$missing" ] && { echo "missing:$missing" >&2; exit 2; }

S=$("$REPO/demo/sandbox.sh")
ln -sf "$THURBOX" "$S/bin/thurbox"
export TMUX_TMPDIR="$S/tmux"
TM=(tmux -L tac-demo)
cleanup() {
    "${TM[@]}" kill-server 2>/dev/null || true
    tmux -L thurbox kill-server 2>/dev/null || true
    rm -rf "$S"
}
trap cleanup EXIT INT TERM

k() { "${TM[@]}" send-keys -t 0 "$1"; sleep "${2:-0.7}"; }
type_slowly() {
    local text=$1 i
    for ((i = 0; i < ${#text}; i++)); do
        if [ "${text:i:1}" = " " ]; then
            "${TM[@]}" send-keys -t 0 Space
        else
            "${TM[@]}" send-keys -t 0 -l "${text:i:1}"
        fi
        sleep 0.07
    done
}
screen() { "${TM[@]}" capture-pane -p -t 0; }
snap() { [ -n "$SNAP" ] && mkdir -p "$SNAP" && screen >"$SNAP/$1.txt"; true; }
# Wait for text on screen, rather than guessing how long something takes.
wait_for() {
    for _ in $(seq 1 "${2:-60}"); do
        screen | grep -qF -- "$1" && return 0
        sleep 1
    done
    echo "demo/record.sh: never saw «$1»" >&2
    screen >&2
    exit 1
}
# Move the pane's cursor down to a row.
select_row() {
    for _ in $(seq 1 10); do
        screen | grep -qF "▸ $1" && return 0
        k j 0.4
    done
    echo "demo/record.sh: no row $1" >&2
    exit 1
}

# ── Pre-roll: the one-time grant, pressed where a user presses it ───────────
"${TM[@]}" new-session -d -x "$COLS" -y "$ROWS" "$S/enter.sh thurbox"
wait_for "Auto-continue · F11"
k F6 1.5
k ']' 1.5
# Idempotent: `t` toggles, so it is pressed only with the file's row selected,
# and only while the line under the list does not already say it is granted.
grant() {
    for _ in $(seq 1 60); do
        if screen | grep '│▸' | grep -qF "$1"; then
            screen | grep -qF "granted; t revokes" && return 0
            k t 1.5
        else
            k j 0.3
        fi
    done
    echo "demo/record.sh: could not trust $1" >&2
    screen >&2
    exit 1
}
grant 85_auto_continu
for _ in $(seq 1 40); do k k 0.1; done
grant 86_auto_continu
k Escape 1
k C-q 2
"${TM[@]}" kill-server 2>/dev/null || true

# ── The recording ────────────────────────────────────────────────────────────
CAST="$S/demo.cast"
"${TM[@]}" new-session -d -x "$COLS" -y "$ROWS" \
    "asciinema rec --overwrite --quiet --cols $COLS --rows $ROWS --command '$S/enter.sh thurbox' '$CAST'"
wait_for "Auto-continue · F11"
sleep 2
snap 0-start

# Off by default: the pane opens on this machine's global switch.
k F11 1
wait_for "off (default)"
sleep 3
snap 1-off

# On for one session.
select_row parser
sleep 1.5
k e 0.5
wait_for "saved parser on"
sleep 2.5
snap 2-on

# A message and a delay of its own.
k m 1
type_slowly "carry on with the parser"
sleep 0.8
k Enter
wait_for "saved parser message"
sleep 1.2
k d 1
type_slowly "5"
sleep 0.6
k Enter
wait_for "saved parser delay"
sleep 2.5
snap 3-custom

# A usage limit, as the fake Claude draws one: the window resets in 20s.
ID=$("$S/enter.sh" thurbox-cli --json session list | python3 -c \
    'import json,sys; print(next(s["id"] for s in json.load(sys.stdin) if s["name"] == "parser"))')
mkdir -p "$S/ctl/$ID"
printf 'limit:five_hour:20:cancelled\nok\n' >"$S/ctl/$ID/script"
"$S/enter.sh" thurbox-cli session send "$ID" -- "tidy up the parser" >/dev/null
wait_for "armed · five_hour"
snap 4-armed
# The countdown, until the send is due (reset + 5s).
for _ in $(seq 1 40); do
    screen | grep -qF "send in" || break
    sleep 1
done
sleep 1
# The TUI's heartbeat fires due one-shots once a minute; run that same tick
# now rather than film up to a minute of waiting. Nothing else is done for it.
"$S/enter.sh" thurbox-cli automation tick >/dev/null
wait_for "sent " 60
sleep 3
snap 5-sent

# And in the session itself: the message, typed once.
k F11 1.5
wait_for "carry on with the parser" 10
sleep 4
snap 6-agent

k C-q 3
for _ in $(seq 1 20); do [ -s "$CAST" ] && break; sleep 1; done

# A GIF loops, so its last frame stays up as long as any other: cut the cast
# where the TUI starts to tear down (leaving the alternate screen).
python3 - "$CAST" <<'TRIM'
import json, sys

path = sys.argv[1]
lines = open(path).read().splitlines()
for i, line in enumerate(lines[1:], start=1):
    if "\x1b[?1049l" in json.loads(line)[2]:
        open(path, "w").write("\n".join(lines[:i]) + "\n")
        break
TRIM

# The GIF is published: refuse to render a cast that carries anything naming
# the machine it was made on. The sandbox lives under the real home directory,
# so a path leaking anywhere in the interface would carry these.
python3 - "$CAST" "$(id -un)" "$(uname -n)" "$REPO" "$(dirname "$S")" "$HOME" <<'PRIVATE'
import json, sys

path, *needles = sys.argv[1:]
text = "".join(json.loads(line)[2] for line in open(path).read().splitlines()[1:])
found = [n for n in needles if n and n in text]
if found:
    sys.exit(f"the recording contains {found!r}; not rendering it")
PRIVATE

mkdir -p "$(dirname "$OUT")"
agg --font-size 14 --idle-time-limit 2 --last-frame-duration 5 --fps-cap 10 "$CAST" "$OUT"
ls -lh "$OUT"
