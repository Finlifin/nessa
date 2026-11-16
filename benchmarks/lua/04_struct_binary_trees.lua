local function make_tree(item, depth)
    if depth <= 0 then
        return { item = item, left = nil, right = nil }
    end
    local next_depth = depth - 1
    return {
        item = item,
        left = make_tree(2 * item - 1, next_depth),
        right = make_tree(2 * item, next_depth),
    }
end

local function check_tree(node)
    if node.left == nil then
        return node.item
    end
    return node.item + check_tree(node.left) - check_tree(node.right)
end

local function pow2(n)
    local res = 1
    for _ = 1, n do
        res = res * 2
    end
    return res
end

local function run_trees()
    local min_depth = 4
    local max_depth = 13
    local stretch = make_tree(0, max_depth + 1)
    local total_check = check_tree(stretch)
    local long_lived = make_tree(0, max_depth)

    local depth = min_depth
    while depth <= max_depth do
        local iterations = pow2(max_depth - depth + min_depth)
        local chk = 0
        for i = 1, iterations do
            chk = chk + check_tree(make_tree(i, depth))
            chk = chk + check_tree(make_tree(-i, depth))
        end
        total_check = total_check + chk
        depth = depth + 2
    end
    return total_check + check_tree(long_lived)
end

local t0 = os.clock()
local ans = run_trees()
local ms = (os.clock() - t0) * 1000.0
print(string.format("KERNEL_MS:%.6f", ms))
print(string.format("CHECKSUM:%.0f", ans))
