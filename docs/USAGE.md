# Using auto-continue

The full user guide. The [README](../README.md) has only the short path. Scripts
and interfaces that read or change settings should use
[CLI-CONTRACT.md](CLI-CONTRACT.md), which defines the JSON output, exit codes
and validation.

- [How it works](#how-it-works)
- [Install](#install)
- [Settings and precedence](#settings-and-precedence)
- [Turning it on safely](#turning-it-on-safely)
- [The Thurbox TUI plugin](#the-thurbox-tui-plugin)
- [Monitoring and outcomes](#monitoring-and-outcomes)
- [Shared SSH and WSL hosts](#shared-ssh-and-wsl-hosts)
- [Stop, disable, uninstall](#stop-disable-uninstall)
- [When it does not send](#when-it-does-not-send)
- [Troubleshooting](#troubleshooting)
- [Support and limits](#support-and-limits)

## How it works

```text
Claude hits its usage limit
  └─ StopFailure hook (~/.claude/settings.json) → thurbox-auto-continue record
       ├─ proves a quota episode from the transcript (a transient 429 is ignored)
       ├─ marks the session idle, writes episode = armed
       └─ schedules one Thurbox automation at  reset + delay_secs

Thurbox heartbeat (every 60 s, TUI open or closed)
  └─ thurbox-auto-continue fire <session>
       ├─ gates: extension active · enabled · Claude session · nobody moved on
       ├─ episode = claimed                     (claimed before anything is typed)
       ├─ reads the screen: empty prompt → type the message, check it landed
       │                    limit menu → Esc once · anything else → skip
       ├─ the same gates again, then Enter
       └─ episode = sent | unconfirmed | skipped:<why>

Every 15 minutes: thurbox-auto-continue sweep
  └─ picks up an episode the hook missed, or a send that never fired
```

No model runs anywhere in this. `auto-continue.log`, in the extension home,
has one line per decision. It records session ids, states and reasons, and
never the message, the screen or the transcript.

## Install

**Requirements:** Linux or macOS, `thurbox-cli` from Thurbox 2.36.2 or newer,
and Claude Code. Building needs `cargo`; without it, build the binary somewhere
else and pass it with `--binary`.

```sh
git clone https://github.com/Thurbeen/thurbox-auto-continue
cd thurbox-auto-continue
./install.sh                    # builds with cargo
./install.sh --binary <path>    # or installs a binary you already built
```

The installer:

- runs `thurbox-cli extension install` and activates the extension;
- copies the binary into the extension home (`~/.config/thurbox/auto-continue/bin`)
  and links it into `~/.local/bin`. **`~/.local/bin` must be on `PATH`**,
  because Claude's hook calls the binary by name. The installer warns if it is
  not;
- merges one `StopFailure` hook into `~/.claude/settings.json` and leaves your
  own entries alone. If `~/.claude` does not exist yet, install Claude Code and
  run the installer again;
- seeds `~/.config/thurbox/auto-continue/config.toml` with every key commented
  out. It never overwrites that file;
- adds the sweep, a `cron:*/15` automation.

Nothing is sent until you turn it on. Running the installer again is safe: it
replaces the binary and keeps your settings.

## Settings and precedence

Every setting has a global value. `enabled`, `message` and `delay_secs` can also
be set for a single session, and each of them resolves in this order (first
match wins):

```text
session override  ──▶  global (config.toml)  ──▶  built-in default
```

A session override wins in both directions: a session set to `off` stays off
when the global switch is on, and one set to `on` runs when the global switch
is off.

| key | default | per session | meaning |
|---|---|---|---|
| `enabled` | `false` | yes | whether to send at all |
| `message` | `continue` | yes | what is typed: one line of 1–200 characters, not starting with `/`, `!` or `#` |
| `delay_secs` | `300` | yes | seconds after the reset before sending, 0–86400 |
| `windows` | `["five_hour"]` | no | windows acted on; add `"seven_day"` for the weekly one |
| `max_attempts` | `2` | no | sends allowed while each is rejected again, then `gave-up` |
| `on_menu` | `"escape"` | no | the limit menu is open: close it once, or `"skip"` |
| `confirm_secs` | `20` | no | how long to wait for `working` after Enter (at most 20) |

```sh
thurbox-auto-continue config set <key> <value>                     # global, in config.toml
thurbox-auto-continue config set <key> <value> --session <session> # one session
thurbox-auto-continue config unset <key> --session <session>       # back to the global value
thurbox-auto-continue config show [--session <session>]            # what applies, and where from

thurbox-auto-continue enable <session>    # = config set enabled on --session <session>
thurbox-auto-continue disable <session>   # = config set enabled off --session <session>
thurbox-auto-continue clear <session>     # = config unset enabled --session <session>
```

`<session>` is a session id, name or unique id prefix, as `thurbox-cli` takes
it. You can also edit `config.toml` by hand, since it is read again on every run.

When a change takes effect:

- `enabled` and `message` are read at send time, so a change also reaches a
  send that is already scheduled. Switching a session off before its send
  cancels it.
- `delay_secs` is read when a limit is recorded, so a change applies from the
  next limit.

## Turning it on safely

1. Start with one session: `thurbox-auto-continue enable <session>`. Only
   Claude sessions can be enabled. For any other agent it is refused.
2. Check the result with `thurbox-auto-continue status` or `config show --session <session>`.
3. Switch it on globally only once you want every Claude session covered:
   `thurbox-auto-continue config set enabled on`. To keep a session out, run
   `disable` on it.

Choose a message that is safe to type unattended. Claude receives it as a new
user turn. The message may never start with `/`, `!` or `#`, which Claude
would read as a command, a shell escape or a memory.

## The Thurbox TUI plugin

`ui/` holds two interface plugins. Install them after the extension:

| file | what it is |
|---|---|
| `ui/plugins/85_auto_continue.lua` | the pane: every Claude session's state, plus the global and per-session settings with an editor |
| `ui/plugins/86_auto_continue_badge.lua` | the badge: a mark at the right edge of each Claude session's row |

```sh
thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-auto-continue --as ui/plugins/85_auto_continue.lua
thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-auto-continue --as ui/plugins/86_auto_continue_badge.lua
thurbox-cli plugin check        # loaded: … auto-continue, auto-continue-badge
```

This clones the repository into your interface directory
(`thurbox-cli plugin dir`). You do not need to edit `layout.lua`: the pane
shares the `center` slot with the agent pane.

**One-time grant.** Both files read and change settings by running
`thurbox-auto-continue`, so each needs Thurbox's `run` capability, and only you
can grant it: `Ctrl+,` (or `F6`) → `]` (Interface) → select
`thurbox-auto-continue/ui/plugins/85_auto_continue.lua` → `t`, then the same
for `86_auto_continue_badge.lua`. Until then the pane says so and runs nothing,
and the session list shows no badge. The plugin never grants itself anything.

**Opening the pane:** press **F11**, click the **Auto-continue** pill in the
action band, or use `Ctrl+P`. Press F11 again to leave it.

| key | on *Every Claude session* | on a Claude session |
|---|---|---|
| `j` / `k` | move | move |
| `e` | on / off (config.toml) | cycle: global → on → off → global |
| `m` / `d` | edit the global message / delay | edit the session's message / delay |
| `M` / `D` | — | message / delay back to the global value |
| `r` | read everything again | read everything again |

A field opens empty and shows the current value as a placeholder. `enter`
saves and `esc` cancels. An invalid value is refused before anything runs, and
if the CLI refuses a value, the pane shows `✗` with the CLI's reason. Sessions
that are not Claude's, and sessions on a host that cannot be reached, have no
controls.

**The Settings switch.** The pane also adds `auto-continue.enabled` to Thurbox's
Settings panel, off by default. It mirrors config.toml's global `enabled`:
flip either one and the other follows on the next interface event. If both
changed, config.toml wins. Pressing `e` on the global row sets both.

The pane keeps no settings of its own. It reads and writes through the CLI, so
a change made in the pane, in a shell or by a script is the same change. The
pane and the badge each read status at most every 10 seconds while they are on
screen.

**Removing the plugin** leaves your settings in place, and the headless
extension keeps acting on them:

```sh
thurbox-cli plugin remove thurbox-auto-continue/ui/plugins/85_auto_continue.lua
thurbox-cli plugin remove thurbox-auto-continue/ui/plugins/86_auto_continue_badge.lua
```

The session list's right-click menu has no auto-continue entry yet, because
that menu is fixed in Thurbox's own session list. The
`auto-continue.toggle_selected` action is in the `Ctrl+P` palette, and you can
bind it to a key from `F1`.

## Monitoring and outcomes

`thurbox-auto-continue status` lists the global switch and each session's
switch, its latest episode and its last outcome. Add `--json` for the shape in
[CLI-CONTRACT.md](CLI-CONTRACT.md#status---json-schema-2).

| episode | meaning | badge |
|---|---|---|
| — | on, no limit yet | `↻` |
| `armed` | waiting for the reset plus the delay | `⏸ 42m`, or `⏸ due` once late |
| `claimed` | sending right now | `…` |
| `sent` | typed and confirmed working | `✓` |
| `unconfirmed` | typed, but the session did not report `working` in time | `?` |
| `skipped` | not sent, with a reason (see [below](#when-it-does-not-send)) | `↷` superseded, `⊘` otherwise |
| `gave-up` | `max_attempts` sends were each rejected again | `✗` |
| `abandoned` | a run crashed after its claim; never resent | `✗` |

The pane shows the same information in more detail: the window and its reset,
the next send with a countdown, the attempt, and how the episode ended.
**Superseded** means that you, Claude's own resume or a restart moved the
session on first. A session that is off and has no episode gets no badge.

## Shared SSH and WSL hosts

A session on an SSH host or WSL distro is handled by the extension installed
**on that host**, using the host's own Thurbox database and heartbeat. So:

1. Install the extension on each host (run `./install.sh` there). The TUI
   plugin is needed only on the machine where you run the TUI.
2. Turn it on from any machine that lists the session. From your laptop,
   `enable`, `disable`, `clear`, `config set|unset|show --session` and `status`
   reach the host's install through `thurbox-cli session exec` and change the
   host's database.
3. The laptop never records, schedules or sends for a remote session. Only one
   machine can send for each session, so two laptops watching the same host
   cannot both send, and the host keeps working while the laptop is off.

This needs the host to share its sessions (`share_sessions = true` in
`hosts.toml`, Thurbox's default). When it cannot work, `status` and the setters
say why instead of guessing: sharing is off, the host has no extension or an
old one, the host does not know the session, or it reaches the session through
yet another host. The reasons are listed in
[CLI-CONTRACT.md](CLI-CONTRACT.md#sessions-on-shared-hosts). `status` asks each
host once per run, so it is only as fast as that host's ssh.

## Stop, disable, uninstall

| goal | command |
|---|---|
| pause everything now (kill switch) | `thurbox-cli extension deactivate auto-continue` |
| resume after the kill switch | `thurbox-cli extension activate auto-continue` |
| off for every session that has no override | `thurbox-auto-continue config set enabled off` |
| off for one session | `thurbox-auto-continue disable <session>` |
| remove the TUI plugin | the two `plugin remove` commands [above](#the-thurbox-tui-plugin) |
| uninstall | `./install.sh --uninstall`, from the clone |

Every pending send checks the kill switch first and skips. Sessions you enabled
one by one stay on after `config set enabled off`, until you `disable` or
`clear` them.

`./install.sh --uninstall` runs `thurbox-auto-continue forget --all`, which
removes every per-session setting, episode and pending send, then
`thurbox-cli extension uninstall auto-continue --purge`, which removes the hook
from `~/.claude/settings.json` and leaves your own entries. Each machine forgets
only its own sessions, so run it on each host too. Remove the TUI plugin first.

One Thurbox behaviour to know about: uninstalling any extension that merged
into `~/.claude/settings.json` also removes every entry there whose command
contains `thurbox-cli session signal`. If you added such a hook by hand in that
file, add it again after uninstalling.

## When it does not send

Every check fails closed. When in doubt, it skips and records why:

| reason | what happened |
|---|---|
| `extension-inactive` | the kill switch is on, or the extension was uninstalled |
| `disabled` | the session was switched off before the send |
| `transcript-moved` | the transcript has a newer user or assistant turn: you typed, Claude resumed by itself, or the session restarted |
| `hook-state-moved` | the session reported a state since the limit |
| `armed-wait` | Claude's own `Continuing automatically at …` countdown is showing |
| `other-text` | the composer holds text that is not ours |
| `limit-menu` | the limit menu is open and `on_menu = "skip"`, or it stayed open after one Esc |
| `unknown` | the screen was not one it recognises |
| `verify-failed`, `moved-before-enter` | the typed text did not land as expected, or the session moved between typing and Enter |
| `window` | the limit's window is not in `windows` |
| `session-gone`, `stopped` | the session no longer exists, or is stopped |
| `error` | a `thurbox-cli` call failed after the claim; see the log |

It also never sends twice. A per-session file lock serialises runs, and the
episode is marked `claimed` before the pane is touched. A run that crashes after
the claim loses its send: the next run marks it `abandoned` and does not resend.
Every `thurbox-cli` call has a 10 second timeout, and a run ends after 45
seconds no matter what. An invalid `config.toml` stops all sends until it is
fixed, and `status` lists the problem under `warnings`.

## Troubleshooting

| symptom | check |
|---|---|
| nothing happens at a limit | `thurbox-auto-continue status`: is the session on, and does it print `extension: INACTIVE`? Look at the episode's reason, and the last lines of `~/.config/thurbox/auto-continue/auto-continue.log` |
| no episode is ever recorded | `~/.local/bin` must be on the `PATH` Claude runs with. A Claude started with another `CLAUDE_CONFIG_DIR` does not see the hook. For an enabled session, the 15-minute sweep also picks up a limit the hook missed |
| pane says it needs the `run` capability | grant it once, as in [the plugin section](#the-thurbox-tui-plugin) |
| pane says the binary is not installed | run `./install.sh` on that machine, and make sure `~/.local/bin` is on `PATH` |
| no badge on session rows | grant `86_auto_continue_badge.lua` its `run` too, and check that the session is Claude's and on |
| Settings switch and config.toml disagree | the pane shows `Settings switch: on · config.toml: off` until the next interface event; config.toml wins |
| a remote session shows `unavailable` | `status <session>` names the reason; see [shared hosts](#shared-ssh-and-wsl-hosts) |
| the installer says the extension would not stay active | run `thurbox-cli extension activate auto-continue` |
| nothing fires after a reboot | the heartbeat starts with the TUI or any `thurbox-cli extension activate` |

## Support and limits

| target | status |
|---|---|
| Linux, macOS (local tmux sessions) | supported; the end-to-end suite runs on both in CI |
| Thurbox 2.36.2 and the latest release | tested in CI |
| SSH / WSL shared hosts | supported, host-local; tested with a second sandboxed Thurbox behind a stand-in `ssh` and `wsl.exe`, not yet against a real remote machine or WSL distro |
| native Windows (psmux), including psmux hosts | not supported yet |
| hosts with `share_sessions = false` | unsupported: reported by `status`, refused by the setters, never sent |

- **Claude subscriptions only.** API-key, Bedrock and Vertex sessions never
  record a quota window, so they are never acted on.
- **Coupled to Claude's internals.** The transcript fields and the English
  screen text belong to Claude, not to an API. The fixtures were recorded from
  Claude Code 2.1.285. If a Claude release changes them, this skips rather than
  sends blind.
- **Not yet proven on a real account.** The whole path is tested against a
  fake Claude that replays recorded screens and transcripts. No send has yet
  been observed at a real account's quota reset.
- **Timing.** The send lands at the reset plus `delay_secs`, within a minute
  (the heartbeat). The last check and Enter are two calls apart, so someone
  typing at that exact moment can still race it.
- **The sweep cannot mark a session idle.** An episode that only the sweep
  recovers keeps reading `working`. If something else restarts that session,
  its new prompt lands in the transcript and this skips, so the session still
  gets one prompt, not two.
