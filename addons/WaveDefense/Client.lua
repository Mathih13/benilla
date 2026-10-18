local protocol = WaveDefense

function protocol.NewClient(send, save)
    local client = {
        send = send, save = save, request = "0", sequence = "0", online = false, synced = false,
        visible = true, notice = "Connecting to Wave Defense...", nextStatus = 0, nextRoster = 0
    }

    function client:Connect(now, saved)
        self.online, self.synced, self.inflight = true, false, nil
        self.status, self.offer, self.roster = nil, nil, nil
        self.request = protocol.Decimal(saved) or "0"
        self.lastStatusSent = nil
        self.nextStatus, self.nextRoster, self.offerRetry = now, now, now
        self.notice = "Connecting to Wave Defense..."
    end

    function client:Disconnect()
        self.online, self.synced, self.inflight, self.offer = false, false, nil, nil
        self.notice = "Connection closed. Log in again to refresh your match."
    end

    function client:Send(action, payload, now)
        self.sequence = protocol.Increment(self.sequence) or "1"
        self.inflight = { action = action, sent = now }
        if action == "status" then self.nextStatus, self.lastStatusSent = now + 2, now end
        if action == "roster" then self.nextRoster = now + 4 end
        if action == "offer" then self.offerRetry = now + 4 end
        self.send(protocol.Request(action, self.sequence, payload))
    end

    function client:Refresh(now)
        self.nextStatus, self.nextRoster = math.min(self.nextStatus, now), now
    end

    function client:CanAct(now)
        return self.online and self.synced and not self.inflight and self.status and now - self.received < 6
    end

    function client:Act(action, arguments, now)
        if not self:CanAct(now) then return false end
        local status = self.status
        if action == "select" or action == "queue" then
            if status.phase ~= "lobby" then return false end
        elseif action == "ready" then
            if status.phase ~= "preparation" or status.isReady == 1
                or arguments[1] ~= status.match or arguments[2] ~= tostring(status.wave) then return false end
        elseif action == "choose" then
            if status.phase ~= "preparation" or not self.offer then return false end
            if arguments[1] ~= self.offer.match or arguments[2] ~= self.offer.revision then return false end
            local found = false
            for _, choice in ipairs(self.offer.choices) do
                if choice.id == arguments[3] then found = true end
            end
            if not found then return false end
        elseif action == "leave" then
            if status.phase == "lobby" then return false end
        else
            return false
        end
        local request = protocol.Increment(self.request)
        if not request then
            self.notice = "Request counter is exhausted. Contact the realm Operator."
            return false
        end
        self.request = request
        self.save(request)
        self.notice = "Waiting for the realm..."
        self.synced = false
        local payload = request
        if table.getn(arguments) > 0 then payload = payload .. "|" .. table.concat(arguments, "|") end
        self:Send(action, payload, now)
        return true
    end

    function client:Receive(prefix, message, distribution, sender, selfName, now)
        -- Private Gateway replies use the Actor's GUID; Benilla calls the whisper lane UNKNOWN.
        if prefix ~= "STC" or not selfName or selfName == "" or sender ~= selfName
            or distribution ~= "UNKNOWN" and distribution ~= "WHISPER" then return false end
        local action, payload = protocol.Reply(message)
        if not action or not self.inflight or self.inflight.action ~= action then return false end
        if string.sub(payload, 1, 8) == "Refused|" then
            self.inflight = nil
            self.notice = string.sub(payload, 9)
            self.synced = false
            self.offer = nil
            if action == "status" then
                self.nextStatus = now + 10
            else
                self.nextStatus = now
            end
            return true
        end
        if action == "offer" then
            local offer = protocol.Offer(payload)
            if not offer then return false end
            self.inflight = nil
            if self.status and self.status.phase == "preparation"
                and offer.match == self.status.match and offer.revision == self.status.revision then
                self.offer = offer
            end
            return true
        end
        if action == "roster" then
            local roster = protocol.Roster(payload)
            if not roster then return false end
            self.inflight, self.roster = nil, roster
            return true
        end
        local status = protocol.Status(payload)
        if not status then return false end
        local previous = self.status
        if action ~= "status" and protocol.Greater(self.request, status.lastRequest) then return false end
        if previous and protocol.Greater(previous.lastRequest, status.lastRequest) then return false end
        if not previous or previous.match ~= status.match or previous.phase ~= status.phase
            or previous.revision ~= status.revision or status.pending == 0 then
            self.offer, self.offerRetry = nil, now
        end
        if not previous or previous.match ~= status.match or previous.phase ~= status.phase then
            self.roster, self.nextRoster = nil, now
            self.banked = false
            if status.phase == "lobby" or status.phase == "queued" then self.visible = true end
        end
        self.inflight, self.status, self.received, self.synced = nil, status, now, true
        if action ~= "status" then self.nextStatus = now + 2 end
        if protocol.Greater(status.lastRequest, self.request) then
            self.request = status.lastRequest
            self.save(self.request)
        end
        if self.notice == "Connecting to Wave Defense..." or self.notice == "Waiting for the realm..."
            or self.notice == "No reply from the realm. Refreshing status..." then self.notice = "" end
        return true
    end

    function client:Tick(now)
        if not self.online then return end
        if self.inflight then
            if now - self.inflight.sent < 6 then return end
            self.inflight, self.synced, self.offer = nil, false, nil
            self.notice = "No reply from the realm. Refreshing status..."
            self.nextStatus = now
        end
        if now >= self.nextStatus and (not self.lastStatusSent or now - self.lastStatusSent >= 2) then
            self:Send("status", "", now)
            return
        end
        if not self.synced or not self.status then return end
        local status = self.status
        if status.phase == "preparation" and status.pending > 0 and not self.offer and now >= self.offerRetry then
            self:Send("offer", "", now)
        elseif self.visible and (status.phase == "lobby" or status.phase == "queued") and now >= self.nextRoster then
            self:Send("roster", "", now)
        end
    end

    return client
end
