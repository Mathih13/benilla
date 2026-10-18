local protocol = WaveDefense
local client = protocol.NewClient(function(message)
    SendAddonMessage("STC", message, "PARTY")
end, function(request)
    WaveDefenseRequest = request
end)
local specs = {
    WARRIOR = { "1", "Protection Warrior", "Tank" },
    PALADIN = { "2", "Retribution Paladin", "Damage" },
    HUNTER = { "3", "Beast Mastery Hunter", "Damage" },
    ROGUE = { "4", "Combat Rogue", "Damage" },
    PRIEST = { "5", "Holy Priest", "Healer" },
    SHAMAN = { "7", "Restoration Shaman", "Healer" },
    MAGE = { "8", "Frost Mage", "Damage" },
    WARLOCK = { "9", "Affliction Warlock", "Damage" },
    DRUID = { "11", "Feral Druid", "Tank" }
}
local classTokens = {
    [1] = "WARRIOR", [2] = "PALADIN", [3] = "HUNTER", [4] = "ROGUE", [5] = "PRIEST",
    [7] = "SHAMAN", [8] = "MAGE", [9] = "WARLOCK", [11] = "DRUID"
}
local leaveUntil = 0

local function spec()
    local _, token = UnitClass("player")
    return specs[token]
end

local function panel(name, width, height, anchor, relative, x, y)
    local frame = CreateFrame("Frame", name, UIParent)
    frame:SetWidth(width)
    frame:SetHeight(height)
    frame:SetPoint(anchor, UIParent, relative, x, y)
    frame:SetBackdrop({
        bgFile = "Interface\\Tooltips\\UI-Tooltip-Background", edgeFile = "Interface\\Tooltips\\UI-Tooltip-Border",
        tile = true, tileSize = 16, edgeSize = 16, insets = { left = 4, right = 4, top = 4, bottom = 4 }
    })
    frame:SetBackdropColor(0.06, 0.07, 0.06, 0.96)
    frame:SetBackdropBorderColor(0.55, 0.47, 0.31, 1)
    frame:SetFrameStrata("HIGH")
    local available = UIParent:GetWidth() - 24
    if available < width then frame:SetScale(available / width) end
    frame:Hide()
    return frame
end

local function label(parent, x, y, width, height, font)
    local text = parent:CreateFontString(nil, "OVERLAY", font or "GameFontHighlightSmall")
    text:SetPoint("TOPLEFT", parent, "TOPLEFT", x, y)
    text:SetWidth(width)
    text:SetHeight(height)
    text:SetJustifyH("LEFT")
    text:SetJustifyV("TOP")
    return text
end

local function button(parent, text, x, y, width, callback)
    local control = CreateFrame("Button", nil, parent, "UIPanelButtonTemplate")
    control:SetWidth(width)
    control:SetHeight(24)
    control:SetPoint("TOPLEFT", parent, "TOPLEFT", x, y)
    control:SetText(text)
    control:SetScript("OnClick", callback)
    return control
end

local function enabled(control, value)
    if value then control:Enable() else control:Disable() end
end

local function shown(frame, value)
    if value then frame:Show() else frame:Hide() end
end

local function countdown(seconds)
    local minutes = math.floor(seconds / 60)
    return string.format("%d:%02d", minutes, math.mod(seconds, 60))
end

local function money(copper)
    local gold = math.floor(copper / 10000)
    local silver = math.floor(math.mod(copper, 10000) / 100)
    return gold .. "g " .. silver .. "s " .. math.mod(copper, 100) .. "c"
end

local dock = panel("WaveDefenseDock", 400, 126, "TOP", "TOP", 0, -65)
local dockTitle = label(dock, 14, -12, 365, 20, "GameFontNormalLarge")
local dockStats = label(dock, 14, -38, 370, 19)
local baseBar = CreateFrame("StatusBar", nil, dock)
baseBar:SetPoint("TOPLEFT", dock, "TOPLEFT", 14, -60)
baseBar:SetWidth(372)
baseBar:SetHeight(9)
baseBar:SetStatusBarTexture("Interface\\TargetingFrame\\UI-StatusBar")
baseBar:SetStatusBarColor(0.36, 0.58, 0.27)
local dockDetail = label(dock, 14, -78, 230, 34)
local ready = button(dock, "Ready", 280, -83, 106, function()
    if client.status then client:Act("ready", { client.status.match, tostring(client.status.wave) }, GetTime()) end
end)

