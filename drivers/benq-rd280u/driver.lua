local monitor = require("generic-ddc.monitor")

local BLUE = 0x19
local SWITCH = 0xD7
local LEVEL = 0xD9
local SWITCH_MAXIMUM = 0x0231
local LEVEL_MAXIMUM = 0x070A
local CONTRAST = 0x12
local AMBIENT = 0xE1
local AMBIENT_MAXIMUM = 0xFFFF

local function failure(code, message)
  return { code = code, message = message }
end

local function in_group(group, definition)
  definition.group = group
  return definition
end

local function register_valid(value, maximum)
  if value.maximum ~= maximum then
    return failure("ddc_error", "A BenQ register no longer reports its required range.")
  end
  return nil
end

local function read_register(ctx, endpoint, code, maximum)
  local value, err = ctx.capabilities.ddc:get_vcp(endpoint, code)
  if err then
    return nil, err
  end
  local range_error = register_valid(value, maximum)
  if range_error then
    return nil, range_error
  end
  return value, nil
end

local function bounded_level(value, maximum)
  if value > maximum then
    return nil, failure("ddc_error", "A BenQ field is outside its supported range.")
  end
  return value, nil
end

local function observe_field(id, raw)
  if id == "low_blue_light" then
    return bounded_level(raw, 5)
  elseif id == "moonhalo.brightness" then
    return bounded_level(raw & 0x00FF, 10)
  elseif id == "moonhalo.color_temperature" then
    return bounded_level(raw >> 8, 7)
  elseif id == "moonhalo.power" then
    local state = raw & 0x00F0
    if state == 0x10 then
      return false, nil
    elseif state == 0x20 or state == 0x30 then
      return true, nil
    end
    return nil, failure("ddc_error", "MoonHalo power has an unsupported packed state.")
  elseif id == "moonhalo.light_mode" then
    local mode = raw >> 8
    if mode == 1 then
      return "270deg", nil
    elseif mode == 2 then
      return "360deg", nil
    end
    return nil, failure("ddc_error", "MoonHalo light mode has an unsupported packed state.")
  end
end

local function update(id, value, err)
  if err then
    return {
      id = id,
      status = err.code == "device_unavailable" and "unavailable" or "error",
      value = nil,
      error = err,
    }
  end
  return { id = id, status = "ok", value = value, error = nil }
end

local function field_update(id, reading, err)
  if err then
    return update(id, nil, err)
  end
  local value, field_error = observe_field(id, reading.current)
  return update(id, value, field_error)
end

local function refresh(ctx, endpoint)
  local brightness, brightness_error = monitor.read_brightness(ctx, endpoint)
  local contrast, contrast_error = monitor.read_level(ctx, endpoint, CONTRAST, "Contrast")
  local blue, blue_error = read_register(ctx, endpoint, BLUE, 5)
  -- Each packed register is sampled once; its fields share the same observation.
  local switch, switch_error = read_register(ctx, endpoint, SWITCH, SWITCH_MAXIMUM)
  local level, level_error = read_register(ctx, endpoint, LEVEL, LEVEL_MAXIMUM)
  local ambient, ambient_error = read_register(ctx, endpoint, AMBIENT, AMBIENT_MAXIMUM)
  return {
    updates = {
      update("brightness", brightness and brightness.current or nil, brightness_error),
      update("contrast", contrast and contrast.current or nil, contrast_error),
      field_update("low_blue_light", blue, blue_error),
      field_update("moonhalo.power", switch, switch_error),
      field_update("moonhalo.brightness", level, level_error),
      field_update("moonhalo.color_temperature", level, level_error),
      field_update("moonhalo.light_mode", switch, switch_error),
      update("ambient_light", ambient and ambient.current or nil, ambient_error),
    },
  }
end

local function number_definition(id, label, maximum, code, mask)
  return {
    id = id,
    label = label,
    spec = {
      kind = "number",
      minimum = 0,
      maximum = maximum,
      step = 1,
      integer = true,
      unit = nil,
    },
    access = { read = true, write = true, notify = true },
    writes = { ddc = { { code = code, changeMask = mask } } },
  }
end

