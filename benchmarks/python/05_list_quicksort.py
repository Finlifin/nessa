import sys
import time

sys.setrecursionlimit(50000)


def quicksort(arr: list[int], lo: int, hi: int) -> None:
    if lo >= hi:
        return
    mid = lo + (hi - lo) // 2
    pivot = arr[mid]
    i = lo
    j = hi
    while i <= j:
        while arr[i] < pivot:
            i += 1
        while arr[j] > pivot:
            j -= 1
        if i <= j:
            arr[i], arr[j] = arr[j], arr[i]
            i += 1
            j -= 1
    if lo < j:
        quicksort(arr, lo, j)
    if i < hi:
        quicksort(arr, i, hi)


def run_quicksort_bench(n: int, rounds: int) -> int:
    seed = 1234567
    checksum = 0
    for _ in range(rounds):
        arr = []
        for _ in range(n):
            seed = (seed * 48271 + 12345) % 1000000007
            arr.append(seed % 1000000)
        quicksort(arr, 0, n - 1)
        checksum += arr[0]
        checksum += arr[n // 2]
        checksum += arr[n - 1]
    return checksum


def main() -> None:
    t0 = time.perf_counter_ns()
    ans = run_quicksort_bench(20000, 8)
    ms = (time.perf_counter_ns() - t0) / 1_000_000.0
    print(f"KERNEL_MS:{ms:.6f}")
    print(f"CHECKSUM:{ans}")


if __name__ == "__main__":
    main()
