local monitor = {}

local function failure(code, message)
  return { code = code, message = message }
end

local function valid_reading(value)
  return value.maximum > 0 and value.current <= value.maximum
end

local function prevents_selection(err)
  return err.code ~= "ddc_error" and err.code ~= "unsupported_operation"
end

function monitor.read_level(ctx, endpoint, code, name)
  local value, err = ctx.capabilities.ddc:get_vcp(endpoint, code)
  if err then
    return nil, err
  end
  if not valid_reading(value) then
    return nil, failure("ddc_error", name .. " reading is outside its reported range.")
  end
  return value, nil
end

function monitor.read_brightness(ctx, endpoint)
  return monitor.read_level(ctx, endpoint, 0x10, "Brightness")
end

function monitor.find_single(ctx)
  local inventory, err = ctx.capabilities.ddc:enumerate()
  if err then
    return nil, err
  end
  -- An inaccessible eligible bus may hide a second monitor.
  if #inventory.errors > 0 then
    return nil, inventory.errors[1]
  end

  local candidate = nil
  for _, endpoint in ipairs(inventory.endpoints) do
    local brightness, read_error = monitor.read_brightness(ctx, endpoint)
    if read_error then
      if prevents_selection(read_error) then
        return nil, read_error
      end
    elseif brightness then
      if candidate then
        return nil, failure("ambiguous_device", "More than one brightness-capable monitor is connected.")
      end
      candidate = { endpoint = endpoint, brightness = brightness }
    end
  end
  return candidate, nil
end

function monitor.level_definition(id, label, code, maximum)
  return {
    id = id,
    label = label,
    spec = {
      kind = "number",
      minimum = 0,
      maximum = maximum,
      step = math.max(1, math.floor(maximum / 20)),
      integer = true,
      unit = nil,
    },
    access = { read = true, write = true, notify = true },
    writes = { ddc = { { code = code, changeMask = 0xFFFF } } },
  }
end

function monitor.brightness_definition(maximum)
  return monitor.level_definition("brightness", "Brightness", 0x10, maximum)
end

function monitor.write_level(ctx, endpoint, code, name, command)
  if command.op ~= "set" and command.op ~= "step" then
    return { error = failure("unsupported_operation", name .. " accepts only set and step commands.") }
  end

  -- Even zero/unchanged commands read hardware; discovery values are not authority.
  local reading, err = monitor.read_level(ctx, endpoint, code, name)
  if err then
    return { error = err }
  end

  local next_value
  if command.op == "set" then
    local value = command.value
    if
      type(value) ~= "number"
      or value ~= value
      or value < 0
      or value > reading.maximum
      or value ~= math.floor(value)
    then
      return { error = failure("invalid_value", name .. " must be an integer within the fresh hardware range.") }
    end
    next_value = value
  else
    local steps = command.steps
    if type(steps) ~= "number" or steps ~= steps or steps < -100 or steps > 100 or steps ~= math.floor(steps) then
      return { error = failure("invalid_value", name .. " steps must be an integer from -100 to 100.") }
    end
    local delta = steps * math.max(1, math.floor(reading.maximum / 20))
    next_value = math.max(0, math.min(reading.maximum, reading.current + delta))
  end

  if next_value ~= reading.current then
    local observed, write_error = ctx.capabilities.ddc:set_vcp(endpoint, code, next_value)
    if write_error then
      return { error = write_error }
    end
    if not valid_reading(observed) then
      return { error = failure("ddc_error", name .. " readback is outside its reported range.") }
    end
  end
  -- The host performs onRefresh under this same lease to confirm public state.
  return nil
end

function monitor.write_brightness(ctx, endpoint, command)
  return monitor.write_level(ctx, endpoint, 0x10, "Brightness", command)
end

return monitor