local function characteristics(brightness_maximum, contrast_maximum)
  return {
    in_group("general", monitor.brightness_definition(brightness_maximum)),
    in_group("general", monitor.level_definition("contrast", "Contrast", CONTRAST, contrast_maximum)),
    in_group("general", number_definition("low_blue_light", "Low Blue Light", 5, BLUE, 0xFFFF)),
    in_group("moonhalo", {
      id = "moonhalo.power",
      label = "MoonHalo Power",
      spec = { kind = "boolean" },
      access = { read = true, write = true, notify = true },
      writes = { ddc = { { code = SWITCH, changeMask = 0x00FE } } },
    }),
    in_group("moonhalo", number_definition("moonhalo.brightness", "MoonHalo Brightness", 10, LEVEL, 0x00FF)),
    in_group(
      "moonhalo",
      number_definition("moonhalo.color_temperature", "MoonHalo Color Temperature", 7, LEVEL, 0xFF00)
    ),
    in_group("moonhalo", {
      id = "moonhalo.light_mode",
      label = "MoonHalo Light Mode",
      spec = {
        kind = "enum",
        options = {
          { value = "270deg", label = "270°" },
          { value = "360deg", label = "360°" },
        },
      },
      access = { read = true, write = true, notify = true },
      writes = { ddc = { { code = SWITCH, changeMask = 0xFF00 } } },
    }),
    {
      id = "ambient_light",
      label = "Ambient Light",
      group = "sensors",
      spec = { kind = "number", minimum = 0, maximum = AMBIENT_MAXIMUM, integer = true },
      access = { read = true, write = false, notify = true },
      writes = {},
    },
  }
end

local function stepped_value(steps, current, minimum, maximum)
  if type(steps) ~= "number" or steps ~= steps or steps < -100 or steps > 100 or steps ~= math.floor(steps) then
    return nil, failure("invalid_value", "Steps must be an integer from -100 to 100.")
  end
  return math.max(minimum, math.min(maximum, current + steps)), nil
end

local function numeric_target(command, current, maximum)
  if command.op == "step" then
    return stepped_value(command.steps, current, 0, maximum)
  elseif command.op == "set" then
    local value = command.value
    if type(value) ~= "number" or value ~= value or value < 0 or value > maximum or value ~= math.floor(value) then
      return nil, failure("invalid_value", "The value must be an integer within the fresh hardware range.")
    end
    return value, nil
  end
  return nil, failure("unsupported_operation", "This numeric characteristic accepts only set and step commands.")
end

local function power_target(command, current, raw)
  local enabled
  if command.op == "toggle" then
    enabled = not current
  elseif command.op == "set" then
    if type(command.value) ~= "boolean" then
      return nil, failure("invalid_value", "MoonHalo power requires a boolean value.")
    end
    enabled = command.value
  else
    return nil, failure("unsupported_operation", "MoonHalo power accepts only set and toggle commands.")
  end
  -- In particular, a targeted on->on set preserves an existing 0x30 state.
  if enabled == current then
    return raw, nil
  end
  -- Preserve high byte and bit0; changing power also clears low bits1..3.
  return (raw & 0xFF01) | (enabled and 0x20 or 0x10), nil
end

local function mode_target(command, current, raw, maximum)
  local index = current == "270deg" and 1 or 2
  local target, err
  if command.op == "step" then
    target, err = stepped_value(command.steps, index, 1, maximum >> 8)
  elseif command.op == "set" then
    if command.value == "270deg" then
      target = 1
    elseif command.value == "360deg" then
      target = 2
    else
      err = failure("invalid_value", "MoonHalo light mode must be 270deg or 360deg.")
    end
  else
    err = failure("unsupported_operation", "MoonHalo light mode accepts only set and step commands.")
  end
  if err then
    return nil, err
  end
  return (target << 8) | (raw & 0x00FF), nil
end

