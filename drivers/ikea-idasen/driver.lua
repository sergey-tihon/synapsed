local MIN_CM = 62
local MAX_CM = 127
local TOLERANCE_RAW = 40
local INTERVAL_MS = 100
local MAX_MOVE_MS = 45000
local STALL_TICKS = 4
local WAKE = { 0xFE, 0x00 }
local STOP = { 0xFF, 0x00 }
local ABORT = { 0x01, 0x80 }
local FINALLY = {
  { characteristic = "command", bytes = STOP },
  { characteristic = "reference", bytes = ABORT },
}
local move = nil

local function failure(code, message)
  return { code = code, message = message }
end

local function decode(bytes)
  if type(bytes) ~= "table" or #bytes < 4 then
    return nil, nil, failure("bluetooth_error", "Desk height sample is too short.")
  end
  local raw = bytes[1] + bytes[2] * 256
  local speed = bytes[3] + bytes[4] * 256
  if speed >= 32768 then
    speed = speed - 65536
  end
  return raw, speed
end

local function cm(raw)
  return math.floor((620 + raw / 10) + 0.5) / 10
end

local function raw_for(value)
  return math.floor(value * 100 - 6200 + 0.5)
end

local function le16(raw)
  return { raw % 256, raw // 256 }
end

local function observation(raw)
  local value = cm(raw)
  if value >= MIN_CM and value <= MAX_CM then
    return { id = "height", status = "ok", value = value }
  end
  return {
    id = "height",
    status = "error",
    error = failure("bluetooth_error", "Desk reported a height outside 62-127 cm."),
  }
end

local function read_raw(ctx, endpoint)
  local bytes, err = ctx.capabilities.bluetooth:read(endpoint, "height")
  if err then
    return nil, err
  end
  local raw, _, decode_error = decode(bytes)
  if decode_error then
    return nil, decode_error
  end
  return raw
end

return {
  configure = function(settings)
    if type(settings) ~= "table" then
      error("IKEA IDÅSEN settings accept only an optional name string.")
    end
    for key in pairs(settings) do
      if key ~= "name" then
        error("IKEA IDÅSEN settings accept only an optional name string.")
      end
    end
    if settings.name ~= nil and (type(settings.name) ~= "string" or #settings.name == 0 or #settings.name > 64) then
      error("IKEA IDÅSEN settings accept only an optional name string.")
    end

    return {
      discover = function(ctx)
        local bt = ctx.capabilities.bluetooth
        local inventory, err = bt:enumerate()
        if err then
          return { error = err }
        end
        if #inventory.errors > 0 then
          return { error = inventory.errors[1] }
        end
        local candidates = {}
        for _, endpoint in ipairs(inventory.endpoints) do
          if settings.name == nil or endpoint.productName == settings.name then
            candidates[#candidates + 1] = endpoint
          end
        end
        if #candidates == 0 then
          return { devices = {} }
        end
        if #candidates > 1 then
          return {
            error = failure(
              "ambiguous_device",
              "More than one desk is visible; set plugins.settings.ikea-idasen.name."
            ),
          }
        end

        local endpoint = candidates[1]
        local ready, connect_error = bt:connect(endpoint)
        if connect_error then
          return { error = connect_error }
        end
        if not ready then
          return { devices = {} }
        end
        local raw, read_error = read_raw(ctx, endpoint)
        if read_error then
          return { error = read_error }
        end

        return {
          devices = {
            {
              id = "desk",
              kind = "desk",
              label = "Desk",
              endpoints = { endpoint },
              identityPolicy = "same",
              characteristics = {
                {
                  id = "height",
                  label = "Height",
                  spec = {
                    kind = "number",
                    minimum = MIN_CM,
                    maximum = MAX_CM,
                    step = 1,
                    integer = false,
                    unit = "cm",
                  },
                  access = { read = true, write = true, notify = true },
                  writes = {
                    bluetooth = {
                      { characteristic = "command" },
                      { characteristic = "reference" },
                    },
                  },
                },
                {
                  id = "stop",
                  label = "Stop",
                  spec = { kind = "action" },
                  access = { read = false, write = true, notify = false },
                  writes = {
                    bluetooth = {
                      { characteristic = "command" },
                      { characteristic = "reference" },
                    },
                  },
                },
              },
              onStart = function(ctx)
                local function on_height(event_ctx, event)
                  if event.error then
                    move = nil
                    event_ctx.publish({
                      {
                        id = "height",
                        status = "error",
                        error = event.error,
                      },
                    })
                    return nil
                  end
                  local sample, _, sample_error = decode(event.bytes)
                  if sample_error then
                    event_ctx.publish({
                      {
                        id = "height",
                        status = "error",
                        error = sample_error,
                      },
                    })
                    return nil
                  end
                  event_ctx.publish({ observation(sample) })
                  if move then
                    move.last_raw = sample
                    if math.abs(sample - move.target) <= TOLERANCE_RAW then
                      local _, speed = decode(event.bytes)
                      if speed == 0 then
                        event_ctx.capabilities.bluetooth:stop_repeat(endpoint)
                        move = nil
                      end
                    end
                  end
                  return nil
                end
                local _, subscribe_error = ctx.subscribe("bluetooth", endpoint, "height", on_height)
                if subscribe_error then
                  return { error = subscribe_error }
                end
                ctx.every(500, function(tick_ctx)
                  if move then
                    if move.last_raw == move.tick_raw then
                      move.idle = move.idle + 1
                    else
                      move.idle = 0
                      move.tick_raw = move.last_raw
                    end
                    if move.idle >= STALL_TICKS then
                      tick_ctx.capabilities.bluetooth:stop_repeat(endpoint)
                      move = nil
                    end
                  end
                  return nil
                end)
                return nil
              end,
              onRefresh = function(refresh_ctx)
                local current, current_error = read_raw(refresh_ctx, endpoint)
                if current_error then
                  return {
                    updates = {
                      {
                        id = "height",
                        status = "error",
                        error = current_error,
                      },
                    },
                  }
                end
                return { updates = { observation(current) } }
              end,
              onWrite = function(write_ctx, id, command)
                local write_bt = write_ctx.capabilities.bluetooth
                if id == "stop" then
                  local _, stop_error = write_bt:stop_repeat(endpoint)
                  if stop_error then
                    return { error = stop_error }
                  end
                  local _, command_error = write_bt:write(endpoint, "command", STOP)
                  if command_error then
                    return { error = command_error }
                  end
                  local _, reference_error = write_bt:write(endpoint, "reference", ABORT)
                  if reference_error then
                    return { error = reference_error }
                  end
                  move = nil
                  return nil
                end
                if id ~= "height" then
                  return {
                    error = failure("unknown_characteristic", "The characteristic is not declared by this driver."),
                  }
                end
                if command.op ~= "set" and command.op ~= "step" then
                  return {
                    error = failure("unsupported_operation", "Desk height supports Set and Step commands."),
                  }
                end
                local current, current_error = read_raw(write_ctx, endpoint)
                if current_error then
                  return { error = current_error }
                end
                local target_cm
                if command.op == "set" then
                  target_cm = command.value
                  if type(target_cm) ~= "number" or target_cm < MIN_CM or target_cm > MAX_CM then
                    return {
                      error = failure("invalid_value", "Desk height must be between 62 and 127 cm."),
                    }
                  end
                else
                  target_cm = math.max(MIN_CM, math.min(MAX_CM, cm(current) + command.steps))
                end
                local target = raw_for(target_cm)
                if math.abs(target - current) <= TOLERANCE_RAW then
                  move = nil
                  return nil
                end

                local _, stop_error = write_bt:stop_repeat(endpoint)
                if stop_error then
                  return { error = stop_error }
                end
                local _, wake_error = write_bt:write(endpoint, "command", WAKE)
                if wake_error then
                  return { error = wake_error }
                end
                local _, clear_error = write_bt:write(endpoint, "command", STOP)
                if clear_error then
                  return { error = clear_error }
                end
                local _, repeat_error = write_bt:start_repeat(endpoint, "reference", le16(target), {
                  intervalMs = INTERVAL_MS,
                  maxDurationMs = MAX_MOVE_MS,
                  finally = FINALLY,
                })
                if repeat_error then
                  return { error = repeat_error }
                end
                move = {
                  target = target,
                  last_raw = current,
                  tick_raw = current,
                  idle = 0,
                }
                return nil
              end,
              onStop = function()
                move = nil
              end,
            },
          },
        }
      end,
    }
  end,
}
