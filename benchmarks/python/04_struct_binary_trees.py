import time


class TreeNode:
    __slots__ = ("item", "left", "right")

    def __init__(self, item: int, left, right) -> None:
        self.item = item
        self.left = left
        self.right = right


def make_tree(item: int, depth: int) -> TreeNode:
    if depth <= 0:
        return TreeNode(item, None, None)
    next_depth = depth - 1
    return TreeNode(
        item,
        make_tree(2 * item - 1, next_depth),
        make_tree(2 * item, next_depth),
    )


def check_tree(node: TreeNode) -> int:
    if node.left is None:
        return node.item
    return node.item + check_tree(node.left) - check_tree(node.right)


def run_trees() -> int:
    min_depth = 4
    max_depth = 13
    stretch = make_tree(0, max_depth + 1)
    total_check = check_tree(stretch)
    long_lived = make_tree(0, max_depth)

    depth = min_depth
    while depth <= max_depth:
        iterations = 1 << (max_depth - depth + min_depth)
        chk = 0
        for i in range(1, iterations + 1):
            chk += check_tree(make_tree(i, depth))
            chk += check_tree(make_tree(-i, depth))
        total_check += chk
        depth += 2

    return total_check + check_tree(long_lived)


def main() -> None:
    t0 = time.perf_counter_ns()
    ans = run_trees()
    ms = (time.perf_counter_ns() - t0) / 1_000_000.0
    print(f"KERNEL_MS:{ms:.6f}")
    print(f"CHECKSUM:{ans}")


if __name__ == "__main__":
    main()
