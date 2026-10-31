local function checked(value, err)
  if not value then
    error("fixture transport failure")
  end
  return value
end
local function read(ctx, endpoint, code)
  return checked(ctx.capabilities.ddc:get_vcp(endpoint, code)).current
end
local function set(ctx, endpoint, code, value)
  read(ctx, endpoint, code)
  checked(ctx.capabilities.ddc:set_vcp(endpoint, code, value))
end
local function observed(id, value)
  return { id = id, status = "ok", value = value }
end
local function definition(id, spec, codes, writable)
  local bindings = {}
  for _, code in ipairs(codes or {}) do
    bindings[#bindings + 1] = { code = code, changeMask = 65535 }
  end
  return {
    id = id,
    label = id,
    spec = spec,
    access = { read = true, write = writable or false, notify = true },
    writes = writable and { ddc = bindings } or {},
  }
end
local function number(maximum, step, integer)
  return { kind = "number", minimum = 0, maximum = maximum, step = step, integer = integer }
end
return {
  configure = function(settings)
    if next(settings) ~= nil then
      error("unknown fixture setting")
    end
    return {
      discover = function(ctx)
        local inventory = checked(ctx.capabilities.ddc:enumerate())
        if #inventory.endpoints ~= 1 then
          return { devices = {} }
        end
        local endpoint = inventory.endpoints[1]
        local string_codes = { 36 }
        for code = 37, 69 do
          if code ~= 45 then
            string_codes[#string_codes + 1] = code
          end
        end
        local chars = {
          definition("temperature", number(100, 0.1, false), nil, false),
          definition("measurement", number(100, 0.1, false), { 33 }, true),
          definition("switch", { kind = "boolean" }, { 34 }, true),
          definition("choice", {
            kind = "enum",
            options = { { value = "first", label = "First" }, { value = "second", label = "Second" } },
          }, { 35 }, true),
          definition("text", { kind = "string", maxLength = 16 }, string_codes, true),
          definition("invocations", number(65535, 1, true), nil, false),
          {
            id = "run",
            label = "Run",
            spec = { kind = "action" },
            access = { read = false, write = true, notify = false },
            writes = { ddc = { { code = 45, changeMask = 65535 } } },
          },
        }
        local function refresh(ctx)
          local length = read(ctx, endpoint, 36)
          if length > 64 then
            error("invalid fixture string length")
          end
          local bytes = {}
          for i = 1, 32 do
            local code = i <= 8 and 36 + i or 37 + i
            local packed = read(ctx, endpoint, code)
            bytes[#bytes + 1] = string.char(math.floor(packed / 256), packed % 256)
          end
          local text = table.concat(bytes):sub(1, length)
          return {
            updates = {
              observed("temperature", read(ctx, endpoint, 32) / 10),
              observed("measurement", read(ctx, endpoint, 33) / 10),
              observed("switch", read(ctx, endpoint, 34) == 1),
              observed("choice", read(ctx, endpoint, 35) == 0 and "first" or "second"),
              observed("text", text),
              observed("invocations", read(ctx, endpoint, 45)),
            },
          }
        end
        return {
          devices = {
            {
              id = "fixture",
              kind = "instrument",
              label = "Transport-backed Lua fixture",
              endpoints = { endpoint },
              identityPolicy = "slot",
              characteristics = chars,
              onStart = function(ctx)
                ctx.publish({ observed("temperature", read(ctx, endpoint, 32) / 10) })
                ctx.every(500, function(ctx)
                  ctx.publish({ observed("temperature", read(ctx, endpoint, 32) / 10) })
                end)
                local _, err = ctx.subscribe("ddc", endpoint, "vcp", function()
                  error("DDC must not emit events")
                end)
                if not err or err.code ~= "unsupported_operation" then
                  error("unsupported event contract")
                end
              end,
              onRefresh = refresh,
              onWrite = function(ctx, id, command)
                if id == "measurement" then
                  local current = read(ctx, endpoint, 33)
                  local value = command.op == "step" and math.max(0, math.min(1000, current + command.steps))
                    or math.floor(command.value * 10 + 0.5)
                  set(ctx, endpoint, 33, value)
                elseif id == "switch" then
                  local current = read(ctx, endpoint, 34)
                  local value = command.op == "toggle" and (current == 1 and 0 or 1) or (command.value and 1 or 0)
                  set(ctx, endpoint, 34, value)
                elseif id == "choice" then
                  local current = read(ctx, endpoint, 35)
                  local value = command.op == "step" and math.max(0, math.min(1, current + command.steps))
                    or (command.value == "first" and 0 or 1)
                  set(ctx, endpoint, 35, value)
                elseif id == "text" then
                  local value = command.value
                  if #value > 64 then
                    return { error = { code = "invalid_value", message = "Fixture string byte limit exceeded." } }
                  end
                  for i = 1, 32 do
                    local code = i <= 8 and 36 + i or 37 + i
                    set(ctx, endpoint, code, (value:byte(i * 2 - 1) or 0) * 256 + (value:byte(i * 2) or 0))
                  end
                  set(ctx, endpoint, 36, #value)
                elseif id == "run" then
                  set(ctx, endpoint, 45, read(ctx, endpoint, 45) + 1)
                else
                  error("unexpected fixture characteristic")
                end
              end,
            },
          },
        }
      end,
    }
  end,
}
