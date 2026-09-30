# thurbox-auto-continue

Headless, LLM-free auto-continue for Claude sessions in [Thurbox](https://github.com/Thurbeen/thurbox).

When a Claude Code session hits its usage limit, this extension waits for the
quota window to reset and then types `continue` into that session, once. It
works with the Thurbox TUI closed. No model runs at any point: every decision
is made by one small deterministic binary.

It is **off by default**, globally and for every session.

## How it works

```text
Claude hits its limit
  └─ StopFailure hook (matcher "rate_limit", in ~/.claude/settings.json)
       └─ thurbox-auto-continue record          reads the hook JSON on stdin
            ├─ proves a quota episode from the transcript
            │    quotaLimits.status == "rejected" and a finite resetsAt
            │    (a transient 429 has no quotaLimits and is ignored)
            ├─ session signal --state idle        (the session is waiting, not stuck)
            ├─ session meta auto-continue.episode = armed
            └─ automation create at:<resetsAt + delay_secs>  → one per session

Thurbox's heartbeat (every 60 s, TUI open or closed) fires the one-shot
  └─ thurbox-auto-continue fire <session>
       ├─ gates: extension active · enabled · local Claude session ·
       │         no newer transcript row · no newer hook_state_at
       ├─ episode = claimed                      (claim before acting)
       ├─ capture → classify the screen
       │    empty prompt → type → re-capture: composer holds exactly the message?
       │    our text → as is · limit menu → Esc once · anything else → skip
       ├─ the same gates again, then Enter
       └─ episode = sent | unconfirmed | skipped:<why>

Every 15 minutes: thurbox-auto-continue sweep
  └─ an episode the hook missed, a send whose one-shot never fired, a stale claim
```

What it will not do:

- **Send twice.** A per-session OS file lock serialises every run on a machine,
  and the episode is written `claimed` before the pane is touched. A run that
  crashes after the claim loses its send; the next run marks it `abandoned` and
  never resends.
- **Talk over anyone.** It skips when the transcript has any user or assistant
  row newer than the rejection (you typed, Claude resumed by itself, or
  something restarted the session), when the session reported a state since,
  when the composer holds other text, when Claude's own countdown is armed
  (`Continuing automatically at …`, never cancelled), and on any screen it
  does not recognise.
- **Act on anything but Claude.** Other agents are never touched, and `enable`
  refuses them.
- **Hang.** Every `thurbox-cli` call has a 10 s timeout, and a run ends after
  45 s whatever happens.
- **Log secrets.** `auto-continue.log` holds session ids, states and reasons —
  never the message, the screen or the transcript.

## Install

On every machine that runs Claude sessions, including each shared host:

```sh
git clone https://github.com/Thurbeen/thurbox-auto-continue
cd thurbox-auto-continue
./install.sh                    # builds with cargo; or: ./install.sh --binary <path>
```

It needs `thurbox-cli` (Thurbox 2.36.2 or newer) and Claude Code on the
machine. The installer builds the binary, runs `thurbox-cli extension install`,
copies the binary into the extension home and links it into `~/.local/bin`,
which must be on `PATH` for Claude's hook to find it.

The install:

- merges one `StopFailure` hook into `~/.claude/settings.json`, next to
  whatever is there. Thurbox's own status hooks reach Claude through a separate
  `--settings` file, and Claude runs both (checked against Claude Code
  2.1.285);
- seeds `~/.config/thurbox/auto-continue/config.toml`, never overwritten;
- adds one `cron:*/15` Exec automation, the sweep.

## Turn it on

```sh
thurbox-auto-continue enable <session>          # one session
thurbox-auto-continue config set enabled on     # every Claude session
thurbox-auto-continue disable <session>         # one session off, whatever the global says
thurbox-auto-continue clear <session>           # back to the global default
thurbox-auto-continue status                    # what is on, and each session's last episode
```

A session's own toggle beats the global setting in both directions.

`config.toml`:

| key | default | meaning |
|---|---|---|
| `enabled` | `false` | the global default |
| `delay_secs` | `300` | how long after the reset to send |
| `message` | `"continue"` | what is typed |
| `windows` | `["five_hour"]` | windows acted on; add `"seven_day"` for the weekly one |
| `max_attempts` | `2` | sends allowed while each one is rejected again, then `gave-up` |
| `on_menu` | `"escape"` | the limit menu open: close it once, or `"skip"` |
| `confirm_secs` | `20` | how long to wait for `working` after Enter before `unconfirmed` |

`thurbox-auto-continue status --json` is the stable shape (`"schema": 1`) for
scripts and for the interface plugin to come.

## Stop it, remove it

- **Kill switch:** `thurbox-cli extension deactivate auto-continue`. Every
  pending send checks this first and skips. `extension activate auto-continue`
  brings it back.
- **Off everywhere:** `thurbox-auto-continue config set enabled off` (sessions
  you enabled one by one stay on until you `disable` or `clear` them).
- **Uninstall:** `./install.sh --uninstall`. It runs `thurbox-auto-continue
  forget --all` (every toggle, episode and pending send), then
  `thurbox-cli extension uninstall auto-continue --purge`, which takes our hook
  out of `~/.claude/settings.json` and leaves your own entries there.

  One Thurbox behaviour to know about: uninstalling any extension that merged
  into `~/.claude/settings.json` also removes every entry there whose command
  contains `thurbox-cli session signal`. If you wired such a hook by hand in
  that file, re-add it after uninstalling. Our own hook never contains that
  text, for exactly this reason.

## Supported

| target | status |
|---|---|
| Linux, local tmux sessions | supported; the end-to-end suite runs here |
| macOS, local tmux sessions | supported; the same suite runs in CI on macOS |
| Thurbox 2.36.2 and the latest release | tested in CI |
| Claude Code 2.1.285 | the transcript rows, screens and hook payload in `tests/fixtures/` were recorded from it |
| SSH / WSL shared hosts | **not yet**: sessions on another host are skipped. The design is host-local — install on the host itself — and needs its own verification |
| native Windows (psmux) | **unverified**: the Windows paths exist (`src/platform.rs`) but have never run |
| Thurbox TUI controls | **not yet**: badge, toggles and the settings switch are a separate plugin |

## Limitations

- **Claude only, subscription only.** API-key, Bedrock and Vertex sessions
  never record a quota window, so they are never acted on.
- **Coupled to Claude's internals.** The transcript fields (`quotaLimits` is
  undocumented) and the English screen text are Claude's, not an API. A Claude
  release that changes them makes this skip (`unknown-screen`, no episode)
  rather than send blind.
