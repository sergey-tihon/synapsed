return {
  configure = function(settings)
    for key in pairs(settings) do
      if key ~= "deviceId" and key ~= "endpoints" and key ~= "failStart" and key ~= "same" then
        error("unknown claim setting")
      end
    end
    local device_id = settings.deviceId or "claimed"
    local indices = settings.endpoints or { 1 }
    if type(device_id) ~= "string" or type(indices) ~= "table" or #indices == 0 then
      error("invalid claim settings")
    end
    for _, index in ipairs(indices) do
      if type(index) ~= "number" or index < 1 or index > 256 or index ~= math.floor(index) then
        error("invalid endpoint index")
      end
    end
    return {
      discover = function(ctx)
        local inventory, err = ctx.capabilities.ddc:enumerate()
        if not inventory then
          return { error = err }
        end
        if #inventory.errors > 0 then
          return { error = inventory.errors[1] }
        end
        local endpoints = {}
        for _, index in ipairs(indices) do
          if not inventory.endpoints[index] then
            return { devices = {} }
          end
          endpoints[#endpoints + 1] = inventory.endpoints[index]
        end
        local reading, err = ctx.capabilities.ddc:get_vcp(endpoints[1], 16)
        if not reading then
          return { error = err }
        end
        return {
          devices = {
            {
              id = device_id,
              kind = "instrument",
              label = "Multi-endpoint transport fixture",
              endpoints = endpoints,
              identityPolicy = settings.same and "same" or "slot",
              pollIntervalMs = 500,
              characteristics = {
                {
                  id = "level",
                  label = "Level",
                  spec = { kind = "number", minimum = 0, maximum = reading.maximum, step = 1, integer = true },
                  access = { read = true, write = true, notify = true },
                  writes = { ddc = { { code = 16, changeMask = 65535 } } },
                },
              },
              onStart = function(ctx)
                if settings.failStart then
                  error("a conflicting claim must not activate")
                end
              end,
              onRefresh = function(ctx)
                local reading, err = ctx.capabilities.ddc:get_vcp(endpoints[1], 16)
                if not reading then
                  return { error = err }
                end
                return { updates = { { id = "level", status = "ok", value = reading.current } } }
              end,
              onWrite = function(ctx, id, command)
                for _, endpoint in ipairs(endpoints) do
                  local reading, err = ctx.capabilities.ddc:get_vcp(endpoint, 16)
                  if not reading then
                    return { error = err }
                  end
                  local value = command.op == "step"
                      and math.max(0, math.min(reading.maximum, reading.current + command.steps))
                    or command.value
                  if value < 0 or value > reading.maximum then
                    return { error = { code = "invalid_value", message = "Value outside fresh range." } }
                  end
                  if value ~= reading.current then
                    local _, err = ctx.capabilities.ddc:set_vcp(endpoint, 16, value)
                    if err then
                      return { error = err }
                    end
                  end
                end
              end,
            },
          },
        }
      end,
    }
  end,
}
