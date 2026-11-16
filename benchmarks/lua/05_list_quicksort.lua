local floor = math.floor

local function quicksort(arr, lo, hi)
    if lo >= hi then
        return
    end
    local mid = lo + floor((hi - lo) / 2)
    local pivot = arr[mid + 1]
    local i = lo
    local j = hi
    while i <= j do
        while arr[i + 1] < pivot do
            i = i + 1
        end
        while arr[j + 1] > pivot do
            j = j - 1
        end
        if i <= j then
            local tmp = arr[i + 1]
            arr[i + 1] = arr[j + 1]
            arr[j + 1] = tmp
            i = i + 1
            j = j - 1
        end
    end
    if lo < j then
        quicksort(arr, lo, j)
    end
    if i < hi then
        quicksort(arr, i, hi)
    end
end

local function run_quicksort_bench(n, rounds)
    local seed = 1234567
    local checksum = 0
    local half = floor(n / 2)
    for _ = 1, rounds do
        local arr = {}
        for k = 1, n do
            seed = (seed * 48271 + 12345) % 1000000007
            arr[k] = seed % 1000000
        end
        quicksort(arr, 0, n - 1)
        checksum = checksum + arr[1]
        checksum = checksum + arr[half + 1]
        checksum = checksum + arr[n]
    end
    return checksum
end

local t0 = os.clock()
local ans = run_quicksort_bench(20000, 8)
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%.0f", ans))
