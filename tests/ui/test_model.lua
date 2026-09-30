-- The plugin's pure half: reading the CLI contract, building its command
-- lines, validating input and wording what an episode did. No snapshot, no
-- kernel — just the module, loaded in the plugin sandbox.

local H = dofile(os.getenv("HERE") .. "/harness.lua")
H.publish()
local model = H.env.require("thurbox-auto-continue.ui.lib.model")

local NOW = 1790762010000
local MIN = 60 * 1000

H.test("decode reads the contract's JSON: objects, arrays, escapes, null as absent", function()
  local v = assert(model.decode('{"a":1,"b":[true,false,"x\\"y\\u00e9\\n"],"c":null,"d":{"e":-2.5e1}}'))
  H.eq(v.a, 1)
  H.eq(v.b[1], true)
  H.eq(v.b[2], false)
  H.eq(v.b[3], 'x"yé\n')
  H.eq(v.c, nil)
  H.eq(v.d.e, -25)
end)

H.test("decode refuses what is not JSON rather than guessing", function()
  local v, err = model.decode("error: no home directory")
  H.eq(v, nil)
  H.truthy(err, "an error")
  H.eq((model.decode('{"a":1} trailing')), nil, "trailing garbage")
  H.eq((model.decode("")), nil, "empty")
end)

H.test("every command line finds the installed binary and asks for JSON", function()
  local cmd = model.command({ "status", "--json" })
  H.contains(cmd, "$HOME/.config/thurbox/auto-continue}/bin/thurbox-auto-continue")
  H.contains(cmd, "command -v thurbox-auto-continue")
  H.contains(cmd, "'status' '--json'")
end)

