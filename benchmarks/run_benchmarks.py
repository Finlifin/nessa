#!/usr/bin/env python3
"""Full-dimension cross-language benchmark & profiling runner for Nessa.

Compares Nessa (default CLI, O0/O3 interpreter, O3 JIT, precompiled .nsbc)
against Node.js, Lua 5.5, LuaJIT (interpreter & JIT), Emacs Lisp (interpreted,
byte-compiled .elc, and native-compiled .eln), and CPython across 9 scenarios.

Records:
- In-process kernel compute time (ms)
- End-to-end process wall-clock time (ms)
- Child-process peak RSS (MiB) via POSIX wait4 getrusage
- Optional hardware CPU counters (cycles, instructions, IPC, branch misses) via perf stat
- Strict cross-language checksum verification
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent
DEFAULT_NESSA_BIN = REPO_ROOT / "target" / "release" / "nessa"

SCENARIOS = [
    "01_fib_rec",
    "02_int_sieve_lcg",
    "03_float_mandelbrot",
    "04_struct_binary_trees",
    "05_list_quicksort",
    "06_map_word_freq",
    "07_string_processing",
    "08_closure_pipeline",
    "09_nessa_effects",
]


def run_with_rusage(
    cmd: list[str],
    env: dict[str, str] | None = None,
    timeout_sec: float = 180.0,
) -> dict[str, Any]:
    merged_env = os.environ.copy()
    if env:
        merged_env.update(env)

    with tempfile.TemporaryFile(mode="w+b") as out_f, tempfile.TemporaryFile(mode="w+b") as err_f:
        t0 = time.perf_counter_ns()
        proc = subprocess.Popen(
            cmd,
            stdout=out_f,
            stderr=err_f,
            env=merged_env,
        )
        # Poll only on wait4 with WNOHANG? No, blocking os.wait4 is zero-overhead!
        _, status, rusage = os.wait4(proc.pid, 0)
        wall_ms = (time.perf_counter_ns() - t0) / 1_000_000.0
        # Prevent Popen.__del__ from calling waitpid again
        proc.returncode = os.waitstatus_to_exitcode(status)

        out_f.seek(0)
        err_f.seek(0)
        stdout = out_f.read().decode("utf-8", errors="replace")
        stderr = err_f.read().decode("utf-8", errors="replace")

    if proc.returncode != 0:
        raise RuntimeError(
            f"Command failed (exit {proc.returncode}): {' '.join(cmd)}\n"
            f"stdout:\n{stdout}\nstderr:\n{stderr}"
        )

    kernel_ms: float | None = None
    checksum: str | None = None
    for line in stdout.splitlines():
        line = line.strip()
        if line.startswith("KERNEL_MS:"):
            kernel_ms = float(line.split(":", 1)[1].strip())
        elif line.startswith("CHECKSUM:"):
            checksum = line.split(":", 1)[1].strip()

    if kernel_ms is None or checksum is None:
        raise RuntimeError(
            f"Missing KERNEL_MS or CHECKSUM in output of {' '.join(cmd)}:\n{stdout}"
        )

    rss_mib = rusage.ru_maxrss / 1024.0
    user_ms = rusage.ru_utime * 1000.0
    sys_ms = rusage.ru_stime * 1000.0
    return {
        "kernel_ms": kernel_ms,
        "wall_ms": wall_ms,
        "user_ms": user_ms,
        "sys_ms": sys_ms,
        "rss_mib": rss_mib,
        "checksum": checksum,
    }


def run_perf_stat(cmd: list[str], env: dict[str, str] | None = None) -> dict[str, float]:
    merged_env = os.environ.copy()
    if env:
        merged_env.update(env)
    perf_cmd = [
        "perf",
        "stat",
        "-x,",
        "-e",
        "cpu-cycles:u,instructions:u,branches:u,branch-misses:u",
        "--",
        *cmd,
    ]
    proc = subprocess.run(
        perf_cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=merged_env,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return {}
    metrics: dict[str, float] = {}
    for line in proc.stderr.splitlines():
        parts = line.split(",")
        if len(parts) < 3:
            continue
        raw_val, _, event = parts[0].strip(), parts[1].strip(), parts[2].strip()
        if not raw_val or raw_val.startswith("<"):
            continue
        try:
            val = float(raw_val)
        except ValueError:
            continue
        if "cycles" in event:
            metrics["cycles"] = val
        elif "instructions" in event:
            metrics["instructions"] = val
        elif "branch-misses" in event:
            metrics["branch_misses"] = val
        elif "branches" in event:
            metrics["branches"] = val
    if metrics.get("cycles", 0) > 0 and "instructions" in metrics:
        metrics["ipc"] = metrics["instructions"] / metrics["cycles"]
    if metrics.get("branches", 0) > 0 and "branch_misses" in metrics:
        metrics["branch_miss_pct"] = 100.0 * metrics["branch_misses"] / metrics["branches"]
    return metrics


def prepare_artifacts(nessa_bin: Path, build_dir: Path) -> dict[str, dict[str, Path]]:
    artifacts: dict[str, dict[str, Path]] = {"nsbc": {}, "elc": {}, "eln": {}}
    nsbc_dir = build_dir / "nsbc"
    elc_dir = build_dir / "elisp"
    eln_cache = build_dir / "eln-cache"
    nsbc_dir.mkdir(parents=True, exist_ok=True)
    elc_dir.mkdir(parents=True, exist_ok=True)
    eln_cache.mkdir(parents=True, exist_ok=True)

    # 1. Precompile Nessa .nsbc archives at -O3
    for scen in SCENARIOS:
        src = BENCH_DIR / "nessa" / f"{scen}.ns"
        dst_ns = nsbc_dir / f"{scen}.ns"
        shutil.copy2(src, dst_ns)
        subprocess.run(
            [str(nessa_bin), "build", "-O3", str(dst_ns)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "NESSA_NO_CACHE": "1"},
        )
        artifacts["nsbc"][scen] = nsbc_dir / f"{scen}.nsbc"

    # 2. Byte-compile (.elc) and attempt native-compile (.eln) for Emacs Lisp
    for scen in SCENARIOS:
        el_src = BENCH_DIR / "elisp" / f"{scen}.el"
        if not el_src.exists():
            continue
        el_copy = elc_dir / f"{scen}.el"
        shutil.copy2(el_src, el_copy)
        subprocess.run(
            ["emacs", "--batch", "-Q", "-f", "batch-byte-compile", str(el_copy)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        elc_path = elc_dir / f"{scen}.elc"
        if elc_path.exists():
            artifacts["elc"][scen] = elc_path

        # Attempt native compilation (.eln)
        eln_out = eln_cache / f"{scen}.eln"
        res = subprocess.run(
            [
                "emacs",
                "--batch",
                "-Q",
                "--eval",
                f'(progn (startup-redirect-eln-cache "{eln_cache}") '
                f'(native-compile "{el_copy}" "{eln_out}"))',
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        if res.returncode == 0 and eln_out.exists():
            artifacts["eln"][scen] = eln_out

    return artifacts


def build_runtime_specs(
    nessa_bin: Path,
    scen: str,
    artifacts: dict[str, dict[str, Path]],
) -> list[tuple[str, list[str], dict[str, str]]]:
    ns_src = str(BENCH_DIR / "nessa" / f"{scen}.ns")
    no_cache_env = {"NESSA_NO_CACHE": "1"}
    specs: list[tuple[str, list[str], dict[str, str]]] = [
        ("nessa_default", [str(nessa_bin), "run", ns_src], no_cache_env),
        ("nessa_interp_O0", [str(nessa_bin), "run", "-O0", "--jit", "off", ns_src], no_cache_env),
        ("nessa_interp_O3", [str(nessa_bin), "run", "-O3", "--jit", "off", ns_src], no_cache_env),
        ("nessa_jit_O3", [str(nessa_bin), "run", "-O3", "--jit", "auto", ns_src], no_cache_env),
        (
            "nessa_nsbc_jit",
            [str(nessa_bin), "run", "--jit", "auto", str(artifacts["nsbc"][scen])],
            no_cache_env,
        ),
    ]

    js_src = BENCH_DIR / "nodejs" / f"{scen}.js"
    if js_src.exists():
        specs.append(("nodejs", ["node", str(js_src)], {}))

    lua_src = BENCH_DIR / "lua" / f"{scen}.lua"
    if lua_src.exists():
        specs.append(("lua", ["lua", str(lua_src)], {}))
        specs.append(("luajit_interp", ["luajit", "-joff", str(lua_src)], {}))
        specs.append(("luajit", ["luajit", str(lua_src)], {}))

    el_src = BENCH_DIR / "elisp" / f"{scen}.el"
    if el_src.exists():
        specs.append(
            ("elisp_interp", ["emacs", "--batch", "-Q", "-l", str(el_src), "-f", "bench-main"], {})
        )
    if scen in artifacts["elc"]:
        specs.append(
            (
                "elisp_byte",
                ["emacs", "--batch", "-Q", "-l", str(artifacts["elc"][scen]), "-f", "bench-main"],
                {},
            )
        )
    if scen in artifacts["eln"]:
        specs.append(
            (
                "elisp_native",
                [
                    "emacs",
                    "--batch",
                    "-Q",
                    "--eval",
                    f'(progn (load "{artifacts["eln"][scen]}" nil t t) (bench-main))',
                ],
                {},
            )
        )

    py_src = BENCH_DIR / "python" / f"{scen}.py"
    if py_src.exists():
        specs.append(("cpython", ["python3", str(py_src)], {}))

    return specs


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--nessa-bin", type=Path, default=DEFAULT_NESSA_BIN)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--scenarios", nargs="*", default=SCENARIOS)
    parser.add_argument("--runtimes", nargs="*", default=None)
    parser.add_argument("--perf", action="store_true")
    parser.add_argument("--output", type=Path, default=None)
    args = parser.parse_args()

    nessa_bin = args.nessa_bin.resolve()
    if not nessa_bin.exists():
        raise SystemExit(f"Nessa binary not found: {nessa_bin}")

    results: dict[str, Any] = {
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "runs": args.runs,
        "scenarios": {},
    }

    with tempfile.TemporaryDirectory(prefix="nessa_bench_") as tmp:
        artifacts = prepare_artifacts(nessa_bin, Path(tmp))
        eln_enabled = len(artifacts["eln"]) > 0
        print(f"[setup] Precompiled .nsbc ({len(artifacts['nsbc'])}), "
              f".elc ({len(artifacts['elc'])}), .eln ({len(artifacts['eln'])}, native={eln_enabled})")

        for scen in args.scenarios:
            print(f"\n=== Scenario: {scen} ===")
            scen_data: dict[str, Any] = {}
            expected_checksum: str | None = None

            for rt_name, cmd, env in build_runtime_specs(nessa_bin, scen, artifacts):
                if args.runtimes and rt_name not in args.runtimes:
                    continue
                samples = [run_with_rusage(cmd, env=env) for _ in range(args.runs)]
                chk = samples[0]["checksum"]
                if expected_checksum is None:
                    expected_checksum = chk
                elif chk != expected_checksum:
                    raise RuntimeError(
                        f"CHECKSUM MISMATCH in {scen}: {rt_name} produced {chk}, "
                        f"expected {expected_checksum}!"
                    )

                med_kernel = statistics.median(s["kernel_ms"] for s in samples)
                min_kernel = min(s["kernel_ms"] for s in samples)
                med_wall = statistics.median(s["wall_ms"] for s in samples)
                med_rss = statistics.median(s["rss_mib"] for s in samples)

                entry: dict[str, Any] = {
                    "kernel_ms": round(med_kernel, 3),
                    "kernel_min_ms": round(min_kernel, 3),
                    "wall_ms": round(med_wall, 3),
                    "rss_mib": round(med_rss, 2),
                    "checksum": chk,
                }
                if args.perf:
                    entry["perf"] = run_perf_stat(cmd, env=env)

                scen_data[rt_name] = entry
                perf_note = ""
                if "perf" in entry and "instructions" in entry["perf"]:
                    insns_m = entry["perf"]["instructions"] / 1e6
                    ipc = entry["perf"].get("ipc", 0.0)
                    perf_note = f" | insns={insns_m:8.1f}M ipc={ipc:.2f}"
                print(
                    f"  {rt_name:16s} | kernel={med_kernel:9.3f} ms | "
                    f"wall={med_wall:9.3f} ms | rss={med_rss:6.2f} MiB | "
                    f"chk={chk}{perf_note}",
                    flush=True,
                )

            results["scenarios"][scen] = scen_data

    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
        print(f"\nSaved JSON benchmark report to {args.output}")


if __name__ == "__main__":
    main()
