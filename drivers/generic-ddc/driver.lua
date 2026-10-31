local monitor = require("monitor")

local function refresh(ctx, endpoint)
  local reading, err = monitor.read_brightness(ctx, endpoint)
  if err then
    return {
      updates = {
        {
          id = "brightness",
          status = err.code == "device_unavailable" and "unavailable" or "error",
          value = nil,
          error = err,
        },
      },
    }
  end
  return {
    updates = {
      { id = "brightness", status = "ok", value = reading.current, error = nil },
    },
  }
end

return {
  configure = function(settings)
    if type(settings) ~= "table" or next(settings) ~= nil then
      error("Generic DDC settings must be an empty table.")
    end
    return {
      discover = function(ctx)
        local candidate, err = monitor.find_single(ctx)
        if err then
          return { error = err }
        end
        if not candidate then
          return { devices = {} }
        end
        local endpoint = candidate.endpoint
        return {
          devices = {
            {
              id = "monitor",
              kind = "monitor",
              label = "Monitor",
              endpoints = { endpoint },
              identityPolicy = "slot",
              characteristics = {
                monitor.brightness_definition(candidate.brightness.maximum),
              },
              pollIntervalMs = 2000,
              onRefresh = function(refresh_ctx)
                return refresh(refresh_ctx, endpoint)
              end,
              onWrite = function(write_ctx, characteristic_id, command)
                if characteristic_id ~= "brightness" then
                  return {
                    error = {
                      code = "unknown_characteristic",
                      message = "The characteristic is not declared by this driver.",
                    },
                  }
                end
                return monitor.write_brightness(write_ctx, endpoint, command)
              end,
            },
          },
        }
      end,
    }
  end,
}
