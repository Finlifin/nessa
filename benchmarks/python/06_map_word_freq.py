import time


def run_map_bench(num_keys: int, rounds: int) -> int:
    keys = [f"word_key_{i}" for i in range(num_keys)]
    mp: dict[str, int] = {}
    for r in range(rounds):
        delta = 1 + (r % 7)
        for idx in range(num_keys):
            k = keys[idx]
            prev = mp.get(k)
            mp[k] = delta if prev is None else prev + delta
    total = len(mp)
    for j in range(num_keys):
        total += mp[keys[j]] * ((j % 11) + 1)
    return total


def main() -> None:
    t0 = time.perf_counter_ns()
    ans = run_map_bench(1500, 100)
    ms = (time.perf_counter_ns() - t0) / 1_000_000.0
    print(f"KERNEL_MS:{ms:.6f}")
    print(f"CHECKSUM:{ans}")


if __name__ == "__main__":
    main()
