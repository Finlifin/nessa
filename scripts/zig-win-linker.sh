#!/usr/bin/env bash
# Link x86_64-pc-windows-gnullvm PE binaries (.exe / .dll) via `zig cc` targeting Windows 10 (10.0 / 1709+).
set -euo pipefail

cache_dir=${NESSA_ZIG_CACHE_DIR:-/tmp/nessa-zig-cache}
mkdir -p -- "$cache_dir"
export ZIG_GLOBAL_CACHE_DIR="$cache_dir"
export ZIG_LOCAL_CACHE_DIR="$cache_dir"

filtered=()
for arg in "$@"; do
    case "$arg" in
        -nolibc|--unwindlib=none|-Wl,-Bstatic|-Wl,-Bdynamic|-lunwind|-lmingw32|-lmingwex|-lmsvcrt|-lgcc_eh|-lgcc_s|-l:libpthread.a|-lgcc|-l:libunwind.a)
            ;;
        -Wl,*.def)
            filtered+=("${arg#-Wl,}")
            ;;
        -Wl,--out-implib=*)
            ;;
        *crt2.o|*dllcrt2.o|*crtbegin.o|*crtend.o)
            ;;
        *)
            filtered+=("$arg")
            ;;
    esac
done

exec zig cc -target x86_64-windows-gnu \
    -lunwind \
    -Wl,--tsaware \
    -Wl,--major-os-version=10 \
    -Wl,--minor-os-version=0 \
    -Wl,--major-subsystem-version=10 \
    -Wl,--minor-subsystem-version=0 \
    "${filtered[@]}"