- **Claude's own resume wins.** Claude 2.1.285 arms its own countdown when a
  limit hits. While that countdown shows, this never sends and never presses
  Esc; if Claude resumes by itself, its resume is in the transcript and this
  skips. It acts when the countdown was cancelled, never armed, or lost (a
  restarted session).
- **The sweep cannot mark the session idle.** A `session signal --session` from
  outside the session's pane reaches Thurbox's database but not the pane, and
  Thurbox's next headless tick copies the pane's stale `working` back. So only
  the hook signals. An episode the sweep recovers keeps reading `working`, and
  fleet's `refuel` may restart it; that restart's prompt lands in the
  transcript and this skips, so the session still gets one prompt, not two.
- **Nothing fires until Thurbox's heartbeat runs.** After a reboot, the keeper
  starts with the TUI or any `thurbox-cli extension activate`.
- **`~/.claude` only.** The hook goes into `~/.claude/settings.json`; a Claude
  run with a different `CLAUDE_CONFIG_DIR` does not see it.
- **Timing.** The send lands at the reset plus `delay_secs`, within a minute
  of the heartbeat. The last check and Enter are two calls apart, so a user
  typing in that instant can still race it; the composer check before Enter
  narrows that, it does not close it.

## Development

```sh
cargo test          # unit tests, and the end-to-end suite against a real thurbox-cli
```

The end-to-end tests need `tmux` and a `thurbox-cli` (on `PATH`, or named by
`TAC_THURBOX_CLI`). Each test gets its own sandbox under `target/tmp/`:
`HOME`, `THURBOX_CONFIG_DIR` and `THURBOX_DATA_DIR` point into it, it gets its
own tmux server, and the Thurbox TUI is never started. `claude` is replaced by
`examples/fake_claude.rs`, which draws Claude 2.1.285's screens, writes its
transcript rows and runs hooks from both `settings.json` and `--settings`, as
Claude does. Nothing reaches your own Thurbox, tmux server or Claude config.
