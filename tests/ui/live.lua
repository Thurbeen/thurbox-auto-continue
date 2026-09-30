-- Drive the INSTALLED pane and badge against a real thurbox, for tests/e2e_ui.rs.
--
-- Everything the pane touches is real except the terminal: the plugin files
-- and lib/ come from the interface directory `plugin install` wrote (UI), the
-- session list from `thurbox-cli session list`, and every `run` the pane asks
-- for is executed with `sh -c` in that session's directory — the kernel's
-- runner for a local session — under the environment this process was given,
-- which is the test's sandbox. So a key pressed here reaches the real binary
-- and the real Thurbox database, exactly as it would from the TUI.
--
-- Steps arrive on stdin, one per line:
--
--   trust | untrust        grant or withhold `run` (default: granted)
--   settle                 render, answer every run for real, repeat until quiet
--   select <name>          move the pane's cursor to that row
--   key <chord>            press it, e.g. `key m`, `key ctrl+u`, `key enter`
--   type <text>            type the rest of the line
--   event <name>           deliver a kernel event
--   setting <id> <bool>    what the Settings panel holds (`auto-continue.enabled true`)
--   expect <text>          the pane's screen must contain it
--   reject <text>          the pane's screen must not contain it
--   badge <session> <text> that session's row in thurbox's list must contain it
--   nobadge <session> <t>  … must not
--   command <verb> <text>  a queued command of that verb mentions <text>
--   expire                 every answer goes stale, as its TTL passing would
--   screen                 print the pane
--
-- Exits non-zero on the first failed expectation, printing the screen.

local HERE = os.getenv("HERE")
local H = dofile(HERE .. "/harness.lua")

local function sh(cmd)
  local p = assert(io.popen(cmd))
  local out = p:read("a")
  p:close()
  return out
end

local model -- loaded through the plugin's own require, below
local sessions = {}

local function refresh_sessions()
  local list = model.decode(sh("thurbox-cli --json session list")) or {}
  sessions = {}
  for _, s in ipairs(list) do
    sessions[#sessions + 1] = {
      id = s.id,
      name = s.name,
      agent = s.agent,
      status = "idle",
      backend = s.backend_type,
      host = (s.backend_type or ""):match("^%a+:(.+)$"),
      cwd = s.cwd,
      worktrees = 0,
      repos = {},
      display_order = 0,
    }
  end
  H.sessions = sessions
end

local function cwd_of(id)
  for _, s in ipairs(sessions) do
    if s.id == id then
      assert(model.is_local(s), "the pane asked to run on a remote session: " .. id)
      return s.cwd
    end
  end
  error("run asked in an unknown session " .. tostring(id))
end

H.now_ms = os.time() * 1000
H.publish()
model = H.env.require("thurbox-auto-continue.ui.lib.model")
refresh_sessions()
H.publish()

local ROOT = H.PLUGIN_ROOT
local pane = H.load(ROOT .. "/ui/plugins/85_auto_continue.lua", { trusted = true })
local badge = H.load(ROOT .. "/ui/plugins/86_auto_continue_badge.lua", { trusted = true })
local list = H.load(H.UI .. "/plugins/10_sessions.lua")
local W, HT = 110, 44

local function frame()
  H.now_ms = os.time() * 1000
  H.publish()
  return H.screen(H.render(pane, W, HT, true))
end

local function settle()
  for _ = 1, 20 do
    frame()
    H.publish()
    H.decorate(badge, H.render(list, 60, 30, false), 60, 30)
    H.flush()
    if H.live(cwd_of) == 0 then
      return
    end
  end
  error("the pane kept asking")
end

local function fail(what)
  io.stderr:write("live: " .. what .. "\n--- pane ---\n" .. frame() .. "\n")
  os.exit(1)
end

local function badge_line(name)
  H.publish()
  local tree = H.decorate(badge, H.render(list, 60, 30, false), 60, 30)
  local id
  for _, s in ipairs(sessions) do
    if s.name == name then
      id = s.id
    end
  end
  for _, node in ipairs(H.find(tree, function(n)
    return n.role == "row" and n.id == id
  end)) do
    return H.line_text(node.text[1])
  end
  return ""
end

for raw in io.lines() do
  local verb, rest = raw:match("^%s*(%S+)%s?(.*)$")
  if verb == nil or verb:sub(1, 1) == "#" then
    goto continue
  end
  if verb == "trust" or verb == "untrust" then
    pane.trusted = verb == "trust"
    badge.trusted = pane.trusted
  elseif verb == "settle" then
    settle()
  elseif verb == "select" then
    local found = false
    for _ = 1, 40 do
      H.key(pane, "k")
    end
    for _ = 1, 40 do
      if frame():find("▸ " .. rest, 1, true) then
        found = true
        break
      end
      H.key(pane, "j")
    end
    if not found then
      fail("no row " .. rest)
    end
  elseif verb == "key" then
    H.key(pane, rest)
  elseif verb == "type" then
    H.type(pane, rest)
  elseif verb == "event" then
    H.event(rest, {})
  elseif verb == "setting" then
    local id, value = rest:match("^(%S+)%s+(%S+)$")
    H.settings[id] = value == "true"
  elseif verb == "expect" then
    if not frame():find(rest, 1, true) then
      fail("expected «" .. rest .. "»")
    end
  elseif verb == "reject" then
    if frame():find(rest, 1, true) then
      fail("did not expect «" .. rest .. "»")
    end
  elseif verb == "badge" or verb == "nobadge" then
    local name, want = rest:match("^(%S+)%s+(.+)$")
    local line = badge_line(name)
    local has = line:find(want, 1, true) ~= nil
    if has ~= (verb == "badge") then
      fail(verb .. " " .. name .. " «" .. want .. "»: the row reads «" .. line .. "»")
    end
  elseif verb == "command" then
    local cverb, want = rest:match("^(%S+)%s+(.+)$")
    local hit = false
    for _, c in ipairs(H.commands) do
      if c.verb == cverb then
        local flat = {}
        for k, v in pairs(c.opts) do
          flat[#flat + 1] = tostring(k) .. "=" .. tostring(v)
        end
        table.sort(flat)
        if table.concat(flat, " "):find(want, 1, true) then
          hit = true
        end
      end
    end
    if not hit then
      fail("no " .. cverb .. " command with «" .. want .. "»")
    end
  elseif verb == "expire" then
    -- The kernel runs a program again once its answer is older than its TTL;
    -- dropping every answer is that, without the wait.
    H.runs = {}
    settle()
  elseif verb == "screen" then
    io.write(frame(), "\n")
  else
    fail("unknown step " .. raw)
  end
  ::continue::
end
os.exit(0)
