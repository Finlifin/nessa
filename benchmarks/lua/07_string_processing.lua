local find = string.find
local sub = string.sub
local tonumber = tonumber
local tostring = tostring

local function run_string_bench(lines)
    local total_latency = 0
    local total_bytes = 0
    local matched_ok = 0
    for i = 0, lines - 1 do
        local lat = (i * 37 + 13) % 997
        local code = (i % 5 == 0) and "500" or "200"
        local line = "2026-11-17T12:00:00Z service=gateway req="
            .. tostring(i)
            .. " latency="
            .. tostring(lat)
            .. "ms status="
            .. code
        local n = #line
        total_bytes = total_bytes + n
        if n >= 10 and sub(line, n - 9, n) == "status=200" then
            matched_ok = matched_ok + 1
        end
        local pos = find(line, "latency=", 1, true)
        if pos ~= nil then
            local start_idx = pos + 8
            local end_pos = find(line, "ms status=", 1, true)
            if end_pos ~= nil then
                local parsed = tonumber(sub(line, start_idx, end_pos - 1))
                if parsed ~= nil then
                    total_latency = total_latency + parsed
                end
            end
        end
    end
    return total_latency * 100 + matched_ok + (total_bytes % 100000)
end

local t0 = os.clock()
local ans = run_string_bench(25000)
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%.0f", ans))
