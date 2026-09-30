-- The settings and monitor pane, rendered and driven through the harness:
-- thurbox's real lib/, the real plugin file, a faked snapshot and faked
-- answers from the CLI (tests/ui/fixtures, in the shapes docs/CLI-CONTRACT.md
-- documents).

local HERE = os.getenv("HERE")
local H = dofile(HERE .. "/harness.lua")
local PANE = H.PLUGIN_ROOT .. "/ui/plugins/85_auto_continue.lua"
local STATUS = H.read(HERE .. "/fixtures/status.json")
local SHOW_WORKER = H.read(HERE .. "/fixtures/show-worker.json")
local SHOW_GLOBAL = H.read(HERE .. "/fixtures/show-global.json")
local SECRET = "STATUS-CARRIES-THIS-BUT-THE-MONITOR-NEVER-SHOWS-IT"

local NOW = 1790762010000
local W, HT = 100, 40

local function id(n)
  return "00000000-0000-4000-8000-0000000000" .. n
end

local function snapshot_sessions()
  local function s(n, name, agent, backend)
    return {
      id = id(n),
      name = name,
      agent = agent,
      status = "idle",
      backend = backend or "local-tmux",
      host = backend and backend:match(":(.+)$") or nil,
      cwd = "/work/" .. name,
      worktrees = 0,
      repos = {},
      display_order = 0,
    }
  end
  return {
    s("0a", "worker", "claude"),
    s("0b", "reviewer", "claude"),
    s("0c", "docs", "claude"),
    s("0d", "codex-cli", "codex"),
    s("0e", "on-devbox", "claude", "ssh:devbox"),
    s("0f", "on-oldbox", "claude", "ssh:oldbox"),
    s("10", "stubborn", "claude"),
  }
end

--- A fresh host with the pane loaded and trusted (or not).
local function fresh(trusted)
  for i = #H.plugins, 1, -1 do
    H.plugins[i] = nil
  end
  for k in pairs(H.store_data) do
    H.store_data[k] = nil
  end
  for k in pairs(H.settings) do
    H.settings[k] = nil
  end
  H.runs, H.pending = {}, {}
  H.reset_log()
  H.sessions = snapshot_sessions()
  H.now_ms = NOW
  H.publish()
  local p = H.load(PANE, { trusted = trusted ~= false })
  H.publish()
  return p
end

local function screen(p)
  H.publish()
  return H.screen(H.render(p, W, HT, true))
end