local reward = panel("WaveDefenseReward", 460, 342, "CENTER", "CENTER", 0, 0)
local rewardTitle = label(reward, 16, -16, 420, 24, "GameFontNormalLarge")
local rewardSpec = label(reward, 16, -46, 420, 20)
local choices = {}
for index = 1, 3 do
    local row = index
    local y = -76 - (index - 1) * 69
    local choice = {
        name = label(reward, 16, y, 312, 20, "GameFontNormal"),
        description = label(reward, 16, y - 22, 312, 43)
    }
    choice.choose = button(reward, "Choose", 342, y - 9, 100, function()
        local offer = client.offer
        if offer and offer.choices[row] then
            client:Act("choose", { offer.match, offer.revision, offer.choices[row].id }, GetTime())
        end
    end)
    table.insert(choices, choice)
end
local rewardPending = label(reward, 16, -291, 285, 34)
button(reward, "Bank for later", 314, -299, 128, function() client.banked = true end)

local lobby = panel("WaveDefenseLobby", 480, 433, "CENTER", "CENTER", 0, -30)
label(lobby, 16, -16, 420, 24, "GameFontNormalLarge"):SetText("Wave defense queue")
local lobbySpec = label(lobby, 16, -51, 448, 24, "GameFontNormal")
local lobbyPhase = label(lobby, 16, -81, 448, 36)
label(lobby, 16, -126, 448, 34):SetText(
    "Class uses normal character selection.\nEvery human must join the queue, including premade members."
)
local select = button(lobby, "Select specialization", 16, -169, 188, function()
    local current = spec()
    if current then client:Act("select", { current[1] }, GetTime()) end
end)
local queue = button(lobby, "Join queue", 220, -169, 116, function() client:Act("queue", {}, GetTime()) end)
local rosterRows = {}
for index = 1, 5 do
    table.insert(rosterRows, label(lobby, 16, -213 - (index - 1) * 23, 448, 22))
end
local lobbyNote = label(lobby, 16, -342, 448, 36)
button(lobby, "Close", 356, -391, 108, function() client.visible = false end)

local utility = panel("WaveDefenseControls", 460, 76, "BOTTOM", "BOTTOM", 0, 130)
local notice = label(utility, 14, -12, 310, 52)
local leave = button(utility, "Leave", 342, -24, 104, function()
    local now = GetTime()
    if now < leaveUntil then
        if client.status then client:Act("leave", { client.status.match }, now) end
        leaveUntil = 0
    else
        leaveUntil = now + 4
    end
end)

