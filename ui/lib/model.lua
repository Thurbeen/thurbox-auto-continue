-- The plugin's pure half: docs/CLI-CONTRACT.md read from Lua.
--
-- Everything here is a function of its arguments. No `thurbox`, no `run`, no
-- `store`: the pane and the badge hand in what they read and draw what comes
-- back, so this is also the half the tests can pin down without a snapshot.
--
-- The CLI is the only source of truth. Nothing here keeps a copy of a setting:
-- values are read with `status --json` and `config show --json` and changed with
-- `config set|unset`, exactly as a script would. `validate` repeats the
-- contract's rules only so an obvious mistake is refused before a process is
-- started; the CLI checks again and its refusal is what the pane shows.

local model = {}

---------------------------------------------------------------------------
-- JSON. The sandbox has no decoder, and the contract is JSON.
---------------------------------------------------------------------------

local ESCAPES = { ['"'] = '"', ["\\"] = "\\", ["/"] = "/", b = "\b", f = "\f", n = "\n", r = "\r", t = "\t" }

--- Decode one JSON document. `null` reads as absent (nil), which is how every
--- caller here treats it. Returns nil and a reason for anything that is not
--- exactly one JSON value.
function model.decode(text)
  if type(text) ~= "string" then
    return nil, "no output"
  end
  local pos = 1
  local function fail(what)
    error({ json = what .. " at byte " .. pos }, 0)
  end
  local function skip()
    pos = text:find("[^ \t\r\n]", pos) or (#text + 1)
  end
  local value
  local function str()
    local out = {}
    pos = pos + 1
    while true do
      local c = text:sub(pos, pos)
      if c == "" then
        fail("unterminated string")
      elseif c == '"' then
        pos = pos + 1
        return table.concat(out)
      elseif c == "\\" then
        local e = text:sub(pos + 1, pos + 1)
        if e == "u" then
          local hex = text:sub(pos + 2, pos + 5)
          local code = tonumber(hex, 16)
          if not code or #hex ~= 4 then
            fail("bad \\u escape")
          end
          pos = pos + 6
          -- A surrogate pair is one character.
          if code >= 0xD800 and code <= 0xDBFF and text:sub(pos, pos + 1) == "\\u" then
            local low = tonumber(text:sub(pos + 2, pos + 5), 16)
            if low and low >= 0xDC00 and low <= 0xDFFF then
              code = 0x10000 + (code - 0xD800) * 0x400 + (low - 0xDC00)
              pos = pos + 6
            end
          end
          out[#out + 1] = utf8.char(code)
        elseif ESCAPES[e] then
          out[#out + 1] = ESCAPES[e]
          pos = pos + 2
        else
          fail("bad escape")
        end
      else
        local stop = text:find('["\\]', pos) or (#text + 1)
        out[#out + 1] = text:sub(pos, stop - 1)
        pos = stop
      end
    end
  end
  value = function()
    skip()
    local c = text:sub(pos, pos)
    if c == "{" then
      local obj = {}
      pos = pos + 1
      skip()
      if text:sub(pos, pos) == "}" then
        pos = pos + 1
        return obj
      end
      while true do
        skip()
        if text:sub(pos, pos) ~= '"' then
          fail("expected a key")
        end
        local key = str()
        skip()
        if text:sub(pos, pos) ~= ":" then
          fail("expected :")
        end
        pos = pos + 1
        obj[key] = value()
        skip()
        local d = text:sub(pos, pos)
        pos = pos + 1
        if d == "}" then
          return obj
        elseif d ~= "," then
          fail("expected , or }")
        end
      end
    elseif c == "[" then
      local arr, n = {}, 0
      pos = pos + 1
      skip()
      if text:sub(pos, pos) == "]" then
        pos = pos + 1
        return arr
      end
      while true do
        n = n + 1
        arr[n] = value()
        skip()
        local d = text:sub(pos, pos)
        pos = pos + 1
        if d == "]" then
          return arr
        elseif d ~= "," then
          fail("expected , or ]")
        end
      end
    elseif c == '"' then
      return str()
    elseif text:sub(pos, pos + 3) == "true" then
      pos = pos + 4
      return true
    elseif text:sub(pos, pos + 4) == "false" then
      pos = pos + 5
      return false
    elseif text:sub(pos, pos + 3) == "null" then
      pos = pos + 4
      return nil
    else
      local num = text:match("^-?%d+%.?%d*[eE]?[-+]?%d*", pos)
      if not num or num == "" or num == "-" then
        fail("unexpected character")
      end
      pos = pos + #num
      return tonumber(num) or fail("bad number")
    end
  end
  local ok, result = pcall(function()
    local v = value()
    skip()
    if pos <= #text then
      fail("trailing characters")
    end
    return v
  end)
  if not ok then
    return nil, type(result) == "table" and result.json or tostring(result)
  end
  if result == nil then
    return nil, "null"
  end
  return result
end

---------------------------------------------------------------------------
-- Command lines.
---------------------------------------------------------------------------

--- One POSIX shell word.
function model.quote(s)
  return "'" .. tostring(s):gsub("'", "'\\''") .. "'"
end

-- The binary: the extension home's own copy first (what install.sh puts
-- there, whatever PATH the TUI was started with), then PATH. Neither found is
-- exit 127, which `parse_run` reads as "not installed".
local LOOKUP = 'b="${THURBOX_AUTO_CONTINUE_HOME:-$HOME/.config/thurbox/auto-continue}/bin/thurbox-auto-continue"; '
  .. '[ -x "$b" ] || b=$(command -v thurbox-auto-continue) || exit 127; exec "$b"'

--- The command line for `thurbox-auto-continue <args…>`, every argument quoted.
function model.command(args)
  local words = { LOOKUP }
  for _, a in ipairs(args) do
    words[#words + 1] = model.quote(a)
  end
  return table.concat(words, " ")
end

function model.status_cmd()
  return model.command({ "status", "--json" })
end

--- `config show`, for one session or (session nil) the global settings.
function model.show_cmd(session)
  if session then
    return model.command({ "config", "show", "--session", session, "--json" })
  end
  return model.command({ "config", "show", "--json" })
end

function model.set_cmd(key, value, session)
  if session then
    return model.command({ "config", "set", key, value, "--session", session, "--json" })
  end
  return model.command({ "config", "set", key, value, "--json" })
end

function model.unset_cmd(key, session)
  return model.command({ "config", "unset", key, "--session", session, "--json" })
end

---------------------------------------------------------------------------
-- Where a command runs.
---------------------------------------------------------------------------

function model.is_local(session)
  return type(session.backend) == "string" and session.backend:sub(1, 5) == "local"
end

--- The session a command about `target` (a session id, or nil for this
--- machine's global settings) runs in.
---
--- Thurbox runs a plugin's program in a session's directory, on that session's
--- machine. A local one is always the right place: the CLI reaches a shared
--- host itself (docs/CLI-CONTRACT.md, "Sessions on shared hosts") and says
--- which hosts cannot be asked. Only with no local session at all does a remote
--- session's own host answer for it; nothing then answers for this machine.
function model.anchor(sessions, target)
  for _, s in ipairs(sessions or {}) do
    if model.is_local(s) then
      return s.id
    end
  end
  if target then
    for _, s in ipairs(sessions or {}) do
      if s.id == target then
        return s.id
      end
    end
  end
  return nil
end

--- Where `status --json` is asked: `{ key, session }` for one local session,
--- or for one session per host when there is no local session (each host
--- then answers for its own).
function model.status_sources(sessions)
  local local_anchor = model.anchor(sessions, nil)
  if local_anchor then
    return { { key = "status", session = local_anchor } }
  end
  local out, seen = {}, {}
  for _, s in ipairs(sessions or {}) do
    if not seen[s.backend] then
      seen[s.backend] = true
      out[#out + 1] = { key = "status@" .. tostring(s.backend), session = s.id }
    end
  end
  return out
end

---------------------------------------------------------------------------
-- Reading answers.
---------------------------------------------------------------------------

local function first_line(s)
  s = tostring(s or "")
  return (s:match("^%s*(.-)%s*$") or s):match("^[^\n]*") or ""
end

--- A read (`status --json`, `config show --json`) as one of:
---   pending        not answered yet (or never asked)
---   failed         could not be run, timed out, or refused, with `error`
---   missing-binary no thurbox-auto-continue where it ran
---   bad-output     it printed something that is not JSON
---   outdated       a `status` whose schema this plugin does not read
---   ok             `data` is the decoded answer
function model.parse_run(run)
  if type(run) ~= "table" or run.state == "pending" or run.state == nil then
    return { kind = "pending" }
  end
  if run.state == "failed" then
    return { kind = "failed", error = run.error or "could not run" }
  end
  if run.timed_out then
    return { kind = "failed", error = "timed out" }
  end
  if run.status == 127 then
    return { kind = "missing-binary" }
  end
  local data = model.decode(run.stdout)
  if run.status ~= 0 then
    local why = type(data) == "table" and data.error or first_line(run.stderr ~= "" and run.stderr or run.stdout)
    return { kind = "failed", error = (why ~= "" and why) or ("exit " .. tostring(run.status)) }
  end
  if type(data) ~= "table" then
    return { kind = "bad-output", error = first_line(run.stdout) }
  end
  if data.sessions ~= nil and data.schema ~= 2 then
    return { kind = "outdated", error = "status schema " .. tostring(data.schema) }
  end
  return { kind = "ok", data = data }
end

--- A setter's answer: pending, ok, or refused with the CLI's own reason.
function model.parse_write(run)
  if type(run) ~= "table" or run.state == "pending" or run.state == nil then
    return { kind = "pending" }
  end
  if run.state == "failed" then
    return { kind = "refused", error = run.error or "could not run" }
  end
  if run.status == 127 then
    return { kind = "refused", error = "thurbox-auto-continue is not installed there" }
  end
  local data = model.decode(run.stdout)
  if run.status == 0 and type(data) == "table" and data.ok == true then
    return { kind = "ok", data = data }
  end
  local why = type(data) == "table" and data.error or first_line(run.stderr ~= "" and run.stderr or run.stdout)
  return { kind = "refused", error = (why ~= "" and why) or ("exit " .. tostring(run.status)) }
end

--- Rows of a status answer, by session id.
function model.index(status)
  local out = {}
  for _, row in ipairs((status and status.sessions) or {}) do
    if type(row.id) == "string" then
      out[row.id] = row
    end
  end
  return out
end

---------------------------------------------------------------------------
-- Validation: the contract's rules, so a plain mistake never starts a process.
---------------------------------------------------------------------------

function model.validate(key, value)
  value = tostring(value or "")
  if key == "message" then
    if value:match("^%s*$") then
      return nil, "the message must not be blank"
    end
    if value:find("[\r\n]") then
      return nil, "the message must be one line"
    end
    if value:find("%c") then
      return nil, "the message must not hold control characters"
    end
    if (utf8.len(value) or #value) > 200 then
      return nil, "the message is at most 200 characters"
    end
    local lead = value:sub(1, 1)
    if lead == "/" or lead == "!" or lead == "#" then
      return nil, "the message must not start with / ! or # — Claude reads those as a command, a shell or a memory"
    end
    return value
  elseif key == "delay_secs" then
    local digits = value:match("^%s*(%d+)%s*$")
    local n = digits and tonumber(digits)
    if not n or n > 86400 then
      return nil, "the delay is whole seconds from 0 to 86400"
    end
    return tostring(math.floor(n))
  elseif key == "enabled" then
    if value == "on" or value == "off" then
      return value
    end
    return nil, "enabled is on or off"
  end
  return nil, "unknown setting " .. key
end

---------------------------------------------------------------------------
-- Words.
---------------------------------------------------------------------------

--- A span of time as a person says it.
function model.duration(ms)
  local s = math.floor(math.max(0, ms or 0) / 1000)
  if s <= 0 then
    return "now"
  elseif s < 60 then
    return s .. "s"
  elseif s < 3600 then
    local m, r = s // 60, s % 60
    return r > 0 and (m .. "m " .. r .. "s") or (m .. "m")
  elseif s < 86400 then
    local h, m = s // 3600, (s % 3600) // 60
    return m > 0 and (h .. "h " .. m .. "m") or (h .. "h")
  end
  local d, h = s // 86400, (s % 86400) // 3600
  return h > 0 and (d .. "d " .. h .. "h") or (d .. "d")
end

--- "in 4m" / "4m ago" against `now`.
function model.relative(at_ms, now)
  if type(at_ms) ~= "number" or type(now) ~= "number" then
    return "at an unknown time"
  end
  if at_ms >= now then
    local d = model.duration(at_ms - now)
    return d == "now" and "now" or ("in " .. d)
  end
  local d = model.duration(now - at_ms)
  return d == "now" and "just now" or (d .. " ago")
end

-- What each skip or give-up reason means, and whether it is the good kind of
-- skip: somebody (you, Claude's own resume, a restart) moved the session on
-- before the send, so it was superseded rather than refused.
local REASONS = {
  ["transcript-moved"] = { "you or Claude resumed first", true },
  ["hook-state-moved"] = { "the session reported new activity first", true },
  ["armed-wait"] = { "Claude's own countdown was running", true },
  ["other-text"] = { "somebody was typing in the composer", true },
  ["moved-before-enter"] = { "the session moved before Enter", true },
  ["disabled"] = { "turned off before the send", false },
  ["extension-inactive"] = { "the extension was deactivated", false },
  ["window"] = { "a window it does not act on", false },
  ["max-attempts"] = { "every attempt was rejected again", false },
  ["session-gone"] = { "the session is gone", false },
  ["not-claude"] = { "not a Claude session", false },
  ["remote"] = { "runs on another host", false },
  ["stopped"] = { "the session was stopped", false },
  ["verify-failed"] = { "the typed text did not verify", false },
  ["unknown-screen"] = { "a screen it did not recognise", false },
  ["unknown"] = { "a screen it did not recognise", false },
  ["limit-menu"] = { "the usage-limit menu was open", false },
  ["error"] = { "an error while sending", false },
}

--- The reason in words, and whether it means the send was superseded.
function model.reason(reason)
  local r = REASONS[reason or ""]
  if r then
    return r[1], r[2]
  end
  return tostring(reason or "no reason given"), false
end

-- Why a host cannot be asked (the contract's unsupported reasons).
local HOST_REASONS = {
  ["not-shared"] = "does not share its sessions",
  ["unknown-host"] = "is not in hosts.toml",
  ["not-installed"] = "not installed",
  ["outdated"] = "too old to be asked",
  ["unreachable"] = "did not answer",
  ["unknown-to-host"] = "does not know this session",
  ["transitive"] = "is reached through a further host",
}

--- A remote row the CLI could not read, in words; nil when there is none.
function model.host_problem(row)
  local host = row and row.host
  if type(host) ~= "table" or host.reason == nil then
    return nil
  end
  local why = HOST_REASONS[host.reason] or host.reason
  if host.reason == "not-installed" then
    return "not installed on " .. tostring(host.backend)
  end
  return tostring(host.backend) .. " " .. why
end

--- Is this row one the pane may offer controls for?
function model.controllable(row)
  if type(row) ~= "table" then
    return false, "no status for this session yet"
  end
  if row.ineligible_reason == "not-claude" or (row.agent and row.agent ~= "claude") then
    return false, "not a Claude session"
  end
  local problem = model.host_problem(row)
  if problem then
    return false, "unavailable: " .. problem
  end
  return true
end

--- The one-line state of a session for the monitor, as `{ text, tone }` where
--- tone is one of: idle, armed, ok, warn, bad, muted.
function model.summary(row, now)
  if not row then
    return { text = "not in the last status", tone = "muted" }
  end
  if row.ineligible_reason == "not-claude" then
    return { text = "not a Claude session", tone = "muted" }
  end
  local problem = model.host_problem(row)
  if problem then
    return { text = "unavailable: " .. problem, tone = "muted" }
  end
  local ep = row.episode
  if type(ep) ~= "table" then
    return { text = "no limit yet", tone = row.enabled and "idle" or "muted" }
  end
  if ep.state == "armed" then
    return {
      text = string.format(
        "armed · %s resets %s · send %s",
        tostring(ep.window or "?"),
        model.relative(ep.resets_at_ms, now),
        model.relative(ep.next_send_at_ms, now)
      ),
      tone = "armed",
    }
  end
  if ep.state == "claimed" then
    return { text = "sending now", tone = "armed" }
  end
  local last = row.last_outcome or ep
  local at = last.at_ms or ep.updated_at_ms
  if ep.state == "sent" then
    return { text = "sent " .. model.relative(ep.sent_at_ms or at, now), tone = "ok" }
  elseif ep.state == "unconfirmed" then
    return { text = "sent, not confirmed " .. model.relative(ep.sent_at_ms or at, now), tone = "warn" }
  elseif ep.state == "skipped" then
    local words, superseded = model.reason(ep.reason)
    if superseded then
      return { text = "superseded " .. model.relative(at, now) .. " · " .. words, tone = "muted" }
    end
    return { text = "skipped " .. model.relative(at, now) .. " · " .. words, tone = "warn" }
  elseif ep.state == "gave-up" then
    return { text = "gave up " .. model.relative(at, now) .. " · " .. (model.reason(ep.reason)), tone = "bad" }
  elseif ep.state == "abandoned" then
    return { text = "abandoned " .. model.relative(at, now) .. " · a run stopped mid-send; never resent", tone = "bad" }
  end
  return { text = tostring(ep.label or ep.state), tone = "muted" }
end

--- The badge a session row wears, or nil for none. Only a Claude session that
--- the CLI could read, and only when it is on or has an episode to report.
function model.badge(row, now)
  if type(row) ~= "table" or not model.controllable(row) then
    return nil
  end
  local ep = row.episode
  if type(ep) ~= "table" then
    if row.enabled then
      return { text = "↻", tone = "idle" }
    end
    return nil
  end
  if ep.state == "armed" then
    local left = type(ep.next_send_at_ms) == "number" and type(now) == "number" and (ep.next_send_at_ms - now) or nil
    return { text = "⏸ " .. (left and model.duration(left) or "?"), tone = "armed" }
  elseif ep.state == "claimed" then
    return { text = "…", tone = "armed" }
  elseif ep.state == "sent" then
    return { text = "✓", tone = "ok" }
  elseif ep.state == "unconfirmed" then
    return { text = "?", tone = "warn" }
  elseif ep.state == "skipped" then
    local _, superseded = model.reason(ep.reason)
    return { text = superseded and "↷" or "⊘", tone = superseded and "muted" or "warn" }
  end
  return { text = "✗", tone = "bad" }
end

---------------------------------------------------------------------------
-- The Settings switch and config.toml.
---------------------------------------------------------------------------

--- Keep the Settings panel's switch and config.toml's global `enabled` in
--- step. `setting` is the switch, `config` the value `status` last read (nil:
--- not read yet), `baseline` the value both last agreed on (nil: never seen).
---
--- Returns `{ write = v }` to set config.toml, `{ adopt = v }` to move the
--- switch, and always the new `baseline`. config.toml is what the headless
--- extension reads, so when both moved apart, or on first sight, it wins.
function model.reconcile(setting, config, baseline)
  if config == nil then
    return { baseline = baseline }
  end
  setting = setting == true
  if baseline == nil then
    if setting == config then
      return { baseline = config }
    end
    return { adopt = config, baseline = config }
  end
  local moved_setting, moved_config = setting ~= baseline, config ~= baseline
  if moved_setting and not moved_config then
    return { write = setting, baseline = setting }
  end
  if moved_config and setting ~= config then
    return { adopt = config, baseline = config }
  end
  return { baseline = config }
end

return model