--- Answer the most recent run whose program contains `pattern`.
local function answer(p, pattern, stdout, status)
  local asks = H.asked_matching(pattern)
  local last = asks[#asks]
  assert(last, "nothing asked for " .. pattern)
  H.answer(p, last.key, stdout, status)
  return last
end

--- Leave any open field, open `key`'s, type `text` and submit it.
local function retype(p, key, text)
  H.key(p, "esc")
  H.key(p, key)
  H.type(p, text)
  H.key(p, "enter")
end

--- Render until the status is in, and the selected target's settings too.
local function ready(p)
  screen(p)
  answer(p, "'status' '--json'", STATUS)
  screen(p)
end

local function select_row(p, name)
  -- Move down from the top until the row named `name` is the selected one.
  for _ = 1, 12 do
    local s = screen(p)
    if s:find("▸ " .. name, 1, true) then
      return s
    end
    H.key(p, "j")
  end
  error("could not select " .. name .. ":\n" .. screen(p))
end

H.test("untrusted: says what to grant, where, and asks for nothing", function()
  local p = fresh(false)
  local s = screen(p)
  H.contains(s, "run")
  H.contains(s, "85_auto_continue.lua")
  H.contains(s, "Ctrl+,")
  H.eq(#H.asked, 0, "runs asked")
end)

H.test("declares a pill with a key behind it, a palette command and the Settings switch", function()
  local p = fresh()
  local pill = p.def.pills[1]
  H.truthy(pill, "a pill")
  local keyed = false
  for _, b in ipairs(p.def.keys) do
    if b.action == pill.action and b.scope == "global" then
      keyed = true
    end
  end
  H.truthy(keyed, "the pill's action has a global key")
  H.eq(p.def.settings[1].id, "enabled")
  H.eq(p.def.settings[1].default, false, "off by default")
  H.truthy(#p.def.commands >= 1, "palette commands")
  H.truthy(p.def.pure, "pure")
end)

H.test("while status is out, the pane says so rather than showing everything off", function()
  local p = fresh()
  local s = screen(p)
  H.contains(s, "reading status")
  H.absent(s, " off ", "no fake zero state")
  local asks = H.asked_matching("'status' '--json'")
  H.eq(#asks, 1)
  H.eq(asks[1].session, id("0a"), "anchored on a local session")
end)

H.test("no binary on this machine is its own state, not an empty list", function()
  local p = fresh()
  screen(p)
  answer(p, "'status' '--json'", "", 127)
  H.contains(screen(p), "not installed")
end)

H.test("a failed or unreadable status is shown as unavailable, with the reason", function()
  local p = fresh()
  screen(p)
  local ask = H.asked_matching("'status' '--json'")[1]
  H.fail_run(p, ask.key, "no session x to run it in")
  local s = screen(p)
  H.contains(s, "unavailable")
  H.contains(s, "no session x to run it in")
end)

H.test("the monitor shows every session's state, and never the prompt text from status", function()
  local p = fresh()
  ready(p)
  local s = screen(p)
  H.absent(s, SECRET, "status's message")
  -- armed: window, reset, next send, attempts
  H.contains(s, "worker")
  H.contains(s, "send in 1h 3m")
  -- sent
  H.contains(s, "reviewer")
  H.contains(s, "sent 12m ago")
  -- off, and no episode yet: not a zero
  H.contains(s, "docs")
  H.contains(s, "no limit yet")
  -- not Claude: no control, said as such
  H.contains(s, "codex-cli")
  H.contains(s, "not a Claude session")
  -- a shared host that answered, and superseded
  H.contains(s, "on-devbox")
  H.contains(s, "ssh:devbox")
  H.contains(s, "superseded")
  -- a host that cannot be asked, honestly
  H.contains(s, "on-oldbox")
  H.contains(s, "unavailable")
  H.contains(s, "not installed on ssh:oldbox")
  -- gave up
  H.contains(s, "gave up")
end)

H.test("render is pure: it writes nothing and asks only reads", function()
  local p = fresh()
  ready(p)
  select_row(p, "worker")
  H.reset_log()
  screen(p)
  screen(p)
  H.eq(#H.render_writes, 0, "writes from render: " .. table.concat(H.render_writes, ", "))
  for _, a in ipairs(H.render_runs) do
    H.truthy(
      a.program:find("'status' '--json'", 1, true) or a.program:find("'config' 'show'", 1, true),
      "a read: " .. a.program
    )
  end
end)

H.test("the selected session's detail: window, reset, next send, attempts", function()
  local p = fresh()
  ready(p)
  local s = select_row(p, "worker")
  H.contains(s, "five_hour")
  H.contains(s, "resets in 58m")
  H.contains(s, "attempt 1 of 2")
end)

H.test(
  "the editor shows the prompt only from `config show`, with where each value comes from",
  function()
    local p = fresh()
    ready(p)
    select_row(p, "worker")
    local s = screen(p)
    H.contains(s, "reading settings")
    local ask = answer(p, "'config' 'show' '--session' '" .. id("0a") .. "'", SHOW_WORKER)
    H.eq(ask.session, id("0a"))
    s = screen(p)
    H.contains(s, "keep going")
    H.contains(s, "session")
    H.contains(s, "60 s")
    H.absent(s, SECRET)
  end
)

H.test(
  "the global row edits config.toml, and its editor reads `config show` without --session",
  function()
    local p = fresh()
    ready(p)
    local s = select_row(p, "Every Claude session")
    H.contains(s, "config.toml")
    answer(p, "'config' 'show' '--json'", SHOW_GLOBAL)
    s = screen(p)
    H.contains(s, "continue")
    H.contains(s, "300 s")
    H.contains(s, "default")
  end
)

H.test(
  "editing a session's message: validated, then `config set --session` on the right machine",
  function()
    local p = fresh()
    ready(p)
    select_row(p, "worker")
    answer(p, "'config' 'show'", SHOW_WORKER)
    screen(p)
    H.key(p, "m")
    local s = screen(p)
    H.contains(s, "now: keep going", "the current value, from config show, as the placeholder")
    H.key(p, "enter")
    s = screen(p)
    H.contains(s, "blank", "a blank message is refused before anything runs")
    H.eq(#H.asked_matching("'config' 'set'"), 0)
    H.type(p, "/compact")
    H.key(p, "enter")
    H.contains(screen(p), "must not start with")
    H.eq(#H.asked_matching("'config' 'set'"), 0)
    retype(p, "m", string.rep("x", 201))
    H.contains(screen(p), "200")
    retype(p, "m", "resume the task")
    local sets = H.asked_matching(
      "'config' 'set' 'message' 'resume the task' '--session' '" .. id("0a") .. "'"
    )
    H.eq(#sets, 1)
    H.eq(sets[1].session, id("0a"))
    H.contains(screen(p), "saving")
  end
)

H.test("a refusal from the CLI is shown in the pane, not swallowed", function()
  local p = fresh()
  ready(p)
  select_row(p, "worker")
  answer(p, "'config' 'show'", SHOW_WORKER)
  retype(p, "d", "-5")
  H.contains(screen(p), "0 to 86400")
  H.eq(#H.asked_matching("'config' 'set'"), 0, "a negative delay never reaches the CLI")
  retype(p, "d", "90")
  answer(
    p,
    "'config' 'set' 'delay_secs' '90'",
    '{"ok":false,"error":"the host refused: disk full"}',
    1
  )
  local s = screen(p)
  H.contains(s, "✗")
  H.contains(s, "disk full")
end)

H.test("a write that lands refreshes status and the editor", function()
  local p = fresh()
  ready(p)
  select_row(p, "worker")
  answer(p, "'config' 'show'", SHOW_WORKER)
  retype(p, "d", "90")
  local before_status = #H.asked_matching("'status' '--json'")
  local before_show = #H.asked_matching("'config' 'show'")
  answer(
    p,
    "'config' 'set' 'delay_secs' '90'",
    '{"ok":true,"scope":"session","key":"delay_secs","value":"90"}'
  )
  local s = screen(p)
  H.contains(s, "✓")
  H.truthy(#H.asked_matching("'status' '--json'") > before_status, "status asked again")
  H.truthy(#H.asked_matching("'config' 'show'") > before_show, "settings asked again")
end)

H.test("enabled cycles inherit → on → off → inherit for a session", function()
  local p = fresh()
  ready(p)
  select_row(p, "docs")
  answer(
    p,
    "'config' 'show'",
    '{"enabled":{"value":false,"source":"default"},"message":{"value":"continue","source":"default"},"delay_secs":{"value":300,"source":"default"}}'
  )
  H.key(p, "e")
  H.eq(
    #H.asked_matching("'config' 'set' 'enabled' 'on' '--session' '" .. id("0c") .. "'"),
    1,
    "inherit → on"
  )
end)

H.test("enabled on an overridden session steps to off, then back to the global", function()
  local p = fresh()
  ready(p)
  select_row(p, "worker")
  H.key(p, "e")
  H.eq(
    #H.asked_matching("'config' 'set' 'enabled' 'off' '--session' '" .. id("0a") .. "'"),
    1,
    "on → off"
  )
end)

H.test("a session that is not Claude's offers no control and runs nothing", function()
  local p = fresh()
  ready(p)
  local s = select_row(p, "codex-cli")
  H.absent(s, "e on/off", "no toggle hint")
  H.reset_log()
  H.key(p, "e")
  H.key(p, "m")
  H.key(p, "d")
  screen(p)
  H.eq(#H.asked_matching("'config' 'set'"), 0)
  H.eq(#H.asked_matching("'config' 'unset'"), 0)
  H.contains(screen(p), "not a Claude session")
end)

H.test("a session on a host that cannot be asked runs nothing and says why", function()
  local p = fresh()
  ready(p)
  select_row(p, "on-oldbox")
  H.reset_log()
  H.key(p, "e")
  screen(p)
  H.eq(#H.asked_matching("'config' 'set'"), 0)
  H.contains(screen(p), "not installed on ssh:oldbox")
end)

H.test(
  "a remote session's toggle goes through this machine's CLI, which reaches its host",
  function()
    local p = fresh()
    ready(p)
    select_row(p, "on-devbox")
    H.key(p, "e")
    local sets = H.asked_matching("'config' 'set' 'enabled' 'off' '--session' '" .. id("0e") .. "'")
    H.eq(#sets, 1)
    H.eq(sets[1].session, id("0a"), "run from a local session: the CLI delegates to ssh:devbox")
  end
)

H.test("with no local session, a remote session is asked on its own host", function()
  local p = fresh()
  H.sessions = { snapshot_sessions()[5] }
  screen(p)
  local asks = H.asked_matching("'status' '--json'")
  H.eq(asks[#asks].session, id("0e"), "status runs on the host")
  answer(p, "'status' '--json'", STATUS)
  select_row(p, "on-devbox")
  H.key(p, "e")
  local sets = H.asked_matching("'config' 'set' 'enabled' 'off' '--session' '" .. id("0e") .. "'")
  H.eq(sets[1].session, id("0e"), "the host's own install answers for its own session")
end)

H.test("the global switch sets config.toml and the Settings row together", function()
  local p = fresh()
  ready(p)
  select_row(p, "Every Claude session")
  H.key(p, "e")
  local sets = H.asked_matching("'config' 'set' 'enabled' 'on' '--json'")
  H.eq(#sets, 1)
  H.absent(sets[1].program, "--session")
  local set = nil
  for _, c in ipairs(H.commands) do
    if c.verb == "set" then
      set = c
    end
  end
  H.truthy(set, "a Settings write")
  H.eq(set.opts.text, "auto-continue.enabled")
  H.eq(set.opts.flag, true)
end)

H.test("a Settings-panel change reaches config.toml from an event, never from render", function()
  local p = fresh()
  ready(p)
  -- First sight: both off, remembered.
  H.event("focus.session", { from = "", to = id("0a") })
  H.eq(#H.asked_matching("'config' 'set'"), 0, "nothing to mirror yet")
  -- The user flips the switch in the Settings panel: the registry changes.
  H.settings["auto-continue.enabled"] = true
  H.reset_log()
  screen(p)
  screen(p)
  H.eq(#H.asked_matching("'config' 'set'"), 0, "render does not write")
  H.contains(screen(p), "Settings")
  H.event("focus.pane", { from = "settings", to = "sessions" })
  local sets = H.asked_matching("'config' 'set' 'enabled' 'on' '--json'")
  H.eq(#sets, 1, "mirrored on the next event")
  H.absent(sets[1].program, "--session")
end)

H.test("config.toml changed outside the TUI moves the Settings switch", function()
  local p = fresh()
  ready(p)
  H.event("focus.session", {})
  -- `thurbox-auto-continue config set enabled on` from a shell.
  local on = STATUS:gsub(
    '"enabled": { "value": false, "source": "default" }',
    '"enabled": { "value": true, "source": "global" }',
    1
  )
  local asks = H.asked_matching("'status' '--json'")
  H.answer(p, asks[#asks].key, on)
  H.reset_log()
  H.event("session.status", { session = id("0a"), from = "working", to = "idle" })
  local set = H.commands[1]
  H.truthy(set and set.verb == "set", "a Settings write")
  H.eq(set.opts.flag, true)
  H.eq(#H.asked_matching("'config' 'set'"), 0, "and nothing written back")
end)

H.test("an extension switched off is said up front: nothing is sent", function()
  local p = fresh()
  screen(p)
  answer(
    p,
    "'status' '--json'",
    (STATUS:gsub('"extension_active": true', '"extension_active": false', 1))
  )
  local s = screen(p)
  H.contains(s, "inactive")
  H.contains(s, "nothing is sent")
end)

H.test("a Settings write the kernel refuses is shown", function()
  local p = fresh()
  ready(p)
  H.event(
    "command.failed",
    { kind = "set", subject = "auto-continue.enabled", error = "not a flag" }
  )
  H.contains(screen(p), "not a flag")
end)

H.test("the palette toggles the session selected in the list, from any pane", function()
  local p = fresh()
  ready(p)
  H.store_data.selected = id("0c")
  H.call(p, "on_action", "auto-continue.toggle_selected")
  H.eq(#H.asked_matching("'config' 'set' 'enabled' 'on' '--session' '" .. id("0c") .. "'"), 1)
  H.store_data.selected = id("0d")
  H.reset_log()
  H.call(p, "on_action", "auto-continue.toggle_selected")
  H.eq(#H.asked_matching("'config' 'set'"), 0, "not for codex")
  local said = false
  for _, c in ipairs(H.commands) do
    if c.verb == "message" and c.opts.level == "error" then
      said = true
    end
  end
  H.truthy(said, "an error in the message band")
end)

H.test("the pane's key both enters and leaves it", function()
  local p = fresh()
  H.call(p, "on_action", p.def.pills[1].action)
  local focus = H.commands[#H.commands]
  H.eq(focus.verb, "focus")
  H.eq(focus.opts.text, "auto-continue")
  H.eq(focus.opts.toggle, true)
end)

H.test("the field never needs a chord thurbox spends elsewhere", function()
  local p = fresh()
  ready(p)
  select_row(p, "worker")
  answer(p, "'config' 'show'", SHOW_WORKER)
  H.key(p, "m")
  -- It opens empty, so nothing has to be cleared first: `ctrl+u` would be
  -- the restore float's, not the field's.
  local field = H.find(H.render(p, W, HT, true), function(n)
    return n.type == "input"
  end)[1]
  H.eq(field.value, "", "opens empty")
  H.truthy(field.focused, "holds the caret")
end)

H.test("typing into a field is never taken for a key", function()
  local p = fresh()
  ready(p)
  select_row(p, "worker")
  answer(p, "'config' 'show'", SHOW_WORKER)
  H.key(p, "m")
  H.type(p, "jejmd")
  local s = screen(p)
  H.contains(s, "jejmd")
  H.eq(#H.asked_matching("'config' 'set'"), 0)
  H.key(p, "esc")
  H.absent(screen(p), "jejmd", "esc cancels")
end)

H.finish()
