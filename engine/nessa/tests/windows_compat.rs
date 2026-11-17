//! Windows 10 1709 (RS3, Build 16299) PE/COFF static import audit and end-to-end runtime verification.

#![cfg(windows)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn read_u16_le(bytes: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([bytes[off], bytes[off + 1]])
}

fn read_u32_le(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

fn read_u64_le(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        bytes[off],
        bytes[off + 1],
        bytes[off + 2],
        bytes[off + 3],
        bytes[off + 4],
        bytes[off + 5],
        bytes[off + 6],
        bytes[off + 7],
    ])
}

struct PeAudit {
    major_os: u16,
    minor_os: u16,
    major_subsys: u16,
    minor_subsys: u16,
    imports: BTreeMap<String, BTreeSet<String>>,
}

fn rva_to_offset(sections: &[(u32, u32, u32, u32)], rva: u32) -> Option<usize> {
    for &(virt_addr, virt_size, raw_ptr, raw_size) in sections {
        let span = virt_size.max(raw_size);
        if rva >= virt_addr && rva < virt_addr.saturating_add(span) {
            return Some((raw_ptr + (rva - virt_addr)) as usize);
        }
    }
    None
}

fn read_cstr(bytes: &[u8], mut off: usize) -> String {
    let start = off;
    while off < bytes.len() && bytes[off] != 0 {
        off += 1;
    }
    String::from_utf8_lossy(&bytes[start..off]).into_owned()
}

fn audit_pe64_binary(bytes: &[u8]) -> PeAudit {
    assert!(bytes.len() > 0x100, "PE binary too small");
    assert_eq!(&bytes[0..2], b"MZ", "missing DOS MZ magic");
    let pe_off = read_u32_le(bytes, 0x3C) as usize;
    assert_eq!(
        &bytes[pe_off..pe_off + 4],
        b"PE\0\0",
        "missing PE signature"
    );

    let coff_off = pe_off + 4;
    let machine = read_u16_le(bytes, coff_off);
    assert_eq!(machine, 0x8664, "expected IMAGE_FILE_MACHINE_AMD64");
    let num_sections = read_u16_le(bytes, coff_off + 2) as usize;
    let opt_hdr_size = read_u16_le(bytes, coff_off + 16) as usize;

    let opt_off = coff_off + 20;
    let opt_magic = read_u16_le(bytes, opt_off);
    assert_eq!(opt_magic, 0x20B, "expected PE32+ optional header");

    let major_os = read_u16_le(bytes, opt_off + 40);
    let minor_os = read_u16_le(bytes, opt_off + 42);
    let major_subsys = read_u16_le(bytes, opt_off + 48);
    let minor_subsys = read_u16_le(bytes, opt_off + 50);
    let num_rva_and_sizes = read_u32_le(bytes, opt_off + 108) as usize;

    let sec_table_off = opt_off + opt_hdr_size;
    let mut sections = Vec::with_capacity(num_sections);
    for i in 0..num_sections {
        let s = sec_table_off + i * 40;
        let virt_size = read_u32_le(bytes, s + 8);
        let virt_addr = read_u32_le(bytes, s + 12);
        let raw_size = read_u32_le(bytes, s + 16);
        let raw_ptr = read_u32_le(bytes, s + 20);
        sections.push((virt_addr, virt_size, raw_ptr, raw_size));
    }

    let mut imports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if num_rva_and_sizes >= 2 {
        let import_dir_rva = read_u32_le(bytes, opt_off + 112 + 8);
        let import_dir_size = read_u32_le(bytes, opt_off + 112 + 12);
        if import_dir_rva != 0
            && import_dir_size >= 20
            && let Some(mut desc_off) = rva_to_offset(&sections, import_dir_rva)
        {
            while desc_off + 20 <= bytes.len() {
                let ilt_rva = read_u32_le(bytes, desc_off);
                let name_rva = read_u32_le(bytes, desc_off + 12);
                let iat_rva = read_u32_le(bytes, desc_off + 16);
                if ilt_rva == 0 && name_rva == 0 && iat_rva == 0 {
                    break;
                }
                let dll_name = rva_to_offset(&sections, name_rva)
                    .map(|o| read_cstr(bytes, o).to_ascii_lowercase())
                    .unwrap_or_default();
                let syms = imports.entry(dll_name).or_default();

                let table_rva = if ilt_rva != 0 { ilt_rva } else { iat_rva };
                if let Some(mut thunk_off) = rva_to_offset(&sections, table_rva) {
                    while thunk_off + 8 <= bytes.len() {
                        let entry = read_u64_le(bytes, thunk_off);
                        if entry == 0 {
                            break;
                        }
                        if (entry & (1u64 << 63)) == 0 {
                            let hint_name_rva = (entry & 0x7FFF_FFFF) as u32;
                            if let Some(hn_off) = rva_to_offset(&sections, hint_name_rva) {
                                syms.insert(read_cstr(bytes, hn_off + 2));
                            }
                        } else {
                            syms.insert(format!("ordinal#{}", entry & 0xFFFF));
                        }
                        thunk_off += 8;
                    }
                }
                desc_off += 20;
            }
        }
    }

    PeAudit {
        major_os,
        minor_os,
        major_subsys,
        minor_subsys,
        imports,
    }
}

