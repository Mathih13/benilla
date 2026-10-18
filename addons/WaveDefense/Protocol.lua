WaveDefense = {}
local protocol = WaveDefense
local maxRequest = "18446744073709551615"
local phases = { lobby = true, queued = true, preparation = true, combat = true, won = true, lost = true }
local roles = { tank = true, healer = true, damage = true }
local actions = {
    status = true, offer = true, roster = true, select = true, queue = true, ready = true, choose = true, leave = true
}

function protocol.Decimal(value)
    if type(value) ~= "string" or not string.find(value, "^%d+$") then return nil end
    if string.len(value) > 1 and string.sub(value, 1, 1) == "0" then return nil end
    if string.len(value) > 20 or string.len(value) == 20 and value > maxRequest then return nil end
    return value
end

function protocol.Greater(left, right)
    if string.len(left) ~= string.len(right) then return string.len(left) > string.len(right) end
    return left > right
end

function protocol.Increment(value)
    if not protocol.Decimal(value) or value == maxRequest then return nil end
    local result, carry = "", 1
    for index = string.len(value), 1, -1 do
        local digit = tonumber(string.sub(value, index, index)) + carry
        if digit == 10 then digit = 0 else carry = 0 end
        result = tostring(digit) .. result
    end
    if carry == 1 then result = "1" .. result end
    return result
end

local function fields(payload)
    if type(payload) ~= "string" or string.len(payload) > 1024 or string.find(payload, "%c") then return nil end
    local result, first = {}, 1
    while true do
        local separator = string.find(payload, "|", first, true)
        if not separator then
            table.insert(result, string.sub(payload, first))
            return result
        end
        table.insert(result, string.sub(payload, first, separator - 1))
        if table.getn(result) > 64 then return nil end
        first = separator + 1
    end
end

local function number(value, maximum)
    if not protocol.Decimal(value) or string.len(value) > 10 then return nil end
    local result = tonumber(value)
    if result > maximum then return nil end
    return result
end

function protocol.Status(payload)
    local parts = fields(payload)
    if not parts or table.getn(parts) ~= 19 or parts[1] ~= "v1" or not phases[parts[2]] then return nil end
    local status = { phase = parts[2], match = protocol.Decimal(parts[3]), lastRequest = protocol.Decimal(parts[18]) }
    local names = {
        "wave", "level", "xp", "nextXP", "baseHP", "baseMax", "enemies", "seconds", "gold", "pending",
        "readyCount", "isReady", "class", "queueSeconds"
    }
    local limits = {
        15, 60, 4294967295, 4294967295, 4294967295, 4294967295, 100000, 86400, 4294967295,
        13, 5, 1, 11, 4294967295
    }
    if not status.match or not status.lastRequest then return nil end
    for index = 1, table.getn(names) do
        local value = number(parts[index + 3], limits[index])
        if not value then return nil end
        status[names[index]] = value
    end
    status.revision = protocol.Decimal(parts[19])
    if not status.revision or status.baseHP > status.baseMax then return nil end
    return status
end

function protocol.Offer(payload)
    local parts = fields(payload)
    if not parts or parts[1] ~= "v1" then return nil end
    local count = table.getn(parts)
    if count < 5 or count > 17 or math.mod(count - 5, 4) ~= 0 then return nil end
    local offer = {
        match = protocol.Decimal(parts[2]), revision = protocol.Decimal(parts[3]),
        milestone = number(parts[4], 60), pending = number(parts[5], 13), choices = {}
    }
    if not offer.match or not offer.revision or not offer.milestone or not offer.pending then return nil end
    local seen = {}
    for index = 6, count, 4 do
        local id, kind = protocol.Decimal(parts[index]), parts[index + 1]
        if not id or id == "0" or seen[id] or kind ~= "spell" and kind ~= "talent"
            or parts[index + 2] == "" then return nil end
        seen[id] = true
        table.insert(offer.choices, { id = id, kind = kind, name = parts[index + 2], description = parts[index + 3] })
    end
    return offer
end

function protocol.Roster(payload)
    local parts = fields(payload)
    if not parts or parts[1] ~= "v1" then return nil end
    local count, roster, seen = table.getn(parts), {}, {}
    if count > 26 or math.mod(count - 1, 5) ~= 0 then return nil end
    for index = 2, count, 5 do
        local guid, role = protocol.Decimal(parts[index]), parts[index + 2]
        if not guid or guid == "0" or seen[guid] or not roles[role] or parts[index + 1] == "" then return nil end
        if parts[index + 3] ~= "0" and parts[index + 3] ~= "1"
            or parts[index + 4] ~= "0" and parts[index + 4] ~= "1" then return nil end
        seen[guid] = true
        table.insert(roster, {
            guid = guid, name = parts[index + 1], role = role,
            bot = parts[index + 3] == "1", ready = parts[index + 4] == "1"
        })
    end
    return roster
end

function protocol.Reply(message)
    if type(message) ~= "string" or string.len(message) > 1130 or string.find(message, "%c") then return nil end
    local _, _, action, sequence, payload = string.find(
        message, "^v1|package%.wave_defense%.([a-z]+)%.result|(%d+)|1/1|(.*)$"
    )
    if not action or not actions[action] or not protocol.Decimal(sequence)
        or string.len(payload) > 1024 then return nil end
    return action, payload
end

function protocol.Request(action, sequence, payload)
    local envelope = "v1|package.wave_defense." .. action .. "|" .. sequence .. "|1/1|" .. payload
    -- The 1.12 chat API requires escaped pipes; the Gateway decodes them once.
    return string.gsub(envelope, "|", "||")
end
