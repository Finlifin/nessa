#!/usr/bin/env bash
# Build or test Nessa workspace crates for Windows (x86_64-pc-windows-gnullvm, Win10 1709+) via `scripts/cargo.sh` + Wine.
set -euo pipefail

scripts_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
chmod +x "$scripts_dir/zig-win-linker.sh" "$scripts_dir/wine-runner.sh" 2>/dev/null || true

export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER="$scripts_dir/zig-win-linker.sh"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUNNER="$scripts_dir/wine-runner.sh"

orig_cargo_home=${CARGO_HOME:-"$HOME/.cargo"}
if [[ ! -w "$orig_cargo_home/registry" ]]; then
    sandbox_cargo_home=${NESSA_SANDBOX_CARGO_HOME:-/tmp/nessa-cargo-home}
    mkdir -p -- "$sandbox_cargo_home/registry/src"
    ln -sfn "$orig_cargo_home/registry/cache" "$sandbox_cargo_home/registry/cache"
    ln -sfn "$orig_cargo_home/registry/index" "$sandbox_cargo_home/registry/index"
    if [[ -d "$orig_cargo_home/bin" ]]; then
        ln -sfn "$orig_cargo_home/bin" "$sandbox_cargo_home/bin"
    fi
    export CARGO_HOME="$sandbox_cargo_home"
    set -- "${1:-check}" --offline "${@:2}"
fi

subcmd=${1:-check}
shift || true

exec bash "$scripts_dir/cargo.sh" "$subcmd" --target x86_64-pc-windows-gnullvm "$@"
