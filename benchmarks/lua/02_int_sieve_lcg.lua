local function run_sieve(limit)
    local flags = {}
    for i = 0, limit do
        flags[i + 1] = true
    end
    flags[1] = false
    flags[2] = false
    local p = 2
    while p * p <= limit do
        if flags[p + 1] then
            local k = p * p
            while k <= limit do
                flags[k + 1] = false
                k = k + p
            end
        end
        p = p + 1
    end
    local count = 0
    for i = 2, limit do
        if flags[i + 1] then
            count = count + 1
        end
    end
    return count
end

local function run_lcg(steps, seed)
    local state = seed
    local acc = 0
    for _ = 1, steps do
        state = (state * 48271 + 12345) % 1000000007
        acc = (acc + (state % 65536)) % 1000000007
    end
    return acc
end

local t0 = os.clock()
local prime_sum = 0
for _ = 1, 40 do
    prime_sum = prime_sum + run_sieve(50000)
end
local lcg_acc = run_lcg(2000000, 42)
local checksum = prime_sum * 1000 + (lcg_acc % 1000)
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%.0f", checksum))
