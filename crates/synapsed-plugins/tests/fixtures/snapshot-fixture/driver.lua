return {
  configure = function(settings)
    for key in pairs(settings) do
      if key ~= "initialLength" and key ~= "failureLength" then
        error("unknown fixture setting")
      end
    end
    local initial_length = settings.initialLength or 900
    local failure_length = settings.failureLength or 1024
    for _, length in ipairs({ initial_length, failure_length }) do
      if type(length) ~= "number" or length < 1 or length > 1024 or length ~= math.floor(length) then
        error("invalid fixture length")
      end
    end
    return {
      discover = function(ctx)
        local inventory, err = ctx.capabilities.ddc:enumerate()
        if not inventory then
          return { error = err }
        end
        local devices = {}
        for index, endpoint in ipairs(inventory.endpoints) do
          local characteristics = {}
          for sample = 1, 64 do
            characteristics[#characteristics + 1] = {
              id = "sample_" .. sample,
              label = "Sample " .. sample,
              spec = { kind = "string", maxLength = 1024 },
              access = { read = true, write = false, notify = true },
              writes = {},
            }
          end
          devices[#devices + 1] = {
            id = "large_" .. index,
            kind = "instrument",
            label = "Large observation fixture",
            endpoints = { endpoint },
            identityPolicy = "slot",
            pollIntervalMs = 500,
            characteristics = characteristics,
            onRefresh = function(ctx)
              local reading, err = ctx.capabilities.ddc:get_vcp(endpoint, 16)
              if not reading then
                return { error = err }
              end
              local value = string.rep("𐐀", reading.current == 66 and failure_length or initial_length)
              local updates = {}
              for sample = 1, 64 do
                updates[#updates + 1] = { id = "sample_" .. sample, status = "ok", value = value }
              end
              return { updates = updates }
            end,
          }
        end
        return { devices = devices }
      end,
    }
  end,
}
