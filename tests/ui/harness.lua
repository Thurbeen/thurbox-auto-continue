-- A stand-in for thurbox's plugin host, for driving the pane and the badge
-- without a terminal.
--
-- What is real: the `lib/` of the thurbox release the plugin runs on (`UI`),
-- the plugin files themselves (`PLUGINS`), and — for the badge — thurbox's own
-- session list, whose tree the badge decorates. What is faked: the snapshot
-- (`thurbox.*`), the answers to `run`, and the kernel's queue behind `command`.
--
-- It holds the plugin to the kernel's rules rather than to looser ones, so a
-- test that passes here does not pass by accident:
--
--   * `os`, `io`, `debug`, `package`, `print`, `load` and friends are absent
--     from the environment the plugin runs in, as they are in the kernel;
--   * `run` is nil unless the test trusts the file;
--   * reading `state` or `store` hands back a copy, so a plugin that mutates a
--     read and forgets to write it back loses the change here too;
--   * every `store`/`state` write, `command` and `run` made while RENDERING is
--     recorded, so a test can assert a `pure` render wrote nothing;
--   * a key is resolved against the plugin's declared bindings before
--     `on_key` sees it, which is the order the kernel uses.
--
-- `run` answers come from the test (`H.answer`) or, in live mode, from really
-- running the program (`H.live`), which is how the end-to-end test drives the
-- pane against a sandboxed thurbox-cli and the real binary.

local H = {}

local UI = assert(os.getenv("UI"), "set UI to a thurbox release's ui/ directory")
-- Where `require("thurbox-auto-continue.…")` resolves: the repository, or the
-- working copy `plugin install` cloned into an interface directory.
local PLUGIN_ROOT = assert(os.getenv("PLUGIN_ROOT"), "set PLUGIN_ROOT to the plugin's repository")
H.UI, H.PLUGIN_ROOT = UI, PLUGIN_ROOT

local function read_file(path)
  local f = assert(io.open(path, "rb"), "cannot read " .. path)
  local body = f:read("a")
  f:close()
  return body
end

local function deepcopy(value)
  if type(value) ~= "table" then
    return value
  end
  local out = {}
  for k, v in pairs(value) do
    out[k] = deepcopy(v)
  end
  return out
end
H.deepcopy = deepcopy

---------------------------------------------------------------------------
-- text: display width in columns. Every glyph this plugin and thurbox's panes
-- draw is one column wide, so a codepoint count is exact for them.
---------------------------------------------------------------------------

local function width(s)
  return utf8.len(s) or #s
end

