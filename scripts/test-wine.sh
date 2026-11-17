#!/usr/bin/env bash
# Build Windows x86_64 binaries (`nessa.exe`, `nessa_regex_ffi.dll`) and run key unit + integration
# test suites under Wine configured to Windows 10 version 1709 (Build 16299).
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$repo_root"

bash scripts/cargo-win.sh build -p regex_ffi -p nessa

bash scripts/cargo-win.sh test \
    -p stack_pool \
    -p gc \
    -p jit \
    -p runtime \
    -p scheduler \
    -p regex_ffi \
    -p wasm \
    --lib --tests \
    -- --test-threads=2

bash scripts/cargo-win.sh test -p nessa \
    --bins \
    --test windows_compat \
    --test package_dependencies_and_cli \
    -- --test-threads=2

bash scripts/cargo-win.sh test -p nessa-test \
    --test jit_tests \
    --test jit_gc \
    --test ffi_and_regex_tests \
    --test wasm_support_tests \
    --test wave5_stdlib_io_sys_collections_tests \
    --test wave_c4_reactor_and_deadlock_tests \
    --test wave_c5_sync_net_and_task_tests \
    --test wave_c6_release_gates_tests \
    --test async_soundness_and_cancel_tests \
    -- --test-threads=2
