# thurbox-auto-continue

Headless, LLM-free auto-continue for Claude sessions in [Thurbox](https://github.com/Thurbeen/thurbox).

When a Claude Code session hits its usage limit, this extension waits for the
quota window to reset and then types `continue` into that session, once. It
works with the Thurbox TUI closed. No model runs at any point: every decision
is made by one small deterministic binary.

It is **off by default**, globally and for every session.

A [Thurbox TUI plugin](#in-the-thurbox-tui) shows and changes all of it from
the interface: a settings and monitor pane, a badge on each Claude session's
row, and the global switch in Thurbox's Settings panel.

![The Thurbox TUI with the auto-continue pane open beside the session list. Auto-continue starts off for every session; F11 opens the pane, the parser session is switched on and given its own message and a 5 second delay. A fake usage limit arms an episode: the row shows the five-hour window resetting and the send counting down, and the session list's badge counts down with it. At the reset the headless extension types "carry on with the parser" into the fake Claude, and the badge turns into a check mark.](media/demo.gif)

The recording is the real TUI with a fake Claude in a throwaway sandbox (no
account, no real sessions); [demo/record.sh](demo/record.sh) makes it again.

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

On a shared SSH or WSL host all of that runs **on the host**, by the host's own
install, against the host's own Thurbox database:

```text
laptop                                        shared host (share_sessions = true)
──────                                        ───────────────────────────────────
session list: worker  ssh:devbox   ◀─mirror─  session list: worker  local-tmux
record / fire / sweep: skip ssh:* and wsl:*   Claude's hook → record → one-shot
                                              host heartbeat → fire → types once
thurbox-auto-continue enable worker
  └─ thurbox-cli session exec worker -- ─────▶ thurbox-auto-continue config set
       thurbox-auto-continue … --delegated       enabled on --session worker
                                                 (stored in the host's database)
thurbox-auto-continue status worker ─────────▶ status --json, relayed field by field
```

One machine owns each session, so two laptops watching one host never send
twice, and the host keeps going while the laptop is off.

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

On every machine that runs Claude sessions, including each shared SSH host and
WSL distro (see [Shared hosts](#shared-hosts)):

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

`enabled`, `message` and `delay_secs` can also be set for one session, and a
session's own value beats the global one in both directions:

```sh
thurbox-auto-continue config set message 'resume the task' --session <session>
thurbox-auto-continue config set delay_secs 60 --session <session>
thurbox-auto-continue config unset message --session <session>   # back to the global value
thurbox-auto-continue config show --session <session>            # what applies, and where it comes from
```

A message must be one line of 1–200 characters and must not start with `/`,
`!` or `#` (Claude reads those as a command, a shell or a memory). `delay_secs` is 0–86400. A
message or switch changed after a limit is recorded still applies to that
send; a changed delay applies from the next limit.

`config.toml` (every key optional):

| key | default | per session | meaning |
|---|---|---|---|
| `enabled` | `false` | yes | the global default |
| `delay_secs` | `300` | yes | how long after the reset to send |
| `message` | `"continue"` | yes | what is typed |
| `windows` | `["five_hour"]` | no | windows acted on; add `"seven_day"` for the weekly one |
| `max_attempts` | `2` | no | sends allowed while each one is rejected again, then `gave-up` |
| `on_menu` | `"escape"` | no | the limit menu open: close it once, or `"skip"` |
| `confirm_secs` | `20` | no | how long to wait for `working` after Enter before `unconfirmed` (at most 20) |

## In the Thurbox TUI

`ui/` is a Thurbox interface plugin, two files installed from this repository:

| file | what it is |
|---|---|
| `ui/plugins/85_auto_continue.lua` | **the pane**: every Claude session's state, and the settings of the global switch and of each session, with an editor. Beside the agent pane; **F11** (or the **Auto-continue** pill in the action band, or `Ctrl+P`) opens it and leaves it |
| `ui/plugins/86_auto_continue_badge.lua` | **the badge**: a mark at the right edge of each Claude session's row in the session list |

Install both, after the extension itself:

```sh
thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-auto-continue --as ui/plugins/85_auto_continue.lua
thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-auto-continue --as ui/plugins/86_auto_continue_badge.lua
thurbox-cli plugin check          # both load: auto-continue, auto-continue-badge
```

That clones this repository into your interface directory
(`thurbox-cli plugin dir`), executables included — nothing in it runs until you
say so. The pane needs no `layout.lua` edit: it is an alternate of the `center`
slot, like the Shell tab.

**Grant it once.** Both files read and change settings by running
`thurbox-auto-continue`, so each needs Thurbox's `run` capability, which only
you can grant: `Ctrl+,` (or `F6`) → `]` → select
`thurbox-auto-continue/ui/plugins/85_auto_continue.lua` → `t`, then the same for
`86_auto_continue_badge.lua`. Until then the pane says exactly that and runs
nothing, and the session list carries no badge. The plugin never grants itself
anything and never edits `ui.json`.

The pane, as the recording above captured it (the empty rows trimmed):

```text
┏ ▸ Auto-continue ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ extension active                                                               ┃
┃── Sessions ────────────────────────────────────────────────────────────────────┃
┃   Every Claude session  config.toml  off (default) · 1 of 2 Claude sessions on ┃
┃ ▸ parser             on  armed · five_hour resets in 12s · send in 17s         ┃
┃   docs               off no limit yet                                          ┃
┃   review             —   not a Claude session                                  ┃
┃                                                                                ┃
┃── parser ──────────────────────────────────────────────────────────────────────┃
┃   enabled    on   session  (global off)                                        ┃
┃   message    carry on with the parser   session                                ┃
┃   delay      5 s   session                                                     ┃
┃   window     five_hour · resets in 12s                                         ┃
┃   next send  in 17s   attempt 1 of 2                                           ┃
┃   last       none yet — this episode is still open                             ┃
┃ ✓ saved parser delay                                                           ┃
┃                                                                                ┃
┃ j/k move · e on/off/inherit · m message · d delay · M/D reset · r refresh      ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
```

| key | on the global row | on a Claude session |
|---|---|---|
| `j` / `k` | move | move |
| `e` | switch on / off (config.toml) | cycle: global → on → off → global |
| `m` | edit the message | edit the session's message |
| `d` | edit the delay | edit the session's delay |
| `M` / `D` | — | the message / the delay back to the global value |
| `r` | read everything again | read everything again |

A field opens empty with the current value as its placeholder; `enter` saves,
`esc` cancels. A message must be one line of 1–200 characters not starting
with `/`, `!` or `#`, a delay 0–86400 seconds: anything else is refused in the
pane before anything runs, and the CLI checks again — its refusal is shown in
the pane as `✗` with its reason. A session that is not Claude's, or one on a
host that cannot be asked, has no controls at all.

What the monitor tells apart: status still being read (`reading status…`), the
binary missing on this machine, the extension switched off (`nothing is
sent`), a session with no limit yet (`no limit yet`), and a session the last
status did not cover. An episode shows its window and reset, the next send
with a countdown (`due on the next heartbeat` once it is late), the attempt,
and how it ended — `sent`, `sent, not confirmed`, `skipped` with its reason,
`gave up` — with **superseded** when you, Claude's own resume or a restart
moved the session on first.

The badge:

| mark | means |
|---|---|
| `↻` | on, no limit yet |
| `⏸ 42m` / `⏸ due` | armed: the send in 42 minutes / at the next heartbeat |
| `…` | sending now |
| `✓` · `?` | sent · sent, not confirmed |
| `↷` · `⊘` | superseded · skipped for another reason |
| `✗` | gave up, or abandoned |

Nothing for a session that is off with no episode, one that is not Claude's, or
one on a host that cannot be asked.

How it works, briefly:

- **One source of truth.** The pane keeps no settings of its own. It reads
  `status --json` and `config show --json` and writes with `config set|unset`
  ([docs/CLI-CONTRACT.md](docs/CLI-CONTRACT.md)), through Thurbox's `run`, so a
  change from the pane, from a shell or from a script is the same change. The
  prompt text is shown only in the settings editor, from `config show`; the
  monitor never shows it, and nothing from a transcript is ever read.
- **Where it runs.** Thurbox runs a plugin's program in a session's directory,
  on that session's machine. The pane runs the CLI in a local session, and the
  CLI reaches a shared host itself (see [Shared hosts](#shared-hosts)): a
  remote session's switch lands in that host's database, and a host that cannot
  be asked is shown as `unavailable` with the reason. With no local session at
  all, a remote session is asked on its own host.
- **The Settings switch.** The pane declares one Thurbox setting,
  `auto-continue.enabled` (Settings panel, off by default), as the face of
  config.toml's global `enabled`, which is what the headless extension reads.
  The two are kept in step from event handlers and key presses — never from the
  render loop — by the pane's one `run` grant: flip the switch and the next
  interface event (a selection or focus change) writes `config set enabled`;
  change config.toml from a shell and the switch follows. When both changed,
  config.toml wins. Until then the pane says `Settings switch: on · config.toml:
  off`. Turning the global switch with `e` in the pane sets both at once.
- **Cost.** The pane and the badge each read `status --json` at most every 10
  seconds while they are drawn, and the pane re-reads right after a change.
  With shared hosts, one status is one `ssh` per host.

Not there yet:

- **The right-click session menu.** Thurbox's session list builds that menu
  from a fixed table in its own `ui/plugins/10_sessions.lua`, which no plugin
  can add to. The plugin's `auto-continue.toggle_selected` action (in the
  `Ctrl+P` palette, and bindable from `F1`) is what such an entry would run; it
  needs a change to Thurbox's bundled session list first.
- **A state in the pill.** The action band's pills carry fixed labels, so the
  pill opens the pane rather than showing the selected session's state; the
  badge does that.

Remove it: `thurbox-cli plugin remove thurbox-auto-continue/ui/plugins/85_auto_continue.lua`
and the same for `86_auto_continue_badge.lua`. That leaves your settings in
config.toml and the session meta, where the headless extension keeps using
them; [uninstalling the extension](#stop-it-remove-it) removes those.

## Shared hosts

A session on an SSH host or WSL distro is looked after by the extension
installed **on that host**: install it there as above, and turn it on there or
from any machine that lists the session. From the laptop, `enable`, `disable`,
`clear`, `config set|unset|show --session` and `status` reach the host's own
install through `thurbox-cli session exec` and change the host's database,
where the host's install reads it. The laptop itself never records, schedules
or sends for such a session.

It needs the host to share its sessions (`share_sessions = true` in
`hosts.toml`, Thurbox's default). Everything else is unsupported, and `status`
and the setters say which case it is rather than guess:

- `share_sessions = false`: the laptop drives the host, no host database owns
  the session, and nothing is sent to it;
- no extension on the host, or one too old to answer the laptop;
- a session the host does not know (made before it shared), or one it reaches
  through a further host;
- a native-Windows (psmux) host.

`status` asks each host once per run, so it is as slow as the host's ssh.

For scripts and the Thurbox plugin, [docs/CLI-CONTRACT.md](docs/CLI-CONTRACT.md)
is the contract: `status --json` (`"schema": 2`) with each session's effective
settings and where each comes from, its episode, next send and last outcome,
and the setters with their validation and exit codes, and how a session on a
shared host is reached. It carries no transcript text.

## Stop it, remove it

- **Kill switch:** `thurbox-cli extension deactivate auto-continue`. Every
  pending send checks this first and skips. `extension activate auto-continue`
  brings it back.
- **Off everywhere:** `thurbox-auto-continue config set enabled off` (sessions
  you enabled one by one stay on until you `disable` or `clear` them).
- **Uninstall:** remove the [TUI plugin](#in-the-thurbox-tui) first if you
  installed it, then `./install.sh --uninstall`. It runs `thurbox-auto-continue
  forget --all` (every per-session setting, episode and pending send), then
  `thurbox-cli extension uninstall auto-continue --purge`, which takes our hook
  out of `~/.claude/settings.json` and leaves your own entries there. A send
  still queued (a plain `extension uninstall`, without `forget --all`) fires
  and skips as `extension-inactive`. Each machine forgets only its own: run it
  on each host too.

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
| SSH / WSL shared hosts | supported, host-local (install on each host). The end-to-end suite runs a second real Thurbox as the host, reached through a stand-in `ssh` and `wsl.exe`; **not yet verified against a real remote machine or a real WSL distro** |
| hosts with `share_sessions = false`, legacy and psmux hosts | **unsupported**: reported by `status` and refused by the setters, never sent |
| native Windows (psmux) | **unverified**: the Windows paths exist (`src/platform.rs`) but have never run |
| Thurbox TUI plugin | Thurbox 2.36.2 and the latest release: its Lua suites run against each one's own `lib/`, and the end-to-end suite installs it with `plugin install` and drives it against the real CLI, on Linux and macOS |

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
cargo test          # unit tests, the plugin's Lua suites, and the end-to-end suite against a real thurbox-cli
tests/ui/run.sh     # the plugin's Lua suites alone: model, pane, badge
```

The end-to-end tests need `tmux`, Lua 5.4 (`lua5.4` on PATH, or named by
`TAC_LUA`) and a `thurbox-cli` (on `PATH`, or named by `TAC_THURBOX_CLI`). Each test gets its own sandbox under `target/tmp/`:
`HOME`, `THURBOX_CONFIG_DIR` and `THURBOX_DATA_DIR` point into it, it gets its
own tmux server, and the Thurbox TUI is never started. `claude` is replaced by
`examples/fake_claude.rs`, which draws Claude 2.1.285's screens, writes its
transcript rows and runs hooks from both `settings.json` and `--settings`, as
Claude does. Nothing reaches your own Thurbox, tmux server or Claude config.

`tests/e2e_remote.rs` gives a test two such sandboxes, a laptop and a shared
host, each with its own database, tmux server and install. The laptop reaches
the host through a stand-in `ssh` or `wsl.exe` on its PATH that runs the
command under the host sandbox's environment, so Thurbox's real delegation,
mirror and `session exec` paths run and only the network is faked.

The plugin is tested three ways, none of which needs a terminal:

- `tests/ui/test_*.lua` load thurbox's own `lib/` — the one the `thurbox-cli`
  under test ships, seeded with `plugin new` — and render the pane, the badge
  and Thurbox's real session list against fake CLI answers
  (`tests/ui/fixtures/`, in the contract's shapes). The harness withholds what
  the kernel withholds (`os`, `io`, an untrusted `run`), records any write a
  render makes, and routes a chord Thurbox's own panes bind globally away from
  the pane, as the kernel does.
- `tests/e2e_ui.rs` installs the plugin with `thurbox-cli plugin install` into
  a disposable `THURBOX_UI_DIR`, checks it with `plugin check`, and drives the
  installed pane through `tests/ui/live.lua`, its `run` executed for real
  against the sandbox's Thurbox, a shared host and this crate's binary:
  configure, validate, a limit episode sent by the heartbeat with no TUI, the
  Settings mirror, and uninstall.
- `demo/record.sh` records `media/demo.gif` in the real TUI:

  ```sh
  demo/record.sh                   # needs asciinema, agg, tmux, python3, sqlite3, cargo
  THURBOX=… THURBOX_CLI=… demo/record.sh   # another thurbox than the one on PATH
  demo/sandbox.sh                  # the sandbox alone; its enter.sh opens a shell in it
  ```

  It builds its sandbox under `~/.cache/thurbox-auto-continue-demo`, installs
  both halves the way this README does, presses the one-time grant in
  Settings → Interface off camera, and refuses to render a recording that
  contains the recording machine's user name, host name or home path.