local function render(now)
    local status = client.status
    local active = status and status.match ~= "0" and status.phase ~= "lobby" and status.phase ~= "queued"
    local preparation = active and status.phase == "preparation"
    shown(dock, active)
    shown(lobby, client.visible and not active)
    shown(reward, preparation and client.offer and not client.banked)
    shown(utility, active or client.visible)
    local canAct = client:CanAct(now)
    enabled(ready, canAct and preparation and status.isReady == 0)
    enabled(select, canAct and status.phase == "lobby" and spec())
    enabled(queue, canAct and status.phase == "lobby" and spec())
    shown(leave, status and status.phase ~= "lobby")
    enabled(leave, canAct)
    leave:SetText(now < leaveUntil and "Confirm leave" or "Leave")
    local message = client.notice
    if now < leaveUntil then message = "Click Confirm leave within 4 seconds to leave." end
    if message == "" and status then
        if status.phase == "combat" then message = "Defend the base. Rewards bank until preparation."
        elseif status.phase == "preparation" then message = "Visit the Quartermaster for gear."
        elseif status.phase == "won" then message = "Victory. Leave to return to the lobby and reset your run."
        elseif status.phase == "lost" then message = "The base has fallen. Leave to return to the lobby."
        else message = "1 tank, 1 healer, 3 damage. Use /wd to open or close." end
    end
    notice:SetText(string.gsub(message, "|", " "))
    local current = spec()
    local selected = status and specs[classTokens[status.class]]
    lobbySpec:SetText(current and current[2] .. "   " .. current[3] or "Select a supported Character class.")
    if not status then
        lobbyPhase:SetText("Waiting for realm status...")
        lobbyNote:SetText("")
    else
        local elapsed = math.max(0, math.floor(now - client.received))
        if status.phase == "queued" then
            local queued = status.queueSeconds + elapsed
            local waiting = queued >= 60 and "Waiting for a complete team."
                or "Bot wait " .. countdown(60 - queued)
            lobbyPhase:SetText("Queued " .. countdown(queued) .. "   Bots fill missing roles after 1:00.\n" .. waiting)
        else
            lobbyPhase:SetText("Select your launch specialization, then join the queue.")
        end
        lobbyNote:SetText("Launch specialization: " .. (selected and selected[2] or "Not selected")
            .. "\nBots are labeled in the roster and Character names.")
        if active then
            local waveTitle = "Wave " .. status.wave .. " / 15"
            if status.wave > 0 and math.mod(status.wave, 5) == 0 then waveTitle = waveTitle .. " boss" end
            local phaseTitle = status.phase == "won" and "Victory" or status.phase == "lost" and "Defeat"
                or status.phase == "combat" and "Defend the base" or "Preparation"
            dockTitle:SetText(waveTitle .. "   " .. phaseTitle)
            dockStats:SetText("Base " .. status.baseHP .. " / " .. status.baseMax
                .. "   Level " .. status.level .. " / 60   Enemies " .. status.enemies)
            baseBar:SetMinMaxValues(0, math.max(1, status.baseMax))
            baseBar:SetValue(status.baseHP)
            local seconds = math.max(0, status.seconds - elapsed)
            local timing = preparation and "Next wave in " .. countdown(seconds)
                or status.seconds > 0 and "Respawn in " .. countdown(seconds)
                or "XP " .. status.xp .. " / " .. status.nextXP
            dockDetail:SetText(timing .. "   " .. status.readyCount .. " / 5 ready\n"
                .. money(status.gold) .. "   " .. status.pending .. " rewards banked")
            shown(ready, preparation)
            ready:SetText(status.isReady == 1 and "Ready" or "Ready early")
        end
    end
    for index = 1, 5 do
        local member = client.roster and client.roster[index]
        local consent = member and (member.bot and "Bot" or member.ready and "Ready" or "Not ready")
        rosterRows[index]:SetText(member and member.role .. "   " .. member.name .. "   " .. consent or "")
    end
    if preparation and client.offer then
        rewardTitle:SetText("Level " .. client.offer.milestone .. " reward")
        rewardSpec:SetText(selected and selected[2] or "Launch specialization")
        rewardPending:SetText(client.offer.pending .. " pending\nChoices can wait until the next preparation.")
        for index = 1, 3 do
            local choice, row = client.offer.choices[index], choices[index]
            row.name:SetText(choice and choice.name or "")
            row.description:SetText(choice and (choice.kind == "talent" and "Talent. " or "Spell. ")
                .. choice.description or "")
            shown(row.choose, choice)
            enabled(row.choose, choice and canAct)
        end
    end
end

SLASH_WAVEDEFENSE1 = "/wd"
SlashCmdList["WAVEDEFENSE"] = function()
    if client.status and client.status.match ~= "0" and client.status.phase ~= "queued" then
        client.banked = not client.banked
    else
        client.visible = not client.visible
    end
    client:Refresh(GetTime())
    render(GetTime())
end

local events = CreateFrame("Frame")
events:RegisterEvent("PLAYER_ENTERING_WORLD")
events:RegisterEvent("PLAYER_LEAVING_WORLD")
events:RegisterEvent("CHAT_MSG_ADDON")
events:RegisterEvent("PARTY_MEMBERS_CHANGED")
events:SetScript("OnEvent", function()
    local now = GetTime()
    if event == "PLAYER_ENTERING_WORLD" then
        client:Connect(now, WaveDefenseRequest)
    elseif event == "PLAYER_LEAVING_WORLD" then
        client:Disconnect()
    elseif event == "PARTY_MEMBERS_CHANGED" then
        client.nextRoster = now
    elseif event == "CHAT_MSG_ADDON" then
        if client:Receive(arg1, arg2, arg3, arg4, UnitName("player"), now)
            and client.synced and InCinematic() then StopCinematic() end
    end
    render(now)
end)
local nextRender = 0
events:SetScript("OnUpdate", function()
    local now = GetTime()
    if now < nextRender then return end
    nextRender = now + 0.2
    client:Tick(now)
    render(now)
end)
