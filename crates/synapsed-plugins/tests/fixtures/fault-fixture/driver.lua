return {
  configure = function(settings)
    local mode = settings.mode or "ordinary"
    for key in pairs(settings) do
      if key ~= "mode" then
        error("unknown setting")
      end
    end
    local calls = 0
    return {
      discover = function(ctx)
        local inventory, err = ctx.capabilities.ddc:enumerate()
        if not inventory then
          return { error = err }
        end
        if #inventory.endpoints ~= 1 then
          return { devices = {} }
        end
        local endpoint = inventory.endpoints[1]
        if mode == "discovery-set" then
          pcall(function()
            ctx.capabilities.ddc:set_vcp(endpoint, 16, 99)
          end)
        end
        return {
          devices = {
            {
              id = "fault",
              kind = "instrument",
              label = "Guard fault fixture",
              endpoints = { endpoint },
              identityPolicy = "slot",
              pollIntervalMs = mode ~= "cancellation" and 500 or nil,
              characteristics = {
                {
                  id = "number",
                  label = "Number",
                  spec = { kind = "number", minimum = 0, maximum = 100, step = 1, integer = true },
                  access = { read = true, write = true, notify = true },
                  writes = { ddc = { { code = 16, changeMask = 255 } } },
                },
              },
              onRefresh = function(ctx)
                calls = calls + 1
                local reading, error = ctx.capabilities.ddc:get_vcp(endpoint, 16)
                if not reading then
                  return { error = error }
                end
                if reading.current == 66 then
                  if mode == "exception" then
                    error("private fixture secret must never escape")
                  elseif mode == "caught-budget" then
                    pcall(function()
                      local sum = 0
                      for i = 1, 2000000 do
                        sum = sum + i
                      end
                    end)
                  elseif mode == "memory" then
                    local large = string.rep("x", 32 * 1024 * 1024)
                  elseif mode == "malformed" then
                    return { updates = { { id = "number", status = "ok", value = "not a number" } } }
                  elseif mode == "cycle" then
                    local updates = {}
                    updates[1] = updates
                    return { updates = updates }
                  end
                end
                return { updates = { { id = "number", status = "ok", value = reading.current } } }
              end,
              onWrite = function(ctx, id, command)
                local reading, err = ctx.capabilities.ddc:get_vcp(endpoint, 16)
                if not reading then
                  return { error = err }
                end
                local value = command.op == "step" and math.max(0, math.min(100, reading.current + command.steps))
                  or command.value
                if mode == "wrong-register" then
                  ctx.capabilities.ddc:get_vcp(endpoint, 33)
                  ctx.capabilities.ddc:set_vcp(endpoint, 33, value)
                elseif mode == "out-of-mask" then
                  ctx.capabilities.ddc:set_vcp(endpoint, 16, 256 + value)
                elseif mode == "publish-write" then
                  pcall(function()
                    ctx.publish({ { id = "number", status = "ok", value = value } })
                  end)
                else
                  local _, err = ctx.capabilities.ddc:set_vcp(endpoint, 16, value)
                  if err then
                    return { error = err }
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
