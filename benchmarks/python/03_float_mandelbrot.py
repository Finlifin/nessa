import time


def mandelbrot(size: int, max_iter: int) -> int:
    total_iters = 0
    fsize = float(size)
    for y in range(size):
        ci = 2.0 * y / fsize - 1.0
        for x in range(size):
            cr = 2.0 * x / fsize - 1.5
            zr = 0.0
            zi = 0.0
            it = 0
            while it < max_iter:
                zr2 = zr * zr
                zi2 = zi * zi
                if zr2 + zi2 > 4.0:
                    break
                zi = 2.0 * zr * zi + ci
                zr = zr2 - zi2 + cr
                it += 1
            total_iters += it
    return total_iters


def main() -> None:
    t0 = time.perf_counter_ns()
    ans = mandelbrot(400, 100)
    ms = (time.perf_counter_ns() - t0) / 1_000_000.0
    print(f"KERNEL_MS:{ms:.6f}")
    print(f"CHECKSUM:{ans}")


if __name__ == "__main__":
    main()