#[test]
fn pe_headers_and_imports_strictly_target_windows_10_1709_baseline() {
    let exe_path = PathBuf::from(env!("CARGO_BIN_EXE_nessa"));
    let exe_bytes = fs::read(&exe_path).expect("read nessa.exe bytes");
    let audit = audit_pe64_binary(&exe_bytes);

    // OS and subsystem version headers must not exceed Windows 10.0 (1709 is 10.0.16299).
    assert!(
        (audit.major_os, audit.minor_os) <= (10, 0),
        "PE OS version {}.{} exceeds Windows 10 1709 (10.0)",
        audit.major_os,
        audit.minor_os
    );
    assert!(
        (audit.major_subsys, audit.minor_subsys) <= (10, 0),
        "PE Subsystem version {}.{} exceeds Windows 10 1709 (10.0)",
        audit.major_subsys,
        audit.minor_subsys
    );

    // Disallow post-1709 API sets and external MinGW/MSVC runtime redistributable DLLs.
    let forbidden_dlls = [
        "api-ms-win-core-memory-l1-1-6.dll", // Win10 1803+ (VirtualAlloc2)
        "api-ms-win-core-memory-l1-1-7.dll", // Win10 1903+
        "api-ms-win-core-file-l1-2-4.dll",   // post-1709 file APIs
        "libgcc_s_seh-1.dll",
        "libwinpthread-1.dll",
        "libstdc++-6.dll",
        "vcruntime140.dll",
    ];
    for dll in audit.imports.keys() {
        assert!(
            !forbidden_dlls.contains(&dll.as_str()),
            "nessa.exe imports forbidden or non-self-contained DLL `{dll}`; imports={:?}",
            audit.imports.keys().collect::<Vec<_>>()
        );
    }

    // Disallow post-1709 Win32 symbols across all imported DLLs.
    let forbidden_symbols = [
        "VirtualAlloc2",
        "VirtualAlloc2FromApp",
        "MapViewOfFile3",
        "MapViewOfFile3FromApp",
        "CreateFileMapping2",
        "GetTempPath2W",
        "GetTempPath2A",
        "CreateFile3",
    ];
    for (dll, syms) in &audit.imports {
        for bad in forbidden_symbols {
            assert!(
                !syms.contains(bad),
                "nessa.exe imports post-1709 API `{bad}` from `{dll}`"
            );
        }
    }

    // Confirm expected Win10 1709-compatible primitives are wired cleanly.
    let k32 = audit
        .imports
        .get("kernel32.dll")
        .expect("kernel32.dll imported");
    assert!(k32.contains("VirtualAlloc"));
    assert!(k32.contains("VirtualProtect"));
    assert!(k32.contains("AddVectoredExceptionHandler"));
    let ws2 = audit
        .imports
        .get("ws2_32.dll")
        .expect("ws2_32.dll imported");
    assert!(ws2.contains("WSAPoll"));
}