local function sub_cols(s, from, to)
  local out, i = {}, 0
  for _, code in utf8.codes(s) do
    i = i + 1
    if i >= from and i <= to then
      out[#out + 1] = utf8.char(code)
    end
  end
  return table.concat(out)
end

local text_api = {
  width = function(s)
    return width(tostring(s))
  end,
  truncate = function(s, cols, opts)
    s = tostring(s)
    local ellipsis, side = "…", "right"
    if type(opts) == "string" then
      ellipsis = opts
    elseif type(opts) == "table" then
      ellipsis = opts.ellipsis or ellipsis
      side = opts.side or side
      assert(
        side == "right" or side == "left" or side == "middle",
        "unknown side " .. tostring(side)
      )
    end
    local w = width(s)
    if w <= cols then
      return s
    end
    local keep = math.max(0, cols - width(ellipsis))
    if cols <= width(ellipsis) then
      return sub_cols(ellipsis, 1, cols)
    end
    if side == "left" then
      return ellipsis .. sub_cols(s, w - keep + 1, w)
    elseif side == "middle" then
      local head = math.ceil(keep / 2)
      return sub_cols(s, 1, head) .. ellipsis .. sub_cols(s, w - (keep - head) + 1, w)
    end
    return sub_cols(s, 1, keep) .. ellipsis
  end,
  pad = function(s, cols, align)
    s = tostring(s)
    local gap = math.max(0, cols - width(s))
    if align == "right" then
      return string.rep(" ", gap) .. s
    elseif align == "center" or align == "centre" then
      local left = math.floor(gap / 2)
      return string.rep(" ", left) .. s .. string.rep(" ", gap - left)
    end
    return s .. string.rep(" ", gap)
  end,
}

---------------------------------------------------------------------------
-- The environment a plugin chunk runs in.
---------------------------------------------------------------------------

local SAFE = {
  "string",
  "table",
  "math",
  "coroutine",
  "utf8",
  "pairs",
  "ipairs",
  "type",
  "tostring",
  "tonumber",
  "select",
  "next",
  "assert",
  "error",
  "pcall",
  "xpcall",
  "setmetatable",
  "getmetatable",
  "rawequal",
  "rawget",
  "rawset",
  "rawlen",
}

local G = {}
for _, name in ipairs(SAFE) do
  G[name] = _G[name]
end
G.text = text_api
H.env = G

-- Recorded side effects, and whether a render is running.
H.rendering = false
H.render_writes = {}
H.commands = {}
H.asked = {}
H.store_data = {}
H.settings = {}
H.state_version = 0

local function note_render_write(what)
  if H.rendering then
    H.render_writes[#H.render_writes + 1] = what
  end
end

--- A table whose reads are copies and whose writes are recorded.
local function copy_on_read(backing, label)
  return setmetatable({}, {
    __index = function(_, key)
      return deepcopy(backing[key])
    end,
    __newindex = function(_, key, value)
      note_render_write(label .. "." .. tostring(key))
      H.state_version = H.state_version + 1
      backing[key] = deepcopy(value)
    end,
  })
end

G.store = copy_on_read(H.store_data, "store")

G.command = function(verb, opts)
  note_render_write("command:" .. tostring(verb))
  H.commands[#H.commands + 1] = { verb = verb, opts = deepcopy(opts or {}) }
end

-- `require`: thurbox's `lib/` from UI, and this plugin's own modules from
-- PLUGIN_ROOT, each loaded once into the plugin environment.
local loaded = {}
G.require = function(name)
  if loaded[name] ~= nil then
    return loaded[name]
  end
  local path
  local own = name:match("^thurbox%-auto%-continue%.(.+)$")
  if own then
    path = PLUGIN_ROOT .. "/" .. own:gsub("%.", "/") .. ".lua"
  elseif name:match("^lib%.") then
    path = UI .. "/" .. name:gsub("%.", "/") .. ".lua"
  else
    error("require outside the interface directory: " .. name)
  end
  local chunk = assert(load(read_file(path), "@" .. path, "t", G))
  local value = chunk()
  loaded[name] = value == nil and true or value
  return loaded[name]
end

---------------------------------------------------------------------------
-- The snapshot.
---------------------------------------------------------------------------

local ROLES = {
  "accent",
  "accent_bright",
  "status_working",
  "status_blocked",
  "status_done",
  "status_idle",
  "status_error",
  "status_unreachable",
  "status_running",
  "status_unknown",
  "text_primary",
  "text_secondary",
  "text_muted",
  "border_focused",
  "border_unfocused",
  "role_name",
  "branch_name",
  "search_bar",
  "keybind_hint",
  "tool_allowed",
  "tool_disallowed",
  "danger",
  "selection_bg",
  "selection_fg",
  "modal_dim_bg",
  "modal_bg",
  "modal_border",
  "inverted_fg",
  "diff_added",
  "diff_removed",
  "diff_added_bg",
  "diff_removed_bg",
  "app_bg",
}

H.runs = {}
local plugins = {}
H.plugins = plugins

local function registry()
  local keys, settings = {}, {}
  for _, p in ipairs(plugins) do
    for _, b in ipairs(p.def.keys or {}) do
      keys[#keys + 1] = {
        plugin = p.def.name,
        action = b.action,
        key = b.key,
        default_key = b.key,
        desc = b.desc or "",
        scope = b.scope or "plugin",
        rebound = false,
        group = b.group or "",
      }
    end
    for _, s in ipairs(p.def.settings or {}) do
      local id = p.def.name .. "." .. s.id
      local value = H.settings[id]
      if value == nil then
        value = s.default
      end
      settings[#settings + 1] = {
        plugin = p.def.name,
        id = s.id,
        desc = s.desc or "",
        type = type(s.default),
        value = value,
        default = s.default,
      }
    end
  end
  return { keys = keys, settings = settings, sections = {} }
end

--- Build `thurbox` afresh, as a publish does: new tables, so identity memos in
--- the plugin see a change.
function H.publish(over)
  over = over or {}
  local roles = {}
  for _, r in ipairs(ROLES) do
    roles[r] = r
  end
  local t = {
    sessions = H.sessions or {},
    deleted = {},
    repos = {},
    agents = {},
    agent_default = "claude",
    settings = {
      features = { mouse = true, notifications = true },
      two_panel_min_cols = 80,
      three_panel_min_cols = 140,
      scrollback_lines = 1000,
    },
    hosts = {},
    tasks = {},
    automations = {},
    commands = {},
    diffs = {},
    links = {},
    printing = {},
    runs = {},
    granted = { run = H.trusted_now or false, program = false },
    metrics = { system = { cpu_percent = 0, memory_used = 0, memory_total = 0 }, sessions = {} },
    platform = { os = "linux", arch = "x86_64" },
    version = "test",
    reloads = 0,
    can_open_links = false,
    taken_at_ms = H.now_ms or 0,
    focus = H.focus or "sessions",
    selection = "",
    hover = {},
    plugins = {},
    ui_dir = "/ui",
    chrome = { status_rows = 1 },
    theme = { name = "test", roles = roles, choices = {} },
    registry = registry(),
  }
  for k, v in pairs(over) do
    t[k] = v
  end
  G.thurbox = t
  return t
end

---------------------------------------------------------------------------
-- Plugins.
---------------------------------------------------------------------------

--- Load a plugin file. `opts.trusted` grants `run` if it declares it.
function H.load(path, opts)
  opts = opts or {}
  local chunk = assert(load(read_file(path), "@" .. path, "t", G))
  local p = { path = path, state = {}, trusted = opts.trusted or false }
  p.state_proxy = copy_on_read(p.state, "state")
  -- The chunk runs with its own `state`, as the kernel enters it.
  G.state = p.state_proxy
  G.run = nil
  p.def = chunk()
  assert(type(p.def) == "table", path .. " did not return a table")
  plugins[#plugins + 1] = p
  return p
end

local function wants(p, cap)
  for _, c in ipairs(p.def.capabilities or {}) do
    if c == cap then
      return true
    end
  end
  return false
end

local run_impl = function(key, program, opts)
  assert(type(key) == "string" and key ~= "", "run: a key is required")
  assert(type(program) == "string" and program ~= "", "run: a program is required")
  opts = opts or {}
  local ask =
    { key = key, program = program, session = opts.session, ttl = opts.ttl, refresh = opts.refresh }
  H.asked[#H.asked + 1] = ask
  if H.rendering then
    H.render_runs[#H.render_runs + 1] = ask
  end
  local answers = H.runs[H.current.path]
  if not answers then
    answers = {}
    H.runs[H.current.path] = answers
  end
  if answers[key] == nil then
    answers[key] = { state = "pending" }
    H.pending[#H.pending + 1] = { plugin = H.current, key = key, ask = ask }
  end
end

--- Enter a plugin: its `state`, its `run` (only if trusted), its answers.
local function enter(p)
  H.current = p
  G.state = p.state_proxy
  local granted = p.trusted and wants(p, "run")
  G.run = granted and run_impl or nil
  G.thurbox.granted = { run = granted, program = false }
  G.thurbox.runs = deepcopy(H.runs[p.path] or {})
end

H.pending = {}
H.render_runs = {}

--- Render a plugin into a `width`×`height` rect.
function H.render(p, width, height, focused)
  enter(p)
  H.rendering = true
  local ok, tree = pcall(p.def.render, {
    width = width,
    height = height,
    focused = focused ~= false,
    frame = 1,
    name = p.def.name,
    slot = p.def.slot or "center",
    elapsed = 0,
  })
  H.rendering = false
  assert(ok, tree)
  return tree
end

--- Decorate `tree` with a decorator plugin.
function H.decorate(p, tree, width, height)
  enter(p)
  H.rendering = true
  local ok, out = pcall(p.def.decorate, tree, { width = width, height = height })
  H.rendering = false
  assert(ok, out)
  return out
end

--- Call a hook the way the kernel does (never a render).
function H.call(p, hook, ...)
  enter(p)
  local fn = p.def[hook]
  if not fn then
    return nil
  end
  local ok, result = pcall(fn, ...)
  assert(ok, result)
  return result
end

local function key_event(name)
  local char = nil
  if utf8.len(name) == 1 then
    char = name
  elseif name == "space" then
    char = " "
  elseif name:match("^ctrl%+.$") or name:match("^alt%+.$") then
    char = name:sub(-1)
  end
  -- A chord reaches `on_key` as its bare key plus modifier flags.
  local ctrl, alt = name:match("^ctrl%+") ~= nil, name:match("^alt%+") ~= nil
  local bare = name:gsub("^ctrl%+", ""):gsub("^alt%+", "")
  return { key = bare, char = char, ctrl = ctrl, alt = alt, shift = false, cmd = false }
end

-- The chords thurbox's stock panes claim globally (`ctrl+u` restores a
-- deleted session, `ctrl+k` moves the list, …), read from the release's own
-- plugin files. The kernel resolves those before a focused pane's `on_key`,
-- so a pane that expects one of them in a text field never receives it.
local stock_globals
local function stock_global(chord)
  if not stock_globals then
    stock_globals = {}
    local ls = io.popen("ls " .. H.quote(UI .. "/plugins"))
    for file in ls:lines() do
      if file:match("%.lua$") then
        local chunk = load(read_file(UI .. "/plugins/" .. file), "@" .. file, "t", G)
        local ok, def = pcall(chunk)
        if ok and type(def) == "table" then
          for _, b in ipairs(def.keys or {}) do
            if b.scope == "global" then
              stock_globals[b.key] = b.action
            end
          end
        end
      end
    end
    ls:close()
  end
  return stock_globals[chord]
end

--- Press a key while `p` has focus, routed as the kernel routes it: `p`'s own
--- binding first, then a global binding of a stock pane (which takes the key),
--- then `on_key`. Returns false, and records the action in `H.swallowed`, when
--- a stock pane took it.
H.swallowed = {}
function H.key(p, name)
  for _, b in ipairs(p.def.keys or {}) do
    if b.key == name then
      if H.call(p, "on_action", b.action) then
        return true
      end
      break
    end
  end
  local taken = stock_global(name)
  if taken then
    H.swallowed[#H.swallowed + 1] = taken
    return false
  end
  return H.call(p, "on_key", key_event(name)) and true or false
end

--- Type each character of `s`, as a paste arrives.
function H.type(p, s)
  for _, code in utf8.codes(s) do
    H.key(p, utf8.char(code))
  end
end

--- Deliver an event to every plugin subscribed to it.
function H.event(name, payload)
  for _, p in ipairs(plugins) do
    for _, e in ipairs(p.def.events or {}) do
      if e == name then
        H.call(p, "on_event", name, payload or {})
      end
    end
  end
end

--- Apply the queued commands the kernel would apply: settings writes land in
--- the registry. Everything else stays recorded for the test to read.
function H.flush()
  for _, c in ipairs(H.commands) do
    if c.verb == "set" then
      local value = c.opts.flag
      if value == nil then
        value = c.opts.number
      end
      H.settings[c.opts.text] = value
    end
  end
end

--- Answer a run the plugin asked for. `ok` true: exit 0.
function H.answer(p, key, stdout, status, stderr)
  local answers = H.runs[p.path] or {}
  H.runs[p.path] = answers
  answers[key] = {
    state = "done",
    stdout = stdout or "",
    stderr = stderr or "",
    status = status or 0,
    ok = (status or 0) == 0,
    truncated = false,
    timed_out = false,
  }
  for i = #H.pending, 1, -1 do
    if H.pending[i].plugin == p and H.pending[i].key == key then
      table.remove(H.pending, i)
    end
  end
end

function H.fail_run(p, key, error_text)
  local answers = H.runs[p.path] or {}
  H.runs[p.path] = answers
  answers[key] = { state = "failed", error = error_text }
end

--- The runs asked since the last reset, optionally filtered by a pattern on
--- the program.
function H.asked_matching(pattern)
  local out = {}
  for _, a in ipairs(H.asked) do
    if not pattern or a.program:find(pattern, 1, true) then
      out[#out + 1] = a
    end
  end
  return out
end

function H.reset_log()
  H.asked, H.commands, H.render_writes, H.render_runs = {}, {}, {}, {}
end

---------------------------------------------------------------------------
-- Live mode: answer each pending run by running it for real.
---------------------------------------------------------------------------

--- Run every pending ask through `sh -c` in the session's directory, the way
--- the kernel's runner does for a local session. Returns how many ran.
function H.live(cwd_of)
  local ran = 0
  while #H.pending > 0 do
    local item = table.remove(H.pending, 1)
    local cwd = cwd_of(item.ask.session)
    -- Beside the test's other scratch files, never the system temp directory.
    H.serial = (H.serial or 0) + 1
    local errfile = assert(os.getenv("SCRATCH"), "set SCRATCH") .. "/stderr-" .. H.serial
    local script = "cd " .. H.quote(cwd) .. " && " .. item.ask.program
    local pipe = assert(
      io.popen("sh -c " .. H.quote(script) .. " 2>" .. H.quote(errfile) .. '; echo "__exit:$?"')
    )
    local out = pipe:read("a")
    pipe:close()
    local stderr = read_file(errfile)
    os.remove(errfile)
    local body, code = out:match("^(.*)__exit:(%d+)%s*$")
    H.answer(item.plugin, item.key, body or out, tonumber(code) or 1, stderr)
    ran = ran + 1
  end
  return ran
end

function H.quote(s)
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

---------------------------------------------------------------------------
-- Reading a tree.
---------------------------------------------------------------------------

local function line_text(line)
  if type(line) == "string" or type(line) == "number" or type(line) == "boolean" then
    return tostring(line)
  end
  if type(line) ~= "table" then
    return ""
  end
  if line.text ~= nil and type(line.text) ~= "table" then
    return tostring(line.text)
  end
  local out = {}
  for _, span in ipairs(line) do
    out[#out + 1] = line_text(span)
  end
  return table.concat(out)
end
H.line_text = line_text

local function node_lines(node)
  local t = node.text
  if t == nil then
    return {}
  end
  if type(t) ~= "table" then
    return { tostring(t) }
  end
  if t.text ~= nil and type(t.text) ~= "table" then
    return { tostring(t.text) }
  end
  -- A list of lines, or one line given as a list of spans.
  local first = t[1]
  if type(first) == "table" and first.text ~= nil and type(first.text) ~= "table" then
    return { line_text(t) }
  end
  local out = {}
  for _, line in ipairs(t) do
    out[#out + 1] = line_text(line)
  end
  return out
end

local function frame_text(frame)
  if type(frame) ~= "table" then
    return {}
  end
  local out = {}
  local function add(v)
    if v == nil then
      return
    end
    if type(v) == "table" and v.text == nil and type(v[1]) == "table" and v[1].text == nil then
      for _, line in ipairs(v) do
        out[#out + 1] = line_text(line)
      end
    else
      out[#out + 1] = line_text(v)
    end
  end
  add(frame.title)
  if type(frame.overlay) == "table" then
    for _, slot in ipairs({ "top_left", "top_right", "bottom_left", "bottom_right" }) do
      add(frame.overlay[slot])
    end
  end
  return out
end

--- Every line of text in the tree, frames included, in tree order.
function H.lines(node, out)
  out = out or {}
  if type(node) ~= "table" then
    return out
  end
  for _, l in ipairs(frame_text(node.frame)) do
    out[#out + 1] = l
  end
  for _, l in ipairs(node_lines(node)) do
    out[#out + 1] = l
  end
  if node.type == "input" or node.type == "field" then
    local value = tostring(node.value or "")
    -- An empty field shows its placeholder, as the painter does.
    out[#out + 1] = value ~= "" and value or tostring(node.placeholder or "")
  end
  for _, child in ipairs(node.children or {}) do
    H.lines(child, out)
  end
  for _, child in ipairs(node) do
    H.lines(child, out)
  end
  return out
end

function H.screen(node)
  return table.concat(H.lines(node), "\n")
end

--- Nodes satisfying `pred`, depth first.
function H.find(node, pred, out)
  out = out or {}
  if type(node) ~= "table" then
    return out
  end
  if pred(node) then
    out[#out + 1] = node
  end
  for _, child in ipairs(node.children or {}) do
    H.find(child, pred, out)
  end
  return out
end

---------------------------------------------------------------------------
-- Assertions.
---------------------------------------------------------------------------

H.failures = 0
H.passed = 0

function H.test(name, fn)
  local ok, err = xpcall(fn, debug.traceback)
  if ok then
    H.passed = H.passed + 1
    io.write("ok   " .. name .. "\n")
  else
    H.failures = H.failures + 1
    io.write("FAIL " .. name .. "\n" .. tostring(err) .. "\n")
  end
end

function H.eq(actual, expected, what)
  if actual ~= expected then
    error(
      (what or "value") .. ": expected " .. tostring(expected) .. ", got " .. tostring(actual),
      2
    )
  end
end

function H.contains(haystack, needle, what)
  if not tostring(haystack):find(needle, 1, true) then
    error((what or "text") .. " does not contain «" .. needle .. "»:\n" .. tostring(haystack), 2)
  end
end

function H.absent(haystack, needle, what)
  if tostring(haystack):find(needle, 1, true) then
    error(
      (what or "text") .. " unexpectedly contains «" .. needle .. "»:\n" .. tostring(haystack),
      2
    )
  end
end

function H.truthy(v, what)
  if not v then
    error((what or "value") .. " is not truthy", 2)
  end
end

function H.finish()
  io.write(string.format("\n%d passed, %d failed\n", H.passed, H.failures))
  os.exit(H.failures == 0 and 0 or 1)
end

function H.read(path)
  return read_file(path)
end

return H
