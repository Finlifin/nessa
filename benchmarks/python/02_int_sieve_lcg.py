import time


def run_sieve(limit: int) -> int:
    flags = [True] * (limit + 1)
    flags[0] = False
    flags[1] = False
    p = 2
    while p * p <= limit:
        if flags[p]:
            k = p * p
            while k <= limit:
                flags[k] = False
                k += p
        p += 1
    count = 0
    i = 2
    while i <= limit:
        if flags[i]:
            count += 1
        i += 1
    return count


def run_lcg(steps: int, seed: int) -> int:
    state = seed
    acc = 0
    for _ in range(steps):
        state = (state * 48271 + 12345) % 1000000007
        acc = (acc + (state % 65536)) % 1000000007
    return acc


def main() -> None:
    t0 = time.perf_counter_ns()
    prime_sum = 0
    for _ in range(40):
        prime_sum += run_sieve(50000)
    lcg_acc = run_lcg(2000000, 42)
    checksum = prime_sum * 1000 + (lcg_acc % 1000)
    ms = (time.perf_counter_ns() - t0) / 1_000_000.0
    print(f"KERNEL_MS:{ms:.6f}")
    print(f"CHECKSUM:{checksum}")


if __name__ == "__main__":
    main()