#[test]
fn windows_1709_cli_jit_gc_reactor_paths_process_cancel_and_wasm_end_to_end() {
    #[repr(C)]
    struct OsVersionInfoW {
        os_version_info_size: u32,
        major_version: u32,
        minor_version: u32,
        build_number: u32,
        platform_id: u32,
        csd_version: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(lp_version_information: *mut OsVersionInfoW) -> i32;
    }

    let mut ver = OsVersionInfoW {
        os_version_info_size: std::mem::size_of::<OsVersionInfoW>() as u32,
        major_version: 0,
        minor_version: 0,
        build_number: 0,
        platform_id: 0,
        csd_version: [0u16; 128],
    };
    // SAFETY: `RtlGetVersion` (NT 5.0+) populates `OsVersionInfoW` without manifest lying.
    let status = unsafe { RtlGetVersion(&raw mut ver) };
    assert_eq!(status, 0);
    assert!(
        ver.major_version > 10 || (ver.major_version == 10 && ver.build_number >= 16299),
        "expected Windows 10 1709+ (10.0.16299+), got {}.{}.{}",
        ver.major_version,
        ver.minor_version,
        ver.build_number
    );

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let work_dir = std::env::temp_dir().join(format!(
        "nessa win1709 e2e {} {} {}",
        std::process::id(),
        stamp,
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(work_dir.join("src")).expect("create temp package dir with spaces");

    fs::write(
        work_dir.join("package.toml"),
        r#"[package]
name = "win1709_e2e"
domain = "org.nessa"
version = "0.1.0"
type = "exe"
"#,
    )
    .unwrap();

    let payload_path = work_dir
        .join("win_payload.bin")
        .to_string_lossy()
        .replace('\\', "\\\\");

    fs::write(
        work_dir.join("src\\main.ns"),
        format!(
            r#"use std.collections.*
use std.env
use std.fs
use std.io.IoError
use std.net.{{TcpListener, TcpStream, UdpSocket, resolve_host}}
use std.path
use std.process
use std.rand
use std.runtime
use std.task
use std.time
use std.time.DateTime
use std.wasm

async effect tcp_srv(lst: TcpListener) -> !IoError i64
async effect tcp_cli(port: i64) -> !IoError i64
effect branch(catch k: Continuation) -> i64

fn hot_jit_loop(n: i64) -> i64 {{
    var i: i64 = 0
    var acc: i64 = 0
    while i < n {{
        acc += (i % 7) + 1
        i += 1
    }}
    acc
}}

fn run_tcp_pair(lst: TcpListener, p: i64) -> #[tcp_srv, tcp_cli] !IoError i64 {{
    let s = tcp_srv(lst)#
    let c = tcp_cli(p)#
    s.await! + c.await!
}}

fn run_branch() -> #branch Any {{
    let x = branch()#
    x + 5
}}

fn tcp_roundtrip() -> !IoError i64 {{
    let lst = TcpListener.bind("127.0.0.1", 0)!
    defer {{ lst.close() }}
    let p = lst.local_addr()!.port
    let res = run_tcp_pair(lst, p) # {{
        async tcp_srv(l) => {{
            let conn = l.accept()!
            defer {{ conn.close() }}
            let msg = conn.read_line()! match {{
                s? => s,
                null => "",
            }}
            conn.write_line("win:" ++ msg)!
            10
        }},
        async tcp_cli(port) => {{
            let cli = TcpStream.connect("127.0.0.1", port)!
            defer {{ cli.close() }}
            cli.write_line("1709")!
            let reply = cli.read_line()! match {{
                s? => s,
                null => "",
            }}
            if reply == "win:1709" {{ 20 }} else {{ -999 }}
        }},
    }}
    res!
}}

fn udp_and_dns() -> !IoError i64 {{
    let addrs = resolve_host("127.0.0.1")!
    if addrs.len() < 1 {{ return -100 }}
    let a = UdpSocket.bind("127.0.0.1", 0)!
    defer {{ a.close() }}
    let b = UdpSocket.bind("127.0.0.1", 0)!
    defer {{ b.close() }}
    let bp = b.local_addr()!.port
    a.send_to_str("127.0.0.1", bp, "udp1709")!
    let d = b.recv_from()!
    if d.text() == "udp1709" {{ 30 }} else {{ -200 }}
}}

