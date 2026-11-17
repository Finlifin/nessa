#!/usr/bin/env bash
# Execute Windows x86_64 PE binaries under Wine configured to Windows 10 1709 (Build 16299).
set -euo pipefail

prefix_dir=${NESSA_WINEPREFIX:-/tmp/nessa-wine-1709}
mkdir -p -- "$prefix_dir"
export WINEPREFIX="$prefix_dir"
export WINEDEBUG=${WINEDEBUG:--all}
export WINEDLLOVERRIDES=${WINEDLLOVERRIDES:-"mscoree,mshtml="}

stamp_file="$prefix_dir/.nessa-win10-1709-v2"
if [[ ! -f "$stamp_file" ]]; then
    (
        flock -x 8
        if [[ ! -f "$stamp_file" ]]; then
            wine reg add 'HKCU\Software\Wine' /v Version /t REG_SZ /d 'win10' /f >/dev/null 2>&1 || true
            wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion' /v CurrentVersion /t REG_SZ /d '10.0' /f >/dev/null 2>&1 || true
            wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion' /v CurrentMajorVersionNumber /t REG_DWORD /d 10 /f >/dev/null 2>&1 || true
            wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion' /v CurrentMinorVersionNumber /t REG_DWORD /d 0 /f >/dev/null 2>&1 || true
            wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion' /v CurrentBuild /t REG_SZ /d '16299' /f >/dev/null 2>&1 || true
            wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion' /v CurrentBuildNumber /t REG_SZ /d '16299' /f >/dev/null 2>&1 || true
            wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion' /v ReleaseId /t REG_SZ /d '1709' /f >/dev/null 2>&1 || true
            touch -- "$stamp_file"
        fi
    ) 8>"$prefix_dir/.nessa-wine-init.lock"
fi

wine_timeout=${NESSA_WINE_TIMEOUT:-120s}
exec timeout --kill-after=5s "$wine_timeout" wine "$@"