H.test("a value is one shell word whatever it holds", function()
  -- Run the built line against a stand-in binary that prints its argv, one per
  -- line: what arrives must be exactly what was typed.
  local dir = os.getenv("SCRATCH")
  local bin = dir .. "/bin"
  os.execute("mkdir -p " .. H.quote(bin))
  local fake = bin .. "/thurbox-auto-continue"
  local f = assert(io.open(fake, "w"))
  f:write('#!/bin/sh\nfor a in "$@"; do printf "[%s]\\n" "$a"; done\n')
  f:close()
  os.execute("chmod +x " .. H.quote(fake))
  local tricky = [[it's "fine" $HOME `id` ; exit 1]]
  local cmd = model.set_cmd("message", tricky, "s-1")
  local pipe = assert(io.popen("env -i HOME=" .. H.quote(dir .. "/nohome") .. " PATH=" .. H.quote(bin .. ":/usr/bin:/bin") .. " sh -c " .. H.quote(cmd)))
  local out = pipe:read("a")
  pipe:close()
  H.eq(out, "[config]\n[set]\n[message]\n[" .. tricky .. "]\n[--session]\n[s-1]\n[--json]\n")
end)

H.test("the setters and readers spell the contract's commands", function()
  H.contains(model.set_cmd("delay_secs", "60", nil), "'config' 'set' 'delay_secs' '60' '--json'")
  H.contains(model.set_cmd("enabled", "on", "s-9"), "'config' 'set' 'enabled' 'on' '--session' 's-9' '--json'")
  H.contains(model.unset_cmd("message", "s-9"), "'config' 'unset' 'message' '--session' 's-9' '--json'")
  H.contains(model.show_cmd("s-9"), "'config' 'show' '--session' 's-9' '--json'")
  H.contains(model.show_cmd(nil), "'config' 'show' '--json'")
  H.contains(model.status_cmd(), "'status' '--json'")
end)

H.test("validate: a message is one line of 1-200 characters, not a command", function()
  H.eq((model.validate("message", "keep going")), "keep going")
  local _, e = model.validate("message", "")
  H.contains(e, "blank")
  _, e = model.validate("message", "   ")
  H.contains(e, "blank")
  _, e = model.validate("message", string.rep("x", 201))
  H.contains(e, "200")
  H.eq((model.validate("message", string.rep("é", 200))), string.rep("é", 200), "200 characters, not bytes")
  _, e = model.validate("message", "one\ntwo")
  H.contains(e, "one line")
  _, e = model.validate("message", "tab\there")
  H.contains(e, "control")
  for _, lead in ipairs({ "/", "!", "#" }) do
    _, e = model.validate("message", lead .. "clear")
    H.contains(e, "must not start with", lead)
  end
end)

H.test("validate: a delay is whole seconds, 0 to 86400", function()
  H.eq(model.validate("delay_secs", "60"), "60")
  H.eq(model.validate("delay_secs", " 0 "), "0")
  H.eq(model.validate("delay_secs", "86400"), "86400")
  for _, bad in ipairs({ "-5", "86401", "1.5", "abc", "", "1e3" }) do
    local v, e = model.validate("delay_secs", bad)
    H.eq(v, nil, bad)
    H.truthy(e, bad)
  end
  local _, e = model.validate("delay_secs", "-5")
  H.contains(e, "0")
end)

H.test("parse_run tells pending, failed, not installed, garbage and old apart from an answer", function()
  H.eq(model.parse_run(nil).kind, "pending")
  H.eq(model.parse_run({ state = "pending" }).kind, "pending")
  local r = model.parse_run({ state = "failed", error = "no session x to run it in" })
  H.eq(r.kind, "failed")
  H.contains(r.error, "no session")
  H.eq(model.parse_run({ state = "done", status = 127, stdout = "", stderr = "" }).kind, "missing-binary")
  r = model.parse_run({ state = "done", status = 0, stdout = "not json" })
  H.eq(r.kind, "bad-output")
  H.eq(model.parse_run({ state = "done", status = 0, stdout = '{"schema":1,"sessions":[]}' }).kind, "outdated")
  r = model.parse_run({ state = "done", status = 0, stdout = '{"schema":2,"sessions":[],"global":{}}' })
  H.eq(r.kind, "ok")
  H.eq(r.data.schema, 2)
  r = model.parse_run({ state = "done", status = 0, timed_out = true, stdout = "" })
  H.eq(r.kind, "failed")
  H.contains(r.error, "timed out")
end)

H.test("parse_write reads a setter's one JSON object, whether it worked or not", function()
  local w = model.parse_write({ state = "done", status = 0, stdout = '{"ok":true,"scope":"session","key":"message","value":"x"}' })
  H.eq(w.kind, "ok")
  w = model.parse_write({ state = "done", status = 1, stdout = '{"ok":false,"error":"`message` must not start with / or !"}' })
  H.eq(w.kind, "refused")
  H.contains(w.error, "must not start")
  w = model.parse_write({ state = "done", status = 2, stdout = "", stderr = "error: unexpected argument" })
  H.eq(w.kind, "refused")
  H.contains(w.error, "unexpected argument")
  H.eq(model.parse_write(nil).kind, "pending")
end)

H.test("durations read as a person says them", function()
  H.eq(model.duration(0), "now")
  H.eq(model.duration(45 * 1000), "45s")
  H.eq(model.duration(125 * 1000), "2m 5s")
  H.eq(model.duration(3 * 3600 * 1000 + 4 * MIN), "3h 4m")
  H.eq(model.duration(2 * 86400 * 1000 + 3600 * 1000), "2d 1h")
end)

H.test("reasons say what happened, and which ones mean somebody else resumed first", function()
  local text, superseded = model.reason("transcript-moved")
  H.truthy(superseded, "transcript-moved is superseded")
  H.contains(text, "resumed")
  _, superseded = model.reason("armed-wait")
  H.truthy(superseded, "Claude's own countdown")
  _, superseded = model.reason("hook-state-moved")
  H.truthy(superseded)
  _, superseded = model.reason("other-text")
  H.truthy(superseded, "somebody typing")
  _, superseded = model.reason("unknown-screen")
  H.eq(superseded, false)
  text = model.reason("something-new")
  H.contains(text, "something-new", "an unknown reason is shown as itself")
end)

local function row(over)
  local r = {
    id = "s-1",
    name = "worker",
    agent = "claude",
    eligible = true,
    enabled = true,
    settings = {},
    overrides = {},
    episode = nil,
    last_outcome = nil,
    warnings = {},
  }
  for k, v in pairs(over or {}) do
    r[k] = v
  end
  return r
end

H.test("badge: armed counts down to the send", function()
  local b = model.badge(row({ episode = { state = "armed", next_send_at_ms = NOW + 63 * MIN, resets_at_ms = NOW + 58 * MIN } }), NOW)
  H.contains(b.text, "1h 3m")
end)

H.test("badge: each outcome has its own mark", function()
  local function mark(state, reason)
    local r = row({ episode = { state = state, reason = reason }, last_outcome = { state = state, reason = reason } })
    return model.badge(r, NOW).text
  end
  H.contains(mark("sent"), "✓")
  H.contains(mark("unconfirmed"), "?")
  H.contains(mark("skipped", "transcript-moved"), "↷")
  H.contains(mark("skipped", "unknown-screen"), "⊘")
  H.contains(mark("gave-up", "max-attempts"), "✗")
  H.contains(mark("claimed"), "…")
end)

H.test("badge: on and waiting shows the switch; off shows nothing", function()
  H.contains(model.badge(row({}), NOW).text, "↻")
  H.eq(model.badge(row({ enabled = false }), NOW), nil)
end)

H.test("badge: never for a session that is not Claude's, nor one nobody can read", function()
  H.eq(model.badge(row({ agent = "codex", eligible = false, ineligible_reason = "not-claude", enabled = false }), NOW), nil)
  H.eq(model.badge(row({ eligible = false, ineligible_reason = "remote", enabled = false, host = { backend = "ssh:old", reason = "not-installed" } }), NOW), nil)
end)

H.test("reconcile mirrors a Settings change to config.toml, and config back to Settings", function()
  -- (setting, config, baseline) -> what to do. `write` goes to config.toml,
  -- `adopt` goes to the Settings switch.
  local function r(s, c, b)
    return model.reconcile(s, c, b)
  end
  -- Nothing known about config yet: do nothing.
  H.eq(r(true, nil, nil).write, nil)
  H.eq(r(true, nil, nil).adopt, nil)
  -- First sight, agreeing: remember.
  local a = r(false, false, nil)
  H.eq(a.baseline, false)
  H.eq(a.write, nil)
  -- First sight, disagreeing: config.toml is what the headless extension
  -- reads, so it wins.
  a = r(true, false, nil)
  H.eq(a.adopt, false)
  H.eq(a.write, nil)
  H.eq(a.baseline, false)
  -- The user flipped the Settings switch.
  a = r(true, false, false)
  H.eq(a.write, true)
  H.eq(a.baseline, true)
  -- Somebody ran `config set enabled on` outside the TUI.
  a = r(false, true, false)
  H.eq(a.adopt, true)
  H.eq(a.write, nil)
  -- Both moved, to the same value.
  a = r(true, true, false)
  H.eq(a.write, nil)
  H.eq(a.adopt, nil)
  H.eq(a.baseline, true)
  -- Both moved, apart: config wins.
  a = r(false, true, nil)
  H.eq(a.adopt, true)
  -- Settled.
  a = r(true, true, true)
  H.eq(a.write, nil)
  H.eq(a.adopt, nil)
end)

H.test("anchor: the CLI runs in a local session, so it can reach every host itself", function()
  local sessions = {
    { id = "r-1", backend = "ssh:devbox" },
    { id = "l-1", backend = "local-tmux" },
    { id = "l-2", backend = "local-tmux" },
  }
  H.eq(model.anchor(sessions, "r-1"), "l-1")
  H.eq(model.anchor(sessions, nil), "l-1")
  -- No local session at all: the remote session's own host answers for it,
  -- and nothing can answer for "this machine" globally.
  local only_remote = { { id = "r-1", backend = "ssh:devbox" }, { id = "r-2", backend = "wsl:u" } }
  H.eq(model.anchor(only_remote, "r-2"), "r-2")
  H.eq(model.anchor(only_remote, nil), nil)
  H.eq(model.anchor({}, nil), nil)
end)

H.finish()
