-- Auto-continue: the settings and the monitor of the headless extension.
--
-- One pane beside the agent (the `center` switch slot, advertised by a pill
-- and toggled by F11). A list — every Claude session, and one row for this
-- machine's global settings — and under it the selected row's settings and its
-- latest limit episode.
--
-- It holds no settings of its own. Everything is read with
-- `thurbox-auto-continue status --json` and `config show --json` and changed
-- with `config set|unset` (docs/CLI-CONTRACT.md), through `run`, so the
-- headless extension and this pane can never disagree about what is set.
--
-- Where a command runs: Thurbox runs a plugin's program in a session's
-- directory on that session's machine. This pane runs the CLI in a LOCAL
-- session, and the CLI reaches a shared host itself and says when it cannot.
-- With no local session at all, a remote session is asked on its own host.
--
-- The one piece of Thurbox configuration it declares is the Settings panel's
-- switch, `auto-continue.enabled` (off by default). It mirrors config.toml's
-- global `enabled`: see `reconcile` below, which runs from event handlers and
-- key actions only — never from render — and whose `run` is the one grant the
-- user made to this file.

local model = require("thurbox-auto-continue.ui.lib.model")
local textinput = require("lib.textinput")
local theme = require("lib.theme")
local ui = require("lib.ui")

local NAME = "auto-continue"
local SETTING = NAME .. ".enabled"
local GLOBAL = "global"
local FILE = "thurbox-auto-continue/ui/plugins/85_auto_continue.lua"

-- How long an answer stays fresh. A write refreshes both at once.
local STATUS_TTL, SHOW_TTL = 10, 30
local TIMEOUT = 60

---------------------------------------------------------------------------
-- Reads. Every one of these is a function of the snapshot and `state`, so a
-- key handler acts on exactly what the render drew.
---------------------------------------------------------------------------

local function sessions()
  return thurbox.sessions or {}
end

local function session_by_id(id)
  for _, s in ipairs(sessions()) do
    if s.id == id then
      return s
    end
  end
  return nil
end

--- Writes this pane issued, oldest first: `{ key, what }`.
local function writes()
  return state.writes or {}
end

--- How many writes have answered. It is part of every read's key, so a write
--- that lands re-reads status and settings on the next render. Writes go one at
--- a time (`busy`), so this is also the index of the last one that answered.
local function generation()
  local runs, n = thurbox.runs or {}, 0
  for _, w in ipairs(writes()) do
    local r = runs[w.key]
    if r and r.state ~= "pending" then
      n = n + 1
    end
  end
  return n + (state.refresh or 0)
end

