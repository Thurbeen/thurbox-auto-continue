# CLI contract for interfaces

What an interface (the Thurbox plugin, a script) may rely on. Everything here
is deterministic and runs no model. Run it on the machine the session lives on:
a session on a shared host is read and changed by that host's own install.

Stable means: fields and commands below are not renamed or removed without
bumping `schema`. New fields may appear at any time; ignore what you do not know.

## Settings and precedence

Three settings can be set per session. Each one resolves in this order, first
match wins:

1. the session's own override (Thurbox session meta, key in the table);
2. the global value in `config.toml`;
3. the built-in default.

| setting | session meta key | global | default | valid values |
|---|---|---|---|---|
| `enabled` | `auto-continue.enabled` | yes | `false` | `on` / `off` (also `true`/`false`, `yes`/`no`, `1`/`0`) |
| `message` | `auto-continue.message` | yes | `continue` | one line, 1–500 characters, no control characters, not starting with `/` or `!` (those run a Claude command or a shell) |
| `delay_secs` | `auto-continue.delay_secs` | yes | `300` | whole seconds, `0`–`86400` |

Global only, never per session: `windows`, `max_attempts`, `on_menu`,
`confirm_secs`.

A session override that fails validation (someone wrote the meta key by hand)
is ignored: the next level applies and `status` lists it under `warnings`.

When a change takes effect:

- `enabled` and `message` are read at send time, so a change reaches a send
  that is already scheduled. Turning a session off before its send means no send.
- `delay_secs` is read when a limit is recorded. A change applies from the next
  limit; an already scheduled send keeps its time.

`enabled on` is refused for a session that is not a local Claude session.

## Setters

```sh
thurbox-auto-continue config set <key> <value> [--session <ref>] [--json]
thurbox-auto-continue config unset <key> --session <ref> [--json]
thurbox-auto-continue config show [--session <ref>] [--json]
```

`<ref>` is a session id, name or unique id prefix, as `thurbox-cli` takes it.
Without `--session`, `set` writes `config.toml`. `unset` exists only per session:
it removes the override so the global value applies again.

`enable <ref>`, `disable <ref>` and `clear <ref>` stay as shortcuts for
`config set enabled on|off --session <ref>` and `config unset enabled --session <ref>`.

Exit codes: `0` done, `1` refused (validation, unknown session, not eligible),
`2` usage. With `--json`, stdout is one object either way:

```json
{"ok": true, "scope": "session", "session": "<id>", "key": "message", "value": "keep going"}
{"ok": true, "scope": "session", "session": "<id>", "key": "message", "value": null}
{"ok": false, "error": "`message` must not start with / or !: it would run a Claude command"}
```

`value` is the value now stored at that scope; `null` after an `unset`.

## `status --json`, schema 2

`thurbox-auto-continue status [<ref>] --json`. With `<ref>`, `sessions` holds
that one session.

```json
{
  "schema": 2,
  "extension_active": true,
  "global": {
    "enabled": { "value": false, "source": "default" },
    "message": { "value": "continue", "source": "default" },
    "delay_secs": { "value": 60, "source": "global" },
    "windows": ["five_hour"],
    "max_attempts": 2
  },
  "sessions": [
    {
      "id": "…", "name": "worker", "agent": "claude",
      "eligible": true, "ineligible_reason": null,
      "enabled": true,
      "settings": {
        "enabled": { "value": true, "source": "session" },
        "message": { "value": "keep going", "source": "session" },
        "delay_secs": { "value": 60, "source": "global" }
      },
      "overrides": { "enabled": "on", "message": "keep going", "delay_secs": null },
      "episode": {
        "state": "armed", "label": "armed", "reason": null,
        "window": "five_hour", "resets_at_ms": 1790765610000,
        "next_send_at_ms": 1790765670000,
        "attempt": 1, "max_attempts": 2,
        "sent_at_ms": null, "updated_at_ms": 1790762010000
      },
      "last_outcome": null,
      "warnings": []
    }
  ]
}
```

- `source` is `session`, `global` or `default`.
- `enabled` is the effective switch: `false` whenever `eligible` is false.
- `ineligible_reason`: `not-claude`, `remote` or `stopped`.
- `episode` is the latest limit episode, or `null`. `state` is one of `armed`,
  `claimed`, `sent`, `unconfirmed`, `skipped`, `gave-up`, `abandoned`; `reason`
  says why for `skipped` and `gave-up` (e.g. `other-text`, `armed-wait`,
  `transcript-moved`, `disabled`, `window`, `max-attempts`).
- `next_send_at_ms` is set only while `state` is `armed`.
- `last_outcome` is `{state, reason, label, at_ms}` once the episode has ended,
  else `null`.
- Times are epoch milliseconds.

What `status` never contains: transcript text or paths, screen contents, or the
prompts the session received. `message` is the user's own setting and is shown.
