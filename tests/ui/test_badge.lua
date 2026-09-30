-- The session-row badge: thurbox's own session list is rendered (the release's
-- real `plugins/10_sessions.lua`), and the decorator is handed that tree, as
-- the kernel hands it every frame.

local HERE = os.getenv("HERE")
local H = dofile(HERE .. "/harness.lua")
local BADGE = H.PLUGIN_ROOT .. "/ui/plugins/86_auto_continue_badge.lua"
local STATUS = H.read(HERE .. "/fixtures/status.json")

local NOW = 1790762010000
local W, HT = 40, 16

local function id(n)
  return "00000000-0000-4000-8000-0000000000" .. n
end

local function sessions()
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
    s("10", "a-very-long-session-name-that-fills-the-row", "claude"),
  }
end

H.sessions = sessions()
H.now_ms = NOW
H.publish()
local list = H.load(H.UI .. "/plugins/10_sessions.lua")
local badge_trusted = H.load(BADGE, { trusted = true })
local badge_untrusted = H.load(BADGE, { trusted = false })

local function rows(tree)
  local out = {}
  for _, node in
    ipairs(H.find(tree, function(n)
      return n.role == "row" and type(n.id) == "string"
    end))
  do
    out[node.id] = H.line_text(node.text[1])
  end
  return out
end

local function decorated(p)
  H.publish()
  local tree = H.render(list, W, HT, false)
  return H.decorate(p, tree, W, HT), tree
end

H.test("untrusted: the list is handed back as it came, and nothing runs", function()
  H.reset_log()
  local out, tree = decorated(badge_untrusted)
  H.eq(H.screen(out), H.screen(tree))
  H.eq(#H.asked, 0)
end)

H.test("status not in yet: no badge rather than a guess", function()
  H.reset_log()
  local out, tree = decorated(badge_trusted)
  H.eq(H.screen(out), H.screen(tree))
  local asks = H.asked_matching("'status' '--json'")
  H.eq(#asks, 1)
  H.eq(asks[1].session, id("0a"), "from a local session")
end)

H.test("each Claude row carries its state; codex and unreachable hosts carry nothing", function()
  local asks = H.asked_matching("'status' '--json'")
  H.answer(badge_trusted, asks[#asks].key, STATUS)
  local out = decorated(badge_trusted)
  local r = rows(out)
  H.contains(r[id("0a")], "1h 3m", "worker: armed, counting down")
  H.contains(r[id("0b")], "✓", "reviewer: sent")
  H.absent(r[id("0c")], "↻", "docs: off, so no badge")
  H.absent(r[id("0d")], "↻", "codex: never")
  H.absent(r[id("0d")], "✓")
  H.contains(r[id("0e")], "↷", "on-devbox: superseded")
  H.absent(r[id("0f")], "↻", "on-oldbox: unreadable host")
end)

H.test("a badge never pushes a row past its width", function()
  local out = decorated(badge_trusted)
  for rid, line in pairs(rows(out)) do
    H.truthy(utf8.len(line) <= W - 2, rid .. " is " .. utf8.len(line) .. " columns: " .. line)
  end
end)

H.test("a badge keeps the row's own identity, so clicks and selection still land", function()
  local out = decorated(badge_trusted)
  local found = H.find(out, function(n)
    return n.id == id("0a") and n.role == "row"
  end)
  H.eq(#found, 1)
  H.contains(found[1].class, "session-row")
end)

H.test("the decorator writes nothing while it draws", function()
  -- thurbox's own list writes its cursor while rendering; only the
  -- decorator's own call is under test.
  H.publish()
  local tree = H.render(list, W, HT, false)
  H.reset_log()
  H.decorate(badge_trusted, tree, W, HT)
  H.eq(#H.render_writes, 0, table.concat(H.render_writes, ", "))
  for _, a in ipairs(H.render_runs) do
    H.contains(a.program, "'status' '--json'", "only status is read")
  end
end)

H.finish()
