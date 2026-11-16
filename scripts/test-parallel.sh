#!/usr/bin/env bash
# Memory-bounded parallel test runner for the Nessa workspace.
#
# Cargo's built-in test runner executes all ~180 test binaries strictly one-by-one.
# This wrapper compiles test binaries once through `scripts/cargo.sh` (respecting
# the workspace lock and cache-size guard) and then runs up to `NESSA_TEST_JOBS`
# test binaries concurrently, each with `--test-threads=$NESSA_TEST_THREADS` and
# an isolated `NESSA_CACHE_DIR` so parallel CLI/package suites never collide.
#
# Usage:
#   bash scripts/test-parallel.sh                 # defaults to --workspace
#   bash scripts/test-parallel.sh -p nessa-test   # specific package(s) / targets
#
# Tunables:
#   NESSA_TEST_JOBS      concurrent test binaries (default: 8)
#   NESSA_TEST_THREADS   threads per test binary  (default: 4)
#   NESSA_BUILD_JOBS     cargo -j build jobs      (default: 6)
set -euo pipefail

workspace_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$workspace_dir"

if (( $# == 0 )); then
    set -- --workspace
fi

build_jobs=${NESSA_BUILD_JOBS:-6}
test_jobs=${NESSA_TEST_JOBS:-8}
test_threads=${NESSA_TEST_THREADS:-4}

tmp_root=$(mktemp -d "${TMPDIR:-/tmp}/nessa-test-parallel.XXXXXX")
trap 'rm -rf -- "$tmp_root"' EXIT

json_log="$tmp_root/build.jsonl"
echo "[test-parallel] Building nessa CLI & test targets ($*) with -j ${build_jobs}..." >&2
bash "$workspace_dir/scripts/cargo.sh" build -j "$build_jobs" -p nessa --bin nessa >/dev/null
bash "$workspace_dir/scripts/cargo.sh" test -j "$build_jobs" "$@" --no-run --message-format=json >"$json_log"

python3 - "$json_log" "$tmp_root" "$test_jobs" "$test_threads" "$workspace_dir" <<'PY'
import concurrent.futures
import json
import os
import shutil
import subprocess
import sys
import time

json_log, tmp_root, max_jobs, test_threads, workspace_dir = sys.argv[1:6]
max_jobs = max(1, int(max_jobs))
test_threads = max(1, int(test_threads))

binaries = []
seen = set()
with open(json_log, "r", encoding="utf-8") as fh:
    for line in fh:
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        if not msg.get("profile", {}).get("test"):
            continue
        exe = msg.get("executable")
        if not exe or exe in seen:
            continue
        seen.add(exe)
        target = msg.get("target", {})
        pkg_id = msg.get("package_id", "")
        pkg_name = pkg_id.split("/")[-1].split("#")[0] if pkg_id else "unknown"
        label = f"{pkg_name}::{target.get('name', os.path.basename(exe))}"
        manifest_path = msg.get("manifest_path")
        cwd = os.path.dirname(manifest_path) if manifest_path else workspace_dir
        binaries.append((label, exe, cwd))

if not binaries:
    print("[test-parallel] No test binaries matched.", file=sys.stderr)
    sys.exit(0)

# Resolve `CARGO_BIN_EXE_nessa` when present in target/debug so CLI integration tests work standalone.
target_dir = os.environ.get("CARGO_TARGET_DIR", os.path.join(workspace_dir, "target"))
nessa_bin = os.path.join(target_dir, "debug", "nessa")

print(
    f"[test-parallel] Running {len(binaries)} test binaries "
    f"(jobs={max_jobs}, test-threads={test_threads})...",
    file=sys.stderr,
    flush=True,
)

start_all = time.monotonic()
failures = []
completed = 0
total = len(binaries)

def run_one(index_and_item):
    idx, (label, exe, cwd) = index_and_item
    cache_dir = os.path.join(tmp_root, f"cache-{idx}")
    os.makedirs(cache_dir, exist_ok=True)
    env = os.environ.copy()
    env["NESSA_CACHE_DIR"] = cache_dir
    if os.path.isfile(nessa_bin):
        env.setdefault("CARGO_BIN_EXE_nessa", nessa_bin)
    t0 = time.monotonic()
    proc = subprocess.run(
        [exe, f"--test-threads={test_threads}"],
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    dt = time.monotonic() - t0
    shutil.rmtree(cache_dir, ignore_errors=True)
    return label, exe, proc.returncode, dt, proc.stdout

def is_exclusive_timing_suite(label):
    return ("multicore" in label) or ("reactor" in label)

parallel_bins = [(i, b) for i, b in enumerate(binaries) if not is_exclusive_timing_suite(b[0])]
serial_bins = [(i, b) for i, b in enumerate(binaries) if is_exclusive_timing_suite(b[0])]

with concurrent.futures.ThreadPoolExecutor(max_workers=max_jobs) as pool:
    futures = [pool.submit(run_one, item) for item in parallel_bins]
    for fut in concurrent.futures.as_completed(futures):
        label, exe, code, dt, out = fut.result()
        completed += 1
        status = "PASS" if code == 0 else f"FAIL(exit={code})"
        print(f"[{completed:3d}/{total:3d}] {status} {label} ({dt:.1f}s)", file=sys.stderr, flush=True)
        if code != 0:
            failures.append((label, exe, code, out))

for item in serial_bins:
    label, exe, code, dt, out = run_one(item)
    completed += 1
    status = "PASS" if code == 0 else f"FAIL(exit={code})"
    print(f"[{completed:3d}/{total:3d}] {status} {label} [exclusive-cpu] ({dt:.1f}s)", file=sys.stderr, flush=True)
    if code != 0:
        failures.append((label, exe, code, out))

elapsed = time.monotonic() - start_all
if failures:
    print(f"\n[test-parallel] {len(failures)}/{total} test binaries FAILED in {elapsed:.1f}s:", file=sys.stderr)
    for label, exe, code, out in failures:
        print(f"\n=== FAILURE: {label} ({exe}, exit {code}) ===", file=sys.stderr)
        sys.stderr.write(out[-16000:] if len(out) > 16000 else out)
    sys.exit(1)

print(f"[test-parallel] All {total}/{total} test binaries passed in {elapsed:.1f}s.", file=sys.stderr)
PY
