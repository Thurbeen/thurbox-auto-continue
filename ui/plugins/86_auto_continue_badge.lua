-- Auto-continue's badge on the session list: what each Claude session's
-- auto-continue is doing, at the right edge of its row.
--
--   ↻       on, no limit yet          ⏸ 42m   armed: the send in 42 minutes
--   …       sending now               ✓       sent
--   ?       sent, not confirmed       ↷       superseded: you or Claude resumed first
--   ⊘       skipped for another reason ✗      gave up, or abandoned
--
-- Nothing for a session that is off with no episode, for a session that is not
-- Claude's, and for one on a host the CLI could not ask: no badge is the
-- honest answer when there is nothing to say or nothing known. The pane
-- (85_auto_continue.lua, F11) says why for each.
--
-- A decorator draws nothing of its own: it is handed the session list's tree
-- on every frame and returns it with a run added to each row it knows. Rows
-- are found by the identity thurbox's own list gives them (`role = "row"`, the
-- session id, class `session-row`), so a click or a selection lands exactly as
-- before. It reads `status --json` through `run`, which needs its own grant —
-- untrusted, it hands the tree back untouched.

local model = require("thurbox-auto-continue.ui.lib.model")
local theme = require("lib.theme")
local tree = require("lib.tree")

local TTL, TIMEOUT = 10, 60

local TONE = {
  idle = "accent",
  armed = "status_blocked",
  ok = "status_done",
  warn = "status_blocked",
  bad = "danger",
  muted = "text_muted",
}

-- The last answer decoded, by its text: a render re-reads the same stdout
-- until a new answer lands, and decoding it once is enough.
local memo = { text = nil, rows = {} }

local function rows_from(stdout)
  if memo.text ~= stdout then
    local parsed = model.parse_run({ state = "done", status = 0, stdout = stdout })
    memo.text = stdout
    memo.rows = parsed.kind == "ok" and model.index(parsed.data) or {}
  end
  return memo.rows
end

--- Every row the status sources answered for, by session id.
local function known_rows()
  local out = {}
  local runs = thurbox.runs or {}
  for _, src in ipairs(model.status_sources(thurbox.sessions)) do
    run(src.key, model.status_cmd(), { session = src.session, ttl = TTL, timeout = TIMEOUT })
    local r = runs[src.key]
    if r and r.state == "done" and r.status == 0 then
      for id, row in pairs(rows_from(r.stdout)) do
        out[id] = row
      end
    end
  end
  return out
end

--- A copy of `spans` cut to `cols` columns.
local function cut(spans, cols)
  local out, used = {}, 0
  for _, s in ipairs(spans) do
    local t = tostring(s.text)
    local w = text.width(t)
    if used + w <= cols then
      out[#out + 1] = s
      used = used + w
    else
      local room = cols - used
      if room > 0 then
        out[#out + 1] = { text = text.truncate(t, room), style = s.style, id = s.id, role = s.role }
        used = used + text.width(out[#out].text)
      end
      break
    end
  end
  return out, used
end

return {
  name = "auto-continue-badge",
  decorates = "sessions",
  order = 86,
  capabilities = { "run" },

  decorate = function(node, ctx)
    if not run then
      return node
    end
    local rows = known_rows()
    if next(rows) == nil then
      return node
    end
    local now = thurbox.taken_at_ms
    -- The list's rows sit inside the panel's border.
    local inner = math.max(0, (ctx.width or 0) - 2)
    return tree.map(node, function(candidate)
      if candidate.role ~= "row" or not tree.has_class(candidate, "session-row") then
        return nil
      end
      local badge = model.badge(rows[candidate.id], now)
      if not badge or type(candidate.text) ~= "table" or type(candidate.text[1]) ~= "table" then
        return nil
      end
      local mark = " " .. badge.text .. " "
      local room = inner - text.width(mark)
      if room < 4 then
        return nil
      end
      local spans, used = cut(candidate.text[1], room)
      spans[#spans + 1] = { text = string.rep(" ", room - used) }
      -- A selected row's bar names no foreground, so the badge keeps its own.
      spans[#spans + 1] = { text = mark, style = { fg = theme.role(TONE[badge.tone] or "text_muted") } }
      candidate.text[1] = spans
      return candidate
    end)
  end,
}