local function write(ctx, endpoint, id, command)
  if id == "brightness" then
    return monitor.write_brightness(ctx, endpoint, command)
  elseif id == "contrast" then
    return monitor.write_level(ctx, endpoint, CONTRAST, "Contrast", command)
  end
  local code, maximum
  if id == "low_blue_light" then
    code, maximum = BLUE, 5
  elseif id == "moonhalo.power" or id == "moonhalo.light_mode" then
    code, maximum = SWITCH, SWITCH_MAXIMUM
  elseif id == "moonhalo.brightness" or id == "moonhalo.color_temperature" then
    code, maximum = LEVEL, LEVEL_MAXIMUM
  else
    return { error = failure("unknown_characteristic", "The characteristic is not declared by this driver.") }
  end

  local reading, err = read_register(ctx, endpoint, code, maximum)
  if err then
    return { error = err }
  end
  local current, field_error = observe_field(id, reading.current)
  if field_error then
    return { error = field_error }
  end

  local next_raw, target_error
  if id == "moonhalo.power" then
    next_raw, target_error = power_target(command, current, reading.current)
  elseif id == "moonhalo.light_mode" then
    next_raw, target_error = mode_target(command, current, reading.current, reading.maximum)
  else
    local field_maximum = reading.maximum
    if id == "moonhalo.brightness" then
      field_maximum = reading.maximum & 0x00FF
    elseif id == "moonhalo.color_temperature" then
      field_maximum = reading.maximum >> 8
    end
    local target
    target, target_error = numeric_target(command, current, field_maximum)
    if not target_error then
      if id == "moonhalo.brightness" then
        next_raw = (reading.current & 0xFF00) | target
      elseif id == "moonhalo.color_temperature" then
        next_raw = (target << 8) | (reading.current & 0x00FF)
      else
        next_raw = target
      end
    end
  end
  if target_error then
    return { error = target_error }
  end

  if next_raw ~= reading.current then
    local observed, write_error = ctx.capabilities.ddc:set_vcp(endpoint, code, next_raw)
    if write_error then
      return { error = write_error }
    end
    local range_error = register_valid(observed, maximum)
    if range_error then
      return { error = range_error }
    end
    local _, observation_error = observe_field(id, observed.current)
    if observation_error then
      return { error = observation_error }
    end
  end
  -- Never echo the request: the host now refreshes all readable state under the lease.
  return nil
end

local function matches_name(endpoint)
  local name = endpoint.productName
  if name == nil then
    return true
  end
  name = string.lower((name:gsub("^%s+", ""):gsub("%s+$", "")))
  return name == "" or name == "benq rd280u" or name == "rd280u"
end

return {
  configure = function(settings)
    if type(settings) ~= "table" or next(settings) ~= nil then
      error("BenQ RD280U settings must be an empty table.")
    end
    return {
      discover = function(ctx)
        local candidate, err = monitor.find_single(ctx)
        if err then
          return { error = err }
        end
        if not candidate or not matches_name(candidate.endpoint) then
          return { devices = {} }
        end

        local endpoint = candidate.endpoint
        for _, fingerprint in ipairs({
          { code = BLUE, maximum = 5 },
          { code = SWITCH, maximum = SWITCH_MAXIMUM },
          { code = LEVEL, maximum = LEVEL_MAXIMUM },
        }) do
          local _, fingerprint_error = read_register(ctx, endpoint, fingerprint.code, fingerprint.maximum)
          if fingerprint_error then
            if fingerprint_error.code == "ddc_error" or fingerprint_error.code == "unsupported_operation" then
              return { devices = {} }
            end
            return { error = fingerprint_error }
          end
        end

        local contrast, contrast_error = monitor.read_level(ctx, endpoint, CONTRAST, "Contrast")
        if contrast_error then
          if contrast_error.code == "ddc_error" or contrast_error.code == "unsupported_operation" then
            return { devices = {} }
          end
          return { error = contrast_error }
        end

        return {
          devices = {
            {
              id = "monitor",
              kind = "monitor",
              label = "BenQ RD280U",
              endpoints = { endpoint },
              identityPolicy = "same",
              groups = {
                { id = "general", label = "General" },
                { id = "moonhalo", label = "MoonHalo" },
                { id = "sensors", label = "Sensors" },
              },
              characteristics = characteristics(candidate.brightness.maximum, contrast.maximum),
              pollIntervalMs = 2000,
              onRefresh = function(refresh_ctx)
                return refresh(refresh_ctx, endpoint)
              end,
              onWrite = function(write_ctx, characteristic_id, command)
                return write(write_ctx, endpoint, characteristic_id, command)
              end,
            },
          },
        }
      end,
    }
  end,
}
