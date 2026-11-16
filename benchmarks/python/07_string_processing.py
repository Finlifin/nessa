import time


def run_string_bench(lines: int) -> int:
    total_latency = 0
    total_bytes = 0
    matched_ok = 0
    for i in range(lines):
        lat = (i * 37 + 13) % 997
        code = "500" if i % 5 == 0 else "200"
        line = f"2026-11-17T12:00:00Z service=gateway req={i} latency={lat}ms status={code}"
        total_bytes += len(line)
        if line.endswith("status=200"):
            matched_ok += 1
        pos = line.find("latency=")
        if pos >= 0:
            start = pos + 8
            end_pos = line.find("ms status=")
            if end_pos >= 0:
                total_latency += int(line[start:end_pos])
    return total_latency * 100 + matched_ok + (total_bytes % 100000)


def main() -> None:
    t0 = time.perf_counter_ns()
    ans = run_string_bench(25000)
    ms = (time.perf_counter_ns() - t0) / 1_000_000.0
    print(f"KERNEL_MS:{ms:.6f}")
    print(f"CHECKSUM:{ans}")


if __name__ == "__main__":
    main()
