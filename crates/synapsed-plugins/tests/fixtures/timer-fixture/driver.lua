return {
  configure = function(settings)
    if next(settings) ~= nil then
      error("unknown fixture setting")
    end
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
        local function definition(id)
          return {
            id = id,
            label = id,
            spec = { kind = "number", minimum = 0, maximum = 1000, step = 1, integer = true },
            access = { read = true, write = false, notify = true },
            writes = {},
          }
        end
        local function sample(ctx, id, code)
          local reading, err = ctx.capabilities.ddc:get_vcp(endpoint, code)
          if not reading then
            return nil, err
          end
          return { id = id, status = "ok", value = reading.current }, nil
        end
        return {
          devices = {
            {
              id = "timed",
              kind = "instrument",
              label = "Partial timer observation fixture",
              endpoints = { endpoint },
              identityPolicy = "slot",
              characteristics = { definition("a"), definition("b") },
              onStart = function(ctx)
                ctx.every(500, function(ctx)
                  local update, err = sample(ctx, "a", 16)
                  if err then
                    return { error = err }
                  end
                  ctx.publish({ update })
                end)
                ctx.every(500, function(ctx)
                  local update, err = sample(ctx, "b", 32)
                  if err then
                    return { error = err }
                  end
                  ctx.publish({ update })
                end)
              end,
              onRefresh = function(ctx)
                local a, err = sample(ctx, "a", 16)
                if err then
                  return { error = err }
                end
                local b, err = sample(ctx, "b", 32)
                if err then
                  return { error = err }
                end
                return { updates = { a, b } }
              end,
            },
          },
        }
      end,
    }
  end,
}
