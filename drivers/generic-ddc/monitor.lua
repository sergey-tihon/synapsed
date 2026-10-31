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

function monitor.read_brightness(ctx, endpoint)
  local value, err = ctx.capabilities.ddc:get_vcp(endpoint, 0x10)
  if err then
    return nil, err
  end
  if not valid_reading(value) then
    return nil, failure("ddc_error", "Brightness reading is outside its reported range.")
  end
  return value, nil
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

function monitor.brightness_definition(maximum)
  return {
    id = "brightness",
    label = "Brightness",
    spec = {
      kind = "number",
      minimum = 0,
      maximum = maximum,
      step = math.max(1, math.floor(maximum / 20)),
      integer = true,
      unit = nil,
    },
    access = { read = true, write = true, notify = true },
    writes = { ddc = { { code = 0x10, changeMask = 0xFFFF } } },
  }
end

function monitor.write_brightness(ctx, endpoint, command)
  if command.op ~= "set" and command.op ~= "step" then
    return { error = failure("unsupported_operation", "Brightness accepts only set and step commands.") }
  end

  -- Even zero/unchanged commands read hardware; discovery values are not authority.
  local reading, err = monitor.read_brightness(ctx, endpoint)
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
      return { error = failure("invalid_value", "Brightness must be an integer within the fresh hardware range.") }
    end
    next_value = value
  else
    local steps = command.steps
    if type(steps) ~= "number" or steps ~= steps or steps < -100 or steps > 100 or steps ~= math.floor(steps) then
      return { error = failure("invalid_value", "Brightness steps must be an integer from -100 to 100.") }
    end
    local delta = steps * math.max(1, math.floor(reading.maximum / 20))
    next_value = math.max(0, math.min(reading.maximum, reading.current + delta))
  end

  if next_value ~= reading.current then
    local observed, write_error = ctx.capabilities.ddc:set_vcp(endpoint, 0x10, next_value)
    if write_error then
      return { error = write_error }
    end
    if not valid_reading(observed) then
      return { error = failure("ddc_error", "Brightness readback is outside its reported range.") }
    end
  end
  -- The host performs onRefresh under this same lease to confirm public state.
  return nil
end

return monitor
