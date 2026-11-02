local monitor = require("monitor")
local features = require("features")
local probed = {}

local function find_entry(entries, id)
  for _, entry in ipairs(entries) do
    if entry.feature.id == id then
      return entry
    end
  end
  return nil
end

local function updates(ctx, endpoint, entries)
  local result = {}
  for _, entry in ipairs(entries) do
    result[#result + 1] = features.observe(ctx, endpoint, entry)
  end
  return result
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
        local entries = {
          { feature = features.BRIGHTNESS, maximum = candidate.brightness.maximum },
        }
        local identity = endpoint.identity
        local identified = type(identity) == "string" and identity ~= ""
        local optional = identified and probed[identity] or nil
        if not optional then
          optional = {}
          for _, feature in ipairs(features.OPTIONAL) do
            local entry, probe_error = features.probe(ctx, endpoint, feature)
            if probe_error then
              return { error = probe_error }
            end
            if entry then
              optional[#optional + 1] = entry
            end
          end
          if identified then
            probed[identity] = optional
          end
        end
        for _, entry in ipairs(optional) do
          entries[#entries + 1] = entry
        end

        local characteristics = {}
        local used_groups = {}
        for _, entry in ipairs(entries) do
          characteristics[#characteristics + 1] = features.definition(entry)
          used_groups[entry.feature.group] = true
        end
        local groups = {}
        for _, group in ipairs(features.GROUPS) do
          if used_groups[group.id] then
            groups[#groups + 1] = group
          end
        end

        return {
          devices = {
            {
              id = "monitor",
              kind = "monitor",
              label = "Monitor",
              endpoints = { endpoint },
              identityPolicy = "slot",
              groups = groups,
              characteristics = characteristics,
              pollIntervalMs = 2000,
              onRefresh = function(refresh_ctx)
                if type(refresh_ctx.confirm) == "string" then
                  local confirmed = find_entry(entries, refresh_ctx.confirm)
                  if confirmed then
                    return { updates = { features.observe(refresh_ctx, endpoint, confirmed) } }
                  end
                end
                return { updates = updates(refresh_ctx, endpoint, entries) }
              end,
              onWrite = function(write_ctx, characteristic_id, command)
                local entry = find_entry(entries, characteristic_id)
                if not entry then
                  return {
                    error = {
                      code = "unknown_characteristic",
                      message = "The characteristic is not declared by this driver.",
                    },
                  }
                end
                return features.write(write_ctx, endpoint, entry, command)
              end,
            },
          },
        }
      end,
    }
  end,
}
