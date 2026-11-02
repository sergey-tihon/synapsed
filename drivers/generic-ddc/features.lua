local monitor = require("monitor")

local features = {}

features.GROUPS = {
  { id = "picture", label = "Picture" },
  { id = "color", label = "Color" },
  { id = "audio", label = "Audio" },
  { id = "system", label = "System" },
}

features.BRIGHTNESS = {
  id = "brightness",
  label = "Brightness",
  code = 0x10,
  group = "picture",
  kind = "level",
}

features.OPTIONAL = {
  { id = "contrast", label = "Contrast", code = 0x12, group = "picture", kind = "level" },
  { id = "sharpness", label = "Sharpness", code = 0x87, group = "picture", kind = "level" },
  {
    id = "color_preset",
    label = "Color Preset",
    code = 0x14,
    group = "picture",
    kind = "choice",
    options = {
      { value = "srgb", label = "sRGB", code = 0x01 },
      { value = "native", label = "Native", code = 0x02 },
      { value = "4000k", label = "4000 K", code = 0x03 },
      { value = "5000k", label = "5000 K", code = 0x04 },
      { value = "6500k", label = "6500 K", code = 0x05 },
      { value = "7500k", label = "7500 K", code = 0x06 },
      { value = "8200k", label = "8200 K", code = 0x07 },
      { value = "9300k", label = "9300 K", code = 0x08 },
      { value = "10000k", label = "10000 K", code = 0x09 },
      { value = "11500k", label = "11500 K", code = 0x0A },
      { value = "user1", label = "User 1", code = 0x0B },
      { value = "user2", label = "User 2", code = 0x0C },
      { value = "user3", label = "User 3", code = 0x0D },
    },
  },
  { id = "red_gain", label = "Red Gain", code = 0x16, group = "color", kind = "level" },
  { id = "green_gain", label = "Green Gain", code = 0x18, group = "color", kind = "level" },
  { id = "blue_gain", label = "Blue Gain", code = 0x1A, group = "color", kind = "level" },
  { id = "red_black_level", label = "Red Black Level", code = 0x6C, group = "color", kind = "level" },
  { id = "green_black_level", label = "Green Black Level", code = 0x6E, group = "color", kind = "level" },
  { id = "blue_black_level", label = "Blue Black Level", code = 0x70, group = "color", kind = "level" },
  { id = "volume", label = "Volume", code = 0x62, group = "audio", kind = "level" },
  {
    id = "mute",
    label = "Mute",
    code = 0x8D,
    group = "audio",
    kind = "mute",
  },
  {
    id = "input_source",
    label = "Input Source",
    code = 0x60,
    group = "system",
    kind = "choice",
    options = {
      { value = "vga1", label = "VGA 1", code = 0x01 },
      { value = "vga2", label = "VGA 2", code = 0x02 },
      { value = "dvi1", label = "DVI 1", code = 0x03 },
      { value = "dvi2", label = "DVI 2", code = 0x04 },
      { value = "composite1", label = "Composite 1", code = 0x05 },
      { value = "composite2", label = "Composite 2", code = 0x06 },
      { value = "svideo1", label = "S-Video 1", code = 0x07 },
      { value = "svideo2", label = "S-Video 2", code = 0x08 },
      { value = "tuner1", label = "Tuner 1", code = 0x09 },
      { value = "tuner2", label = "Tuner 2", code = 0x0A },
      { value = "tuner3", label = "Tuner 3", code = 0x0B },
      { value = "component1", label = "Component 1", code = 0x0C },
      { value = "component2", label = "Component 2", code = 0x0D },
      { value = "component3", label = "Component 3", code = 0x0E },
      { value = "dp1", label = "DisplayPort 1", code = 0x0F },
      { value = "dp2", label = "DisplayPort 2", code = 0x10 },
      { value = "hdmi1", label = "HDMI 1", code = 0x11 },
      { value = "hdmi2", label = "HDMI 2", code = 0x12 },
    },
  },
  {
    id = "power_mode",
    label = "Power Mode",
    code = 0xD6,
    group = "system",
    kind = "status",
    options = {
      { value = "on", label = "On", code = 0x01 },
      { value = "standby", label = "Standby", code = 0x02 },
      { value = "suspend", label = "Suspend", code = 0x03 },
      { value = "off", label = "Off", code = 0x04 },
    },
  },
}

local function failure(code, message)
  return { code = code, message = message }
end

local function error_update(feature, err)
  return {
    id = feature.id,
    status = err.code == "device_unavailable" and "unavailable" or "error",
    value = nil,
    error = err,
  }
end

local function error_result(err)
  return { error = err }
end

local function absent_error(err)
  return err.code == "ddc_error" or err.code == "unsupported_operation"
end

function features.probe(ctx, endpoint, feature)
  local value, err = ctx.capabilities.ddc:get_vcp(endpoint, feature.code)
  if err and err.code == "ddc_error" then
    value, err = ctx.capabilities.ddc:get_vcp(endpoint, feature.code)
  end
  if err then
    if absent_error(err) then
      return nil, nil
    end
    return nil, err
  end
  if not value then
    return nil, nil
  end
  if feature.kind == "level" then
    if value.maximum <= 0 or value.current > value.maximum then
      return nil, nil
    end
  elseif (value.current & 0x00FF) == 0 then
    return nil, nil
  end
  return { feature = feature, maximum = value.maximum }, nil