fn main() {{
    assert_eq(env.os(), "windows")
    assert_eq(path.SEPARATOR, "\\")
    assert_eq(path.join("C:\\win10", "1709"), "C:\\win10\\1709")

    fs.write_raw_bytes("{payload_path}", std.bytes.Bytes.from_string("nessa-win1709")).unwrap()
    let rb = fs.read_raw_bytes("{payload_path}").unwrap()
    assert_eq(rb.to_utf8(), "nessa-win1709")

    assert(time.cpu_secs() >= 0.0)
    let dt = DateTime.from_unix_local(1700000000)
    assert_eq(dt.to_unix_local(), 1700000000)
    let _r = rand.next_u64()

    let sh = process.shell("echo win1709-shell").unwrap()
    assert_eq(sh.stdout.trim(), "win1709-shell")

    -- Prompt cancellation of long-running Windows child process (`cmd.exe` loop)
    let t_cancel = time.monotonic_nanos()
    let cancel_score = task.with_timeout(40, || -> !IoError i64 {{
        let _ = process.shell("for /L %i in (1,1,20000000) do @rem")!
        -1
    }}) ! {{
        _ => 40,
    }}
    let cancel_ms = (time.monotonic_nanos() - t_cancel) / 1000000
    assert_eq(cancel_score, 40)
    assert(cancel_ms < 2500)

    let jit_sum = hot_jit_loop(50000)
    assert(jit_sum > 0)
    let g0 = runtime.gc_collections()
    let g1 = runtime.collect_garbage()
    assert(g1 > g0)

    let k = (run_branch() # {{ branch(cont) => cont }}).as(Continuation)
    let k2 = k.clone()
    assert_eq(k(10).as(i64) + k2(20).as(i64), 40)

    let net_score = (tcp_roundtrip() ! {{ _ => -1 }}) + (udp_and_dns() ! {{ _ => -2 }})
    assert_eq(net_score, 60)

    -- Minimal valid Wasm module: `(module (func (export "add") (param i64 i64) (result i64) local.get 0 local.get 1 i64.add))`
    let wasm_bytes = [
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x07, 0x01, 0x60, 0x02, 0x7e, 0x7e, 0x01, 0x7e,
        0x03, 0x02, 0x01, 0x00,
        0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00,
        0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x7c, 0x0b
    ]
    let inst = wasm.compile(std.bytes.Bytes.from_list(wasm_bytes)).unwrap().instantiate().unwrap()
    let wsum = inst.call_i64("add", args = [1700, 9]).unwrap()
    assert_eq(wsum, 1709)

    println("WIN1709_ALL_OK")
}}
"#
        ),
    )
    .unwrap();

    let nessa_exe = env!("CARGO_BIN_EXE_nessa");
    let pkg_arg = work_dir.to_str().unwrap();

    // 1. `nessa.exe check`
    let chk = Command::new(nessa_exe)
        .args(["check", pkg_arg])
        .output()
        .unwrap();
    assert!(
        chk.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&chk.stderr)
    );

    // 2. `nessa.exe run -O2` across Tier-2 JIT
    let started = Instant::now();
    let run_out = Command::new(nessa_exe)
        .args(["run", "-O2", "--jit", "tier2", pkg_arg])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        run_out.status.success(),
        "run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run_out.stdout).trim(),
        "WIN1709_ALL_OK"
    );
    assert!(elapsed < Duration::from_secs(10));

    // 3. `nessa.exe build` + execute standalone `.nsbc` archive
    let bld = Command::new(nessa_exe)
        .args(["build", "-O3", pkg_arg])
        .output()
        .unwrap();
    assert!(
        bld.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&bld.stderr)
    );
    let nsbc_path = work_dir.join("package.nsbc");
    assert!(nsbc_path.is_file());
    let run_nsbc = Command::new(nessa_exe)
        .args(["run", nsbc_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(run_nsbc.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run_nsbc.stdout).trim(),
        "WIN1709_ALL_OK"
    );

    // 4. `nessa.exe build --target wasm32-wasi` + `nessa.exe run out.wasm` on Windows
    let wasm_src = work_dir.join("hello_wasi.ns");
    let wasm_out = work_dir.join("hello_wasi.wasm");
    fs::write(&wasm_src, "fn main() {\n    println(1700 + 9)\n}\n").unwrap();
    let bld_wasm = Command::new(nessa_exe)
        .args([
            "build",
            "--target",
            "wasm32-wasi",
            "-o",
            wasm_out.to_str().unwrap(),
            wasm_src.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        bld_wasm.status.success(),
        "wasm build failed: {}",
        String::from_utf8_lossy(&bld_wasm.stderr)
    );
    let run_wasm = Command::new(nessa_exe)
        .args(["run", wasm_out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(run_wasm.status.success());
    assert_eq!(String::from_utf8_lossy(&run_wasm.stdout).trim(), "1709");

    let _ = fs::remove_dir_all(&work_dir);
}