local function busy()
  local list = writes()
  local last = list[#list]
  return last ~= nil and model.parse_write((thurbox.runs or {})[last.key]).kind == "pending"
end

local function status_sources()
  return model.status_sources(sessions())
end

--- The newest answered read for `base`, looking back from the current
--- generation so the pane does not blink to "pending" while it refreshes.
local function latest(base, gen)
  local runs = thurbox.runs or {}
  for g = gen, 0, -1 do
    local r = runs[base .. ":" .. g]
    if r and r.state ~= "pending" then
      return model.parse_run(r), g
    end
  end
  return model.parse_run(nil), -1
end

--- The status, merged over its sources: `{ kind, error, data, rows, gen }`.
local function status_now()
  local gen = generation()
  local sources = status_sources()
  if #sources == 0 then
    return { kind = "empty", rows = {}, gen = gen }
  end
  local merged = { kind = "ok", rows = {}, gen = math.huge }
  for _, src in ipairs(sources) do
    local answer, g = latest(src.key, gen)
    merged.gen = math.min(merged.gen, g)
    if answer.kind ~= "ok" then
      -- One source is enough to say the whole is not known.
      merged.kind, merged.error = answer.kind, answer.error
    else
      if not merged.data then
        merged.data = answer.data
      end
      for id, row in pairs(model.index(answer.data)) do
        merged.rows[id] = row
      end
    end
  end
  return merged
end

local function setting_on()
  for _, s in ipairs((thurbox.registry or {}).settings or {}) do
    if s.plugin == NAME and s.id == "enabled" then
      return s.value == true
    end
  end
  return false
end

--- Rows of the list: this machine's global settings, then every session.
local function targets()
  local out = { GLOBAL }
  for _, s in ipairs(sessions()) do
    out[#out + 1] = s.id
  end
  return out
end

local function selected()
  local list = targets()
  local want = state.cursor or GLOBAL
  for i, t in ipairs(list) do
    if t == want then
      return t, i, list
    end
  end
  return list[1], 1, list
end

--- The `config show` answer for a target.
local function show_now(target, gen)
  return (latest("show:" .. target, gen))
end

local function show_cmd(target)
  return model.show_cmd(target ~= GLOBAL and target or nil)
end

local function anchor_for(target)
  return model.anchor(sessions(), target ~= GLOBAL and target or nil)
end

---------------------------------------------------------------------------
-- Writes: only from handlers.
---------------------------------------------------------------------------

local function notice(text, level)
  if state.notice ~= text then
    state.notice = text
  end
  if level == "error" then
    command("message", { text = "auto-continue: " .. text, level = "error" })
  end
end

--- Can `target` be changed from here? Returns the session name for messages.
local function writable(target, status)
  if target == GLOBAL then
    if not model.anchor(sessions(), nil) then
      return false, "no local session to run the CLI in; the global settings are this machine's"
    end
    return true, "the global settings"
  end
  local s = session_by_id(target)
  local name = s and s.name or target
  if status.kind ~= "ok" then
    return false, name .. ": status is not in yet"
  end
  local ok, why = model.controllable(status.rows[target])
  if not ok then
    return false, name .. ": " .. why
  end
  return true, name
end

--- Issue one `config set` (value) or `config unset` (value nil).
local function write(target, key, value, what)
  if not run then
    return false
  end
  if busy() then
    notice("still saving the last change", "error")
    return false
  end
  local anchor = anchor_for(target)
  if not anchor then
    notice("no session to run the CLI in", "error")
    return false
  end
  local cmd
  if target == GLOBAL then
    cmd = model.set_cmd(key, value, nil)
  elseif value == nil then
    cmd = model.unset_cmd(key, target)
  else
    cmd = model.set_cmd(key, value, target)
  end
  local list = writes()
  state.serial = (state.serial or 0) + 1
  local run_key = "write:" .. state.serial
  list[#list + 1] = { key = run_key, what = what }
  -- Only the latest few are ever drawn or counted against.
  while #list > 8 do
    table.remove(list, 1)
    state.refresh = (state.refresh or 0) + 1
  end
  state.writes = list
  state.notice = nil
  run(run_key, cmd, { session = anchor, ttl = 86400, timeout = TIMEOUT })
  return true
end

--- Keep the Settings switch and config.toml's `enabled` in step (see
--- `model.reconcile`). Called from handlers only.
local function reconcile()
  if not run or busy() then
    return
  end
  local status = status_now()
  if status.kind ~= "ok" or not status.data then
    return
  end
  -- A status read before our own mirror write answered would read as config
  -- moving back: wait for one read after it.
  if state.mirror_wait and status.gen < state.mirror_wait then
    return
  end
  local global = status.data.global or {}
  local config = type(global.enabled) == "table" and global.enabled.value == true or false
  local act = model.reconcile(setting_on(), config, state.mirror)
  if act.write ~= nil then
    if
      write(GLOBAL, "enabled", act.write and "on" or "off", "the global switch (from Settings)")
    then
      state.mirror_wait = generation() + 1
    else
      return
    end
  end
  if act.adopt ~= nil then
    command("set", { text = SETTING, flag = act.adopt })
  end
  if state.mirror ~= act.baseline then
    state.mirror = act.baseline
  end
end

local function current_value(target, key)
  local show = show_now(target, generation())
  if show.kind == "ok" and type(show.data[key]) == "table" then
    return show.data[key].value
  end
  return nil
end

local function cycle_enabled(target)
  local status = status_now()
  local ok, name = writable(target, status)
  if not ok then
    notice(name, "error")
    return
  end
  if target == GLOBAL then
    local g = (status.data and status.data.global) or {}
    local on = not (type(g.enabled) == "table" and g.enabled.value == true)
    if
      write(GLOBAL, "enabled", on and "on" or "off", "global switch " .. (on and "on" or "off"))
    then
      command("set", { text = SETTING, flag = on })
      state.mirror = on
      state.mirror_wait = generation() + 1
    end
    return
  end
  -- inherit → on → off → inherit
  local override = ((status.rows[target] or {}).overrides or {}).enabled
  if override == "on" then
    write(target, "enabled", "off", name .. " off")
  elseif override == "off" then
    write(target, "enabled", nil, name .. " back to the global switch")
  else
    write(target, "enabled", "on", name .. " on")
  end
end

local function start_edit(target, key)
  local ok, name = writable(target, status_now())
  if not ok then
    notice(name, "error")
    return
  end
  -- The field opens empty, the current value as its placeholder: typing the
  -- new one needs no clearing first, and the readline clear (`ctrl+u`) is a
  -- global chord in thurbox (restore a deleted session), never the field's.
  local value = current_value(target, key)
  if value ~= nil and key == "delay_secs" then
    value = tostring(math.floor(value)) .. " s"
  end
  state.edit = {
    target = target,
    key = key,
    name = name,
    now = value ~= nil and tostring(value) or nil,
    field = textinput.new(""),
  }
  state.edit_error = nil
end

local function submit_edit()
  local edit = state.edit
  local value, err = model.validate(edit.key, edit.field.value)
  if not value then
    state.edit_error = err
    return
  end
  local label = edit.key == "message" and "message" or "delay"
  if write(edit.target, edit.key, value, edit.name .. " " .. label) then
    state.edit, state.edit_error = nil, nil
  end
end

local function reset(target, key)
  if target == GLOBAL then
    notice("the global value has no default to go back to here; set it", "error")
    return
  end
  local ok, name = writable(target, status_now())
  if not ok then
    notice(name, "error")
    return
  end
  write(
    target,
    key,
    nil,
    name .. " " .. (key == "message" and "message" or "delay") .. " back to the global value"
  )
end

local function move(delta)
  local _, index, list = selected()
  local next_index = math.max(1, math.min(#list, index + delta))
  if list[next_index] ~= state.cursor then
    state.cursor = list[next_index]
    state.notice = nil
  end
end

---------------------------------------------------------------------------
-- Drawing.
---------------------------------------------------------------------------

local TONE = {
  idle = function()
    return theme.accent
  end,
  armed = function()
    return theme.role("status_blocked")
  end,
  ok = function()
    return theme.role("status_done")
  end,
  warn = function()
    return theme.role("status_blocked")
  end,
  bad = function()
    return theme.role("danger")
  end,
  muted = function()
    return theme.muted
  end,
}

local function span(t, fg, extra)
  local style = { fg = fg }
  for k, v in pairs(extra or {}) do
    style[k] = v
  end
  return { text = t, style = style }
end

local function line(spans, width)
  -- Cut the row to the rect: a node wider than its rect would be clipped by
  -- the painter anyway, but a cut we make ourselves ends in an ellipsis.
  local out, used = {}, 0
  for _, s in ipairs(spans) do
    local w = text.width(s.text)
    if used + w > width then
      local room = width - used
      if room > 0 then
        out[#out + 1] = { text = text.truncate(s.text, room), style = s.style }
      end
      break
    end
    out[#out + 1] = s
    used = used + w
  end
  ---@type thurbox.TextNode
  local node = { type = "text", len = 1, text = { out } }
  return node
end

local function blank()
  return { type = "text", len = 1, text = "" }
end

local function source_text(entry)
  if type(entry) ~= "table" then
    return ""
  end
  return tostring(entry.source or "")
end

local function show_value(key, entry)
  if type(entry) ~= "table" or entry.value == nil then
    return "—"
  end
  if key == "enabled" then
    return entry.value and "on" or "off"
  elseif key == "delay_secs" then
    return tostring(math.floor(entry.value)) .. " s"
  end
  return tostring(entry.value)
end

local function trust_view(ctx)
  local w = math.max(0, ctx.width - 2)
  return ui.panel({
    title = "Auto-continue",
    focused = ctx.focused,
    body = {
      type = "box",
      children = {
        blank(),
        line({
          span(
            " This pane runs thurbox-auto-continue to read and change its settings,",
            theme.text
          ),
        }, w),
        line({ span(" so it needs the run capability, which only you can grant:", theme.text) }, w),
        blank(),
        line({ span("   Ctrl+, (or F6) → ] → " .. FILE .. " → t", theme.accent) }, w),
        blank(),
        line({
          span(
            " Do the same for 86_auto_continue_badge.lua to see the badge on session rows.",
            theme.muted
          ),
        }, w),
        line({ span(" Nothing is read or changed until then.", theme.muted) }, w),
        { type = "text", fill = 1, text = "" },
      },
    },
  })
end

local function status_banner(status, w)
  if status.kind == "pending" then
    return line({ span(" reading status…", theme.muted) }, w)
  elseif status.kind == "empty" then
    return line({ span(" no session to run the CLI in — open any session", theme.muted) }, w)
  elseif status.kind == "missing-binary" then
    return line({
      span(
        " thurbox-auto-continue is not installed on this machine — see the README's Install",
        theme.bad
      ),
    }, w)
  elseif status.kind ~= "ok" then
    return line(
      { span(" status unavailable: " .. tostring(status.error or status.kind), theme.bad) },
      w
    )
  end
  local active = status.data.extension_active
  if active == false then
    return line({
      span(
        " extension inactive — nothing is sent (thurbox-cli extension activate auto-continue)",
        theme.bad
      ),
    }, w)
  elseif active == nil then
    return line(
      { span(" extension state unknown: thurbox-cli did not answer", theme.role("status_blocked")) },
      w
    )
  end
  local warnings = status.data.warnings or {}
  if #warnings > 0 then
    return line({
      span(
        " config.toml: " .. tostring(warnings[1]) .. " — nothing is sent until it is fixed",
        theme.bad
      ),
    }, w)
  end
  return line({ span(" extension active", theme.role("status_done")) }, w)
end

local function global_row(status, is_sel, w)
  local marker = is_sel and " ▸ " or "   "
  local summary
  if status.kind == "pending" then
    summary = span("reading status…", theme.muted)
  elseif status.kind ~= "ok" or not status.data then
    summary = span("unknown", theme.muted)
  else
    local g = status.data.global or {}
    local on = type(g.enabled) == "table" and g.enabled.value == true
    local count, total = 0, 0
    for _, s in ipairs(sessions()) do
      local row = status.rows[s.id]
      if row and model.controllable(row) then
        total = total + 1
        if row.enabled then
          count = count + 1
        end
      end
    end
    summary = span(
      string.format(
        "%s (%s) · %d of %d Claude sessions on",
        on and "on" or "off",
        source_text(g.enabled),
        count,
        total
      ),
      on and theme.accent or theme.muted
    )
  end
  local node = line({
    span(marker, theme.accent),
    span("Every Claude session", theme.text, { bold = true }),
    span("  config.toml  ", theme.muted),
    summary,
  }, w)
  if is_sel then
    node.style = { bg = theme.role("selection_bg") }
  end
  return node
end

local function session_row(s, status, is_sel, w, now)
  local marker = is_sel and " ▸ " or "   "
  local row = status.rows[s.id]
  local switch, switch_fg, summary
  if status.kind == "pending" and not row then
    switch, switch_fg, summary = "…", theme.muted, { text = "", tone = "muted" }
  elseif status.kind ~= "ok" and not row then
    switch, switch_fg, summary = "?", theme.muted, { text = "unknown", tone = "muted" }
  else
    summary = model.summary(row, now)
    if row and model.controllable(row) then
      switch = row.enabled and "on" or "off"
      switch_fg = row.enabled and theme.accent or theme.muted
    else
      switch, switch_fg = "—", theme.muted
    end
  end
  local where = ""
  if s.backend and not model.is_local(s) and not summary.text:find(s.backend, 1, true) then
    where = " · " .. s.backend
  end
  local node = line({
    span(marker, theme.accent),
    span(text.pad(text.truncate(s.name or s.id, 18), 18), theme.text),
    span(" " .. text.pad(switch, 4), switch_fg),
    span(summary.text, TONE[summary.tone or "muted"]()),
    span(where, theme.muted),
  }, w)
  if is_sel then
    node.style = { bg = theme.role("selection_bg") }
  end
  return node
end

local function kv(label, value, value_fg, note, w)
  return line({
    span("   " .. text.pad(label, 11), theme.muted),
    span(value, value_fg or theme.text),
    span(note and ("   " .. note) or "", theme.muted),
  }, w)
end

--- The settings of the selected row, from `config show`, with the editor.
local function settings_lines(target, status, gen, w, out)
  local show = show_now(target, gen)
  local edit = state.edit
  local function field(key, label)
    if edit and edit.target == target and edit.key == key then
      out[#out + 1] = textinput.node(edit.field, {
        label = label
          .. (key == "message" and " — one line, 1-200 characters" or " — seconds, 0-86400"),
        placeholder = edit.now and ("now: " .. edit.now) or "",
        focused = true,
      })
      if state.edit_error then
        out[#out + 1] = line({ span("   ✗ " .. state.edit_error, theme.bad) }, w)
      end
      out[#out + 1] = line({ span("   enter save · esc cancel", theme.muted) }, w)
      return
    end
    local entry = show.kind == "ok" and show.data[key] or nil
    out[#out + 1] = kv(label, show_value(key, entry), theme.text, source_text(entry), w)
  end
  if show.kind == "pending" then
    out[#out + 1] = line({ span("   reading settings…", theme.muted) }, w)
    return
  elseif show.kind ~= "ok" then
    out[#out + 1] =
      line({ span("   settings unavailable: " .. tostring(show.error or show.kind), theme.bad) }, w)
    return
  end
  local g = (status.data and status.data.global) or {}
  local global_note = type(g.enabled) == "table"
      and ("global " .. (g.enabled.value and "on" or "off"))
    or ""
  local entry = show.data.enabled
  out[#out + 1] = kv(
    "enabled",
    show_value("enabled", entry),
    theme.text,
    source_text(entry) .. (target ~= GLOBAL and ("  (" .. global_note .. ")") or ""),
    w
  )
  field("message", "message")
  field("delay_secs", "delay")
  if target == GLOBAL then
    out[#out + 1] =
      line({ span("   Settings ▸ auto-continue.enabled mirrors this switch", theme.muted) }, w)
  end
end

--- The Settings switch and config.toml disagree until the next event mirrors
--- one onto the other; say so wherever the cursor is.
local function mirror_line(status, w)
  if status.kind ~= "ok" or not status.data then
    return nil
  end
  local g = status.data.global or {}
  local s, c = setting_on(), type(g.enabled) == "table" and g.enabled.value == true
  if s == c then
    return nil
  end
  return line({
    span(
      string.format(
        " Settings switch: %s · config.toml: %s — mirrored on the next interface event",
        s and "on" or "off",
        c and "on" or "off"
      ),
      theme.role("status_blocked")
    ),
  }, w)
end

--- The latest limit episode of the selected session.
local function episode_lines(row, now, w, out)
  local ep = row and row.episode
  if type(ep) ~= "table" then
    out[#out + 1] = kv("episode", "no limit recorded yet", theme.muted, nil, w)
    return
  end
  out[#out + 1] = kv(
    "window",
    tostring(ep.window or "?") .. " · resets " .. model.relative(ep.resets_at_ms, now),
    nil,
    nil,
    w
  )
  local attempts =
    string.format("attempt %s of %s", tostring(ep.attempt or "?"), tostring(ep.max_attempts or "?"))
  if ep.state == "armed" then
    local due = type(ep.next_send_at_ms) == "number"
      and type(now) == "number"
      and ep.next_send_at_ms <= now
    local when = due and "due — on Thurbox's next heartbeat"
      or model.relative(ep.next_send_at_ms, now)
    out[#out + 1] = kv("next send", when, theme.role("status_blocked"), attempts, w)
  else
    out[#out + 1] = kv("attempts", attempts, nil, nil, w)
  end
  local last = row.last_outcome
  if type(last) == "table" then
    local summary = model.summary(row, now)
    local _, superseded = model.reason(last.reason)
    out[#out + 1] = kv(
      "last",
      summary.text,
      TONE[summary.tone](),
      superseded and "native resume or user activity came first" or nil,
      w
    )
  else
    out[#out + 1] = kv("last", "none yet — this episode is still open", theme.muted, nil, w)
  end
  for _, warning in ipairs(row.warnings or {}) do
    out[#out + 1] = line({ span("   ! " .. tostring(warning), theme.role("status_blocked")) }, w)
  end
end

local function feedback_lines(w, out)
  local list = writes()
  local last = list[#list]
  if last then
    local answer = model.parse_write((thurbox.runs or {})[last.key])
    if answer.kind == "pending" then
      out[#out + 1] = line({ span(" … saving " .. last.what, theme.muted) }, w)
    elseif answer.kind == "ok" then
      out[#out + 1] = line({ span(" ✓ saved " .. last.what, theme.role("status_done")) }, w)
    else
      out[#out + 1] =
        line({ span(" ✗ " .. last.what .. ": " .. tostring(answer.error), theme.bad) }, w)
    end
  end
  if state.set_error then
    out[#out + 1] = line({ span(" ✗ Settings: " .. state.set_error, theme.bad) }, w)
  end
  if state.notice then
    out[#out + 1] = line({ span(" " .. state.notice, theme.role("status_blocked")) }, w)
  end
end

local function hints(target, status)
  if state.edit then
    return "enter save · esc cancel"
  end
  if target == GLOBAL then
    return "j/k move · e on/off · m message · d delay · r refresh · F11 back"
  end
  if model.controllable(status.rows[target]) then
    return "j/k move · e on/off/inherit · m message · d delay · M/D reset · r refresh"
  end
  return "j/k move · r refresh"
end

local function render(ctx)
  if not run then
    return trust_view(ctx)
  end
  local w = math.max(0, ctx.width - 2)
  local h = math.max(0, ctx.height - 2)
  local gen = generation()
  local now = thurbox.taken_at_ms

  -- Ask on every render: an answer still fresh costs a table lookup.
  for _, src in ipairs(status_sources()) do
    run(
      src.key .. ":" .. gen,
      model.status_cmd(),
      { session = src.session, ttl = STATUS_TTL, timeout = TIMEOUT }
    )
  end
  local status = status_now()
  local target, index, list = selected()
  local anchor = anchor_for(target)
  if anchor and (target == GLOBAL or model.controllable(status.rows[target])) then
    run(
      "show:" .. target .. ":" .. gen,
      show_cmd(target),
      { session = anchor, ttl = SHOW_TTL, timeout = TIMEOUT }
    )
  end

  -- The detail first, so the list gets whatever height is left.
  local detail = {}
  local title
  if target == GLOBAL then
    title = "this machine's global settings (config.toml)"
  else
    local s = session_by_id(target)
    title = (s and s.name or target)
      .. ((s and not model.is_local(s)) and (" on " .. s.backend) or "")
  end
  detail[#detail + 1] = { type = "text", len = 1, text = { ui.rule(title, w) } }
  if target == GLOBAL then
    if anchor then
      settings_lines(target, status, gen, w, detail)
    else
      detail[#detail + 1] = line(
        { span("   no local session: the global settings are each machine's own", theme.muted) },
        w
      )
    end
  else
    local row = status.rows[target]
    local ok, why = model.controllable(row)
    if ok then
      settings_lines(target, status, gen, w, detail)
      episode_lines(row, now, w, detail)
    elseif status.kind == "ok" or row then
      detail[#detail + 1] = line({ span("   " .. why .. " — no controls here", theme.muted) }, w)
    end
  end
  feedback_lines(w, detail)

  local children = { status_banner(status, w) }
  children[#children + 1] = mirror_line(status, w)
  children[#children + 1] = { type = "text", len = 1, text = { ui.rule("Sessions", w) } }
  local room = math.max(1, h - #children - #detail - 1)
  local first = math.max(1, math.min(index - math.floor(room / 2), #list - room + 1))
  for i = first, math.min(#list, first + room - 1) do
    local t = list[i]
    if t == GLOBAL then
      children[#children + 1] = global_row(status, i == index, w)
    else
      children[#children + 1] = session_row(session_by_id(t), status, i == index, w, now)
    end
  end
  children[#children + 1] = blank()
  for _, d in ipairs(detail) do
    children[#children + 1] = d
  end
  children[#children + 1] = { type = "text", fill = 1, text = "" }
  children[#children + 1] = line({ span(" " .. hints(target, status), theme.muted) }, w)

  return ui.panel({
    title = "Auto-continue",
    focused = ctx.focused,
    body = { type = "box", children = children },
  })
end

return {
  name = NAME,
  -- Beside the agent, as a switch alternate: the pill and F11 bring it forward.
  slot = "center",
  order = 85,
  focusable = true,
  -- Render reads the snapshot, `state` and `run` answers, and writes nothing.
  pure = true,
  capabilities = { "run" },

  keys = {
    {
      key = "f11",
      action = "auto-continue.toggle",
      desc = "auto-continue: show or leave the pane",
      scope = "global",
    },
    { key = "j", action = "auto-continue.next", desc = "next row" },
    { key = "down", action = "auto-continue.next", desc = "next row" },
    { key = "k", action = "auto-continue.previous", desc = "previous row" },
    { key = "up", action = "auto-continue.previous", desc = "previous row" },
    { key = "e", action = "auto-continue.enabled", desc = "on / off / back to the global switch" },
    { key = "m", action = "auto-continue.message", desc = "edit the continue message" },
    { key = "d", action = "auto-continue.delay", desc = "edit the delay after the reset" },
    {
      key = "M",
      action = "auto-continue.reset_message",
      desc = "message back to the global value",
    },
    { key = "D", action = "auto-continue.reset_delay", desc = "delay back to the global value" },
    { key = "r", action = "auto-continue.refresh", desc = "read the status again" },
  },
  pills = { { action = "auto-continue.toggle", label = "Auto-continue", priority = 5 } },
  commands = {
    { action = "auto-continue.toggle", desc = "auto-continue: show or leave the pane" },
    {
      action = "auto-continue.toggle_selected",
      desc = "auto-continue: turn on/off for the selected session",
    },
  },
  settings = {
    {
      id = "enabled",
      desc = "auto-continue every Claude session (config.toml's global switch; off by default)",
      default = false,
    },
  },
  events = {
    "focus.session",
    "focus.pane",
    "session.status",
    "interface.reloaded",
    "command.done",
    "command.failed",
  },

  render = render,

  on_action = function(action)
    if action == "auto-continue.toggle" then
      command("focus", { text = NAME, toggle = true })
      reconcile()
      return true
    end
    if action == "auto-continue.toggle_selected" then
      local id = store.selected
      local status = status_now()
      local ok, name = writable(id or "", status)
      if not id or not ok then
        notice(id and name or "no session selected", "error")
        return true
      end
      local row = status.rows[id]
      local on = not row.enabled
      write(id, "enabled", on and "on" or "off", name .. (on and " on" or " off"))
      return true
    end
    -- A letter pressed while the field has focus is typing, not an action.
    if state.edit then
      return false
    end
    if action == "auto-continue.next" then
      move(1)
    elseif action == "auto-continue.previous" then
      move(-1)
    elseif action == "auto-continue.enabled" then
      cycle_enabled((selected()))
    elseif action == "auto-continue.message" then
      start_edit((selected()), "message")
    elseif action == "auto-continue.delay" then
      start_edit((selected()), "delay_secs")
    elseif action == "auto-continue.reset_message" then
      reset((selected()), "message")
    elseif action == "auto-continue.reset_delay" then
      reset((selected()), "delay_secs")
    elseif action == "auto-continue.refresh" then
      state.refresh = (state.refresh or 0) + 1
    else
      return false
    end
    reconcile()
    return true
  end,

  on_key = function(key)
    local edit = state.edit
    if not edit then
      return false
    end
    if key.key == "esc" then
      state.edit, state.edit_error = nil, nil
      return true
    end
    if key.key == "enter" then
      submit_edit()
      return true
    end
    if textinput.key(edit.field, key) then
      state.edit, state.edit_error = edit, nil
    end
    -- Swallow the rest: a field that let keys through is not one.
    return true
  end,

  on_click = function(hit)
    return false and hit
  end,

  on_event = function(name, payload)
    if
      name == "focus.session"
      and not state.edit
      and type(payload.to) == "string"
      and payload.to ~= ""
    then
      -- Follow the session list, so the pane opens on what you were looking at.
      if state.cursor ~= payload.to then
        state.cursor = payload.to
      end
    end
    if name == "command.failed" and payload.kind == "set" and payload.subject == SETTING then
      state.set_error = tostring(payload.error or "refused")
    elseif
      name == "command.done"
      and payload.kind == "set"
      and payload.subject == SETTING
      and state.set_error
    then
      state.set_error = nil
    end
    reconcile()
  end,
}
