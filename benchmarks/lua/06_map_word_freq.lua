local function run_map_bench(num_keys, rounds)
    local keys = {}
    for i = 0, num_keys - 1 do
        keys[i + 1] = "word_key_" .. tostring(i)
    end
    local mp = {}
    local mp_len = 0
    for r = 0, rounds - 1 do
        local delta = 1 + (r % 7)
        for idx = 1, num_keys do
            local k = keys[idx]
            local prev = mp[k]
            if prev == nil then
                mp_len = mp_len + 1
                mp[k] = delta
            else
                mp[k] = prev + delta
            end
        end
    end
    local sum = mp_len
    for j = 0, num_keys - 1 do
        sum = sum + mp[keys[j + 1]] * ((j % 11) + 1)
    end
    return sum
end

local t0 = os.clock()
local ans = run_map_bench(1500, 100)
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%.0f", ans))
