local function map_i64(src, f)
    local out = {}
    local n = #src
    for i = 1, n do
        out[i] = f(src[i])
    end
    return out
end

local function filter_i64(src, pred)
    local out = {}
    local n = #src
    local k = 0
    for i = 1, n do
        local v = src[i]
        if pred(v) then
            k = k + 1
            out[k] = v
        end
    end
    return out
end

local function fold_i64(src, init, step)
    local acc = init
    local n = #src
    for i = 1, n do
        acc = step(acc, src[i])
    end
    return acc
end

local function run_closure_bench(size, rounds)
    local base = {}
    for i = 0, size - 1 do
        base[i + 1] = (i * 17 + 5) % 1000
    end
    local grand_total = 0
    for r = 0, rounds - 1 do
        local offset = r * 3 + 7
        local scale = (r % 5) + 2
        local mapped = map_i64(base, function(x)
            return x * scale + offset
        end)
        local filtered = filter_i64(mapped, function(x)
            return x % 3 ~= 0
        end)
        local folded = fold_i64(filtered, 0, function(acc, x)
            return (acc + x + offset) % 1000000007
        end)
        grand_total = (grand_total + folded) % 1000000007
    end
    return grand_total
end

local t0 = os.clock()
local ans = run_closure_bench(10000, 25)
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%.0f", ans))