end

local function enum_spec(feature)
  local options = {}
  for _, option in ipairs(feature.options) do
    options[#options + 1] = { value = option.value, label = option.label }
  end
  return { kind = "enum", options = options }
end

function features.definition(entry)
  local feature = entry.feature
  if feature.kind == "level" then
    local definition = monitor.level_definition(feature.id, feature.label, feature.code, entry.maximum)
    definition.group = feature.group
    return definition
  elseif feature.kind == "mute" then
    return {
      id = feature.id,
      label = feature.label,
      group = feature.group,
      spec = { kind = "boolean" },
      access = { read = true, write = true, notify = true },
      writes = { ddc = { { code = feature.code, changeMask = 0x00FF } } },
    }
  elseif feature.kind == "choice" then
    return {
      id = feature.id,
      label = feature.label,
      group = feature.group,
      spec = enum_spec(feature),
      access = { read = true, write = true, notify = true },
      writes = { ddc = { { code = feature.code, changeMask = 0x00FF } } },
    }
  elseif feature.kind == "status" then
    return {
      id = feature.id,
      label = feature.label,
      group = feature.group,
      spec = enum_spec(feature),
      access = { read = true, write = false, notify = true },
      writes = {},
    }
  end
  error("Unknown generic DDC feature kind.")
end

local function option_value(feature, code)
  for _, option in ipairs(feature.options) do
    if option.code == code then
      return option.value
    end
  end
  return nil
end

function features.observe(ctx, endpoint, entry)
  local feature = entry.feature
  if feature.kind == "level" then
    local value, err = monitor.read_level(ctx, endpoint, feature.code, feature.label)
    if err then
      return error_update(feature, err)
    end
    if value.current > entry.maximum then
      return error_update(feature, failure("ddc_error", feature.label .. " reading exceeds its declared range."))
    end
    return { id = feature.id, status = "ok", value = value.current, error = nil }
  end

  local value, err = ctx.capabilities.ddc:get_vcp(endpoint, feature.code)
  if err then
    return error_update(feature, err)
  end
  local current = value.current & 0x00FF
  if feature.kind == "mute" then
    if current == 0x01 then
      return { id = feature.id, status = "ok", value = true, error = nil }
    elseif current == 0x02 then
      return { id = feature.id, status = "ok", value = false, error = nil }
    end
  else
    local selected = option_value(feature, current)
    if selected then
      return { id = feature.id, status = "ok", value = selected, error = nil }
    end
  end
  return error_update(
    feature,
    failure("ddc_error", feature.label .. " reports a value outside the MCCS standard table.")
  )
end

function features.write(ctx, endpoint, entry, command)
  local feature = entry.feature
  if feature.kind == "level" then
    return monitor.write_level(ctx, endpoint, feature.code, feature.label, command)
  elseif feature.kind == "status" then
    return error_result(failure("unsupported_operation", "Power Mode is read-only."))
  elseif feature.kind == "mute" then
    local toggle = command.op == "toggle"
    local muted
    if command.op == "set" then
      if type(command.value) ~= "boolean" then
        return error_result(failure("invalid_value", "Mute requires a boolean value."))
      end
      muted = command.value
    elseif not toggle then
      return error_result(failure("unsupported_operation", "Mute accepts only set and toggle commands."))
    end

    local raw, err = ctx.capabilities.ddc:get_vcp(endpoint, feature.code)
    if err then
      return error_result(err)
    end
    local current = raw.current & 0x00FF
    if current ~= 0x01 and current ~= 0x02 then
      return error_result(failure("ddc_error", feature.label .. " reports a value outside the MCCS standard table."))
    end
    if toggle then
      muted = current == 0x02
    end
    local requested = (raw.current & 0xFF00) | (muted and 0x01 or 0x02)
    if requested ~= raw.current then
      local observed, set_error = ctx.capabilities.ddc:set_vcp(endpoint, feature.code, requested)
      if set_error then
        return error_result(set_error)
      end
      local readback = observed.current & 0x00FF
      if readback ~= 0x01 and readback ~= 0x02 then
        return error_result(failure("ddc_error", "Mute readback is outside the MCCS standard table."))
      end
    end
    return nil
  elseif feature.kind == "choice" then
    if command.op ~= "set" then
      return error_result(failure("unsupported_operation", feature.label .. " accepts only set commands."))
    end
    local selected = nil
    for _, option in ipairs(feature.options) do
      if option.value == command.value then
        selected = option
        break
      end
    end
    if not selected then
      return error_result(failure("invalid_value", feature.label .. " must be one of its declared options."))
    end
    local raw, err = ctx.capabilities.ddc:get_vcp(endpoint, feature.code)
    if err then
      return error_result(err)
    end
    local requested = (raw.current & 0xFF00) | selected.code
    if requested ~= raw.current then
      local _, set_error = ctx.capabilities.ddc:set_vcp(endpoint, feature.code, requested)
      if set_error then
        return error_result(set_error)
      end
    end
    return nil
  end
  return error_result(failure("unsupported_operation", "Unsupported generic DDC feature operation."))
end

return features
