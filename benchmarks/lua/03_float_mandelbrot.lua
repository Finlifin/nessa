local function mandelbrot(size, max_iter)
    local total_iters = 0
    for y = 0, size - 1 do
        local ci = (2.0 * y) / size - 1.0
        for x = 0, size - 1 do
            local cr = (2.0 * x) / size - 1.5
            local zr = 0.0
            local zi = 0.0
            local iter = 0
            while iter < max_iter do
                local zr2 = zr * zr
                local zi2 = zi * zi
                if zr2 + zi2 > 4.0 then
                    break
                end
                zi = 2.0 * zr * zi + ci
                zr = zr2 - zi2 + cr
                iter = iter + 1
            end
            total_iters = total_iters + iter
        end
    end
    return total_iters
end

local t0 = os.clock()
local ans = mandelbrot(400, 100)
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%.0f", ans))
