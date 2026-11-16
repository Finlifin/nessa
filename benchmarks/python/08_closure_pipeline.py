import time


def map_i64(src: list[int], f) -> list[int]:
    out = []
    for v in src:
        out.append(f(v))
    return out


def filter_i64(src: list[int], pred) -> list[int]:
    out = []
    for v in src:
        if pred(v):
            out.append(v)
    return out


def fold_i64(src: list[int], init: int, step) -> int:
    acc = init
    for v in src:
        acc = step(acc, v)
    return acc


def run_closure_bench(size: int, rounds: int) -> int:
    base = [(i * 17 + 5) % 1000 for i in range(size)]
    grand_total = 0
    for r in range(rounds):
        offset = r * 3 + 7
        scale = (r % 5) + 2
        mapped = map_i64(base, lambda x: x * scale + offset)
        filtered = filter_i64(mapped, lambda x: x % 3 != 0)
        folded = fold_i64(filtered, 0, lambda acc, x: (acc + x + offset) % 1000000007)
        grand_total = (grand_total + folded) % 1000000007
    return grand_total


def main() -> None:
    t0 = time.perf_counter_ns()
    ans = run_closure_bench(10000, 25)
    ms = (time.perf_counter_ns() - t0) / 1_000_000.0
    print(f"KERNEL_MS:{ms:.6f}")
    print(f"CHECKSUM:{ans}")


if __name__ == "__main__":
    main()
