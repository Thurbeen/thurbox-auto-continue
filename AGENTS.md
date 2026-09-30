# AGENTS.md

Rules for anyone changing this repository, human or coding assistant. The
feature targets Claude Code sessions. Your own tooling can be anything.

Read the authoritative docs rather than copies of them:
[README.md](README.md) is the front door, [docs/USAGE.md](docs/USAGE.md) has the
user-facing behaviour, [docs/CLI-CONTRACT.md](docs/CLI-CONTRACT.md) has the
interface contract, and the `//!` header of each module holds the details.

## What this is

A Thurbox extension that types one message into a Claude Code session once its
usage-limit window resets, headless (with the TUI closed), plus a Thurbox TUI
plugin that shows and edits its settings. Every decision is deterministic code.

## Source map

| path | role |
|---|---|
| `src/engine.rs` | `record` (the hook), `fire` (the scheduled send), `sweep` (the fallback) |
| `src/transcript.rs`, `src/screen.rs` | proving a quota episode from the transcript; classifying the pane |
| `src/episode.rs`, `src/lock.rs` | episode state machine in session meta; per-session OS lock |
| `src/config.rs` | `config.toml`, session overrides, precedence and validation |
| `src/remote.rs` | shared SSH/WSL hosts: delegation through `thurbox-cli session exec` |
| `src/thurbox.rs` | the only module that runs `thurbox-cli` (JSON, hard timeouts) |
| `src/platform.rs` | every platform-specific decision |
| `src/main.rs`, `src/log.rs` | CLI (clap) and the decision log |
| `extension.toml`, `extension/`, `install.sh` | Thurbox manifest, hook merge, seeded config, installer |
| `ui/plugins/85_*.lua`, `ui/plugins/86_*.lua`, `ui/lib/model.lua` | the pane, the badge, their pure model |
| `examples/fake_claude.rs`, `tests/fixtures/` | the fake Claude and the Claude Code 2.1.285 recordings it replays |
| `tests/e2e_*.rs`, `tests/support/`, `tests/ui/` | end-to-end suites, the sandbox, the Lua suites |
| `demo/` | reproducible recording of `media/demo.gif` |

## Invariants

These rules must hold after every change. A change that weakens one needs a
test that proves the new behaviour, and it must be named in the PR.

- **No LLM at runtime.** No model call, no network service, no inference.
- **Off by default.** Nothing is sent unless `enabled` resolves to on:
  session override → `config.toml` → default `false`.
- **One writer per session.** Only the machine whose Thurbox database owns the
  session records, claims and sends. A laptop never acts on `ssh:*` or `wsl:*`
  sessions. Remote settings are written by the host's own install.
- **Send at most once.** Take the lock, then write `claimed` before touching
  the pane. A crash may lose a send, but must never repeat one.
- **Fail closed.** An unrecognised screen, unreadable gate, invalid config,
  newer transcript row or state, Claude's own countdown, or foreign composer
  text all mean skip and record why. Never guess.
- **No secrets in output.** The log, `status` and the plugin never carry
  transcript text, screen contents or received prompts.
- **The plugin holds no state.** It reads and writes only through the CLI
  contract, never grants itself capabilities, and never edits `ui.json`.

## Tests come first

1. For a bug or a new behaviour, write the end-to-end test that reproduces it,
   and see it fail for the right reason before fixing anything.
2. Tests run in an isolated sandbox (`tests/support/mod.rs`): their own `HOME`,
   `THURBOX_CONFIG_DIR`, `THURBOX_DATA_DIR`, tmux server and `THURBOX_UI_DIR`,
   under `target/tmp/`. Claude is `examples/fake_claude.rs`. Never test against
   a live Claude account, your own Thurbox database, tmux server or `~/.claude`.
3. Shared hosts are tested with a second sandbox behind a stand-in `ssh` or
   `wsl.exe` (`Sandbox::add_host`). Only the network is faked.
4. A new Claude screen or transcript shape starts as a recorded fixture under
   `tests/fixtures/`, with its Claude Code version in the path. Scrub it
   first: no real prompts, paths, account data or host names.

The suites need `tmux`, Lua 5.4 (`lua5.4`, or `TAC_LUA`) and `thurbox-cli`
(on `PATH`, or `TAC_THURBOX_CLI`). `tests/ui/run.sh` runs the Lua suites alone.

## Platforms

Linux and macOS are supported, and CI runs the full suite on both against
Thurbox 2.36.2 and the latest release. Native Windows is deferred: the branches
in `src/platform.rs` are unverified. Never describe Windows as supported, and
never describe a real-account quota reset as verified, until a test or
recorded run shows it.

## Gate and publishing

`.publish.yaml` is the gate, and CI runs the same checks:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
shellcheck install.sh demo/*.sh tests/ui/run.sh
stylua --check ui tests/ui
cargo test --locked
```

PR titles are conventional commits (`.github/workflows/pr-title.yml`) because
squash merging makes the title the commit on `main`. Add no dependency and no
runtime permission without saying why in the PR. Nothing private goes in
commits, fixtures or docs, including host names, usernames, home paths or
account details.

## Boundaries with Thurbox

- This repository depends on the public `thurbox-cli` and the plugin API only.
  Do not patch Thurbox core or a user's installed interface directory from here.
- What needs Thurbox changes (for example a right-click session-menu entry,
  which comes from Thurbox's bundled `10_sessions.lua`) is an upstream change
  in the Thurbox repository. Do not work around it here, and do not document
  it as available before it ships.
- Raising `min_thurbox_version` in `extension.toml` also means updating the CI
  matrix and the requirements in the README.
- A change to `status --json` or the setters follows
  [docs/CLI-CONTRACT.md](docs/CLI-CONTRACT.md)'s rules: add fields freely, and
  bump `schema` for anything renamed or removed.
- When a user-visible command, key or screen changes, update
  [docs/USAGE.md](docs/USAGE.md), keep the README's short path correct, and
  re-record the demo with `demo/record.sh` if what it shows changed.
