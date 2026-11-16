local function fib(n)
    if n < 2 then
        return n
    end
    return fib(n - 1) + fib(n - 2)
end

local t0 = os.clock()
local ans = fib(34)
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%d", ans))
