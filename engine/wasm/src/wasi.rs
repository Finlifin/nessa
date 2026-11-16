//! Sandboxed WASI Preview 1 (`wasi_snapshot_preview1`) host environment.
//!
//! Supports both fully virtualized in-memory execution (capturing `stdout`/`stderr` and feeding
//! virtual `stdin`/`args`/`env` with zero host filesystem privileges) and live CLI stdio passthrough
//! for `nessa run <file.wasm>`.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::exec::{HostFuncMap, WasmInstance};
use crate::{WasmConfig, WasmError, WasmImportKind, WasmModule, WasmTrap, WasmVal};

const ERRNO_SUCCESS: i32 = 0;
const ERRNO_BADF: i32 = 8;
const ERRNO_INVAL: i32 = 28;
const ERRNO_NOSYS: i32 = 52;

/// Configuration for running a WASI Preview 1 command module.
#[derive(Debug, Clone, Default)]
pub struct WasiConfig {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: Vec<u8>,
    /// When `true`, `fd_write` on fd 1/2 forwards directly to host stdout/stderr in addition to
    /// optional capture; when `false` (default for `std.wasm`), output is captured purely in memory.
    pub inherit_stdio: bool,
    pub limits: WasmConfig,
}

/// Captured result of running a WASI command module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasiOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

struct WasiState {
    args: Vec<Vec<u8>>,
    env: Vec<Vec<u8>>,
    stdin: Vec<u8>,
    stdin_pos: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    inherit_stdio: bool,
    start_instant: Instant,
    rng_state: u64,
}

impl WasiState {
    fn next_rand_u64(&mut self) -> u64 {
        // SplitMix64 deterministic stream seeded from monotonic/wall clock.
        self.rng_state = self.rng_state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng_state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Validate, instantiate, and run a `.wasm` WASI command module (`_start`).
pub fn run_wasi_bytes(bytes: &[u8], cfg: WasiConfig) -> Result<WasiOutput, WasmError> {
    let module = Arc::new(WasmModule::from_bytes(bytes)?);
    run_wasi_module(module, cfg)
}

/// Instantiate and execute `_start` (or `main`) on a pre-parsed [`WasmModule`].
pub fn run_wasi_module(module: Arc<WasmModule>, cfg: WasiConfig) -> Result<WasiOutput, WasmError> {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x4E45_5353_415F_5741);

    let state = Arc::new(Mutex::new(WasiState {
        args: cfg.args.iter().map(|a| a.as_bytes().to_vec()).collect(),
        env: cfg
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}").into_bytes())
            .collect(),
        stdin: cfg.stdin,
        stdin_pos: 0,
        stdout: Vec::new(),
        stderr: Vec::new(),
        inherit_stdio: cfg.inherit_stdio,
        start_instant: Instant::now(),
        rng_state: seed ^ 0xA5A5_5A5A_F0F0_0F0F,
    }));

    let host_imports = build_wasi_imports(&module, Arc::clone(&state))?;
    let mut instance = WasmInstance::instantiate(
        Arc::clone(&module),
        cfg.limits,
        host_imports,
        &HashMap::new(),
    )?;

    let entry_res = if module.export_func_sig("_start").is_some() {
        instance.call_export("_start", &[])
    } else if let Some(sig) = module.export_func_sig("main") {
        if sig.params.is_empty() {
            instance.call_export("main", &[])
        } else if sig.params.len() == 2 {
            instance.call_export("main", &[WasmVal::I32(0), WasmVal::I32(0)])
        } else {
            return Err(WasmError::SignatureMismatch(
                "`main` export must take 0 or 2 parameters".into(),
            ));
        }
    } else {
        return Err(WasmError::UnknownExport("_start".into()));
    };

    let exit_code = match entry_res {
        Ok(vals) => match vals.first() {
            Some(WasmVal::I32(code)) => *code,
            Some(WasmVal::I64(code)) => *code as i32,
            _ => 0,
        },
        Err(WasmError::Trap(WasmTrap::Exit(code))) => code,
        Err(other) => return Err(other),
    };

    let mut guard = state.lock().unwrap();
    Ok(WasiOutput {
        exit_code,
        stdout: std::mem::take(&mut guard.stdout),
        stderr: std::mem::take(&mut guard.stderr),
    })
}

fn build_wasi_imports(
    module: &WasmModule,
    state: Arc<Mutex<WasiState>>,
) -> Result<HostFuncMap, WasmError> {
    let mut map: HostFuncMap = HashMap::new();
    for imp in module.imports() {
        if imp.module != "wasi_snapshot_preview1" && imp.module != "wasi_unstable" {
            continue;
        }
        let WasmImportKind::Func(ref sig) = imp.kind else {
            continue;
        };
        let mod_name = imp.module.clone();
        let fn_name = imp.name.clone();
        let st = Arc::clone(&state);
        let result_ty = sig.results.first().copied();

        let cb: crate::exec::HostCallback = match fn_name.as_str() {
            "proc_exit" => Box::new(move |_caller, args| {
                let code = match args.first() {
                    Some(WasmVal::I32(c)) => *c,
                    _ => 1,
                };
                Err(WasmTrap::Exit(code))
            }),
            "fd_write" => Box::new(move |caller, args| {
                let fd = arg_i32(args, 0);
                let iovs_ptr = arg_u32(args, 1);
                let iovs_len = arg_u32(args, 2);
                let nwritten_ptr = arg_u32(args, 3);
                if fd != 1 && fd != 2 {
                    return Ok(vec![WasmVal::I32(ERRNO_BADF)]);
                }
                let mut total = 0u32;
                let mut collected = Vec::new();
                for i in 0..iovs_len {
                    let base_off = iovs_ptr
                        .checked_add(i.checked_mul(8).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    let buf_ptr = caller.read_u32_le(base_off)?;
                    let buf_len = caller.read_u32_le(base_off + 4)?;
                    let slice = caller.read_bytes(buf_ptr, buf_len)?;
                    collected.extend_from_slice(slice);
                    total = total.saturating_add(buf_len);
                }
                {
                    let mut s = st.lock().unwrap();
                    if s.inherit_stdio {
                        if fd == 1 {
                            let _ = io::stdout().write_all(&collected);
                            let _ = io::stdout().flush();
                        } else {
                            let _ = io::stderr().write_all(&collected);
                            let _ = io::stderr().flush();
                        }
                    }
                    if fd == 1 {
                        s.stdout.extend_from_slice(&collected);
                    } else {
                        s.stderr.extend_from_slice(&collected);
                    }
                }
                caller.write_u32_le(nwritten_ptr, total)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "fd_read" => Box::new(move |caller, args| {
                let fd = arg_i32(args, 0);
                let iovs_ptr = arg_u32(args, 1);
                let iovs_len = arg_u32(args, 2);
                let nread_ptr = arg_u32(args, 3);
                if fd != 0 {
                    return Ok(vec![WasmVal::I32(ERRNO_BADF)]);
                }
                let mut total = 0u32;
                for i in 0..iovs_len {
                    let base_off = iovs_ptr
                        .checked_add(i.checked_mul(8).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    let buf_ptr = caller.read_u32_le(base_off)?;
                    let buf_len = caller.read_u32_le(base_off + 4)? as usize;
                    let chunk = {
                        let mut s = st.lock().unwrap();
                        let avail = s.stdin.len().saturating_sub(s.stdin_pos);
                        let n = avail.min(buf_len);
                        let data = s.stdin[s.stdin_pos..s.stdin_pos + n].to_vec();
                        s.stdin_pos += n;
                        data
                    };
                    let n = chunk.len();
                    if n > 0 {
                        caller.write_bytes(buf_ptr, &chunk)?;
                        total = total.saturating_add(n as u32);
                    }
                    if n < buf_len {
                        break;
                    }
                }
                caller.write_u32_le(nread_ptr, total)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "args_sizes_get" => Box::new(move |caller, args| {
                let argc_ptr = arg_u32(args, 0);
                let argv_buf_size_ptr = arg_u32(args, 1);
                let (argc, buf_sz) = {
                    let s = st.lock().unwrap();
                    let sz: usize = s.args.iter().map(|a| a.len() + 1).sum();
                    (s.args.len() as u32, sz as u32)
                };
                caller.write_u32_le(argc_ptr, argc)?;
                caller.write_u32_le(argv_buf_size_ptr, buf_sz)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "args_get" => Box::new(move |caller, args| {
                let mut argv_ptr = arg_u32(args, 0);
                let mut buf_ptr = arg_u32(args, 1);
                let cloned_args = st.lock().unwrap().args.clone();
                for arg in &cloned_args {
                    caller.write_u32_le(argv_ptr, buf_ptr)?;
                    argv_ptr = argv_ptr
                        .checked_add(4)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    caller.write_bytes(buf_ptr, arg)?;
                    buf_ptr = buf_ptr
                        .checked_add(arg.len() as u32)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    caller.write_bytes(buf_ptr, &[0])?;
                    buf_ptr = buf_ptr
                        .checked_add(1)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                }
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "environ_sizes_get" => Box::new(move |caller, args| {
                let envc_ptr = arg_u32(args, 0);
                let env_buf_size_ptr = arg_u32(args, 1);
                let (envc, buf_sz) = {
                    let s = st.lock().unwrap();
                    let sz: usize = s.env.iter().map(|e| e.len() + 1).sum();
                    (s.env.len() as u32, sz as u32)
                };
                caller.write_u32_le(envc_ptr, envc)?;
                caller.write_u32_le(env_buf_size_ptr, buf_sz)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "environ_get" => Box::new(move |caller, args| {
                let mut env_ptr = arg_u32(args, 0);
                let mut buf_ptr = arg_u32(args, 1);
                let cloned_env = st.lock().unwrap().env.clone();
                for entry in &cloned_env {
                    caller.write_u32_le(env_ptr, buf_ptr)?;
                    env_ptr = env_ptr
                        .checked_add(4)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    caller.write_bytes(buf_ptr, entry)?;
                    buf_ptr = buf_ptr
                        .checked_add(entry.len() as u32)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    caller.write_bytes(buf_ptr, &[0])?;
                    buf_ptr = buf_ptr
                        .checked_add(1)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                }
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "clock_res_get" => Box::new(move |caller, args| {
                let out_ptr = arg_u32(args, 1);
                caller.write_u64_le(out_ptr, 1)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "clock_time_get" => Box::new(move |caller, args| {
                let clock_id = arg_u32(args, 0);
                let out_ptr = arg_u32(args, 2);
                let nanos: u64 = match clock_id {
                    0 => SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0),
                    1..=3 => st.lock().unwrap().start_instant.elapsed().as_nanos() as u64,
                    _ => return Ok(vec![WasmVal::I32(ERRNO_INVAL)]),
                };
                caller.write_u64_le(out_ptr, nanos)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "random_get" => Box::new(move |caller, args| {
                let buf_ptr = arg_u32(args, 0);
                let buf_len = arg_u32(args, 1) as usize;
                let mut out = vec![0u8; buf_len];
                {
                    let mut s = st.lock().unwrap();
                    let mut i = 0usize;
                    while i < buf_len {
                        let bytes = s.next_rand_u64().to_le_bytes();
                        let n = (buf_len - i).min(8);
                        out[i..i + n].copy_from_slice(&bytes[..n]);
                        i += n;
                    }
                }
                caller.write_bytes(buf_ptr, &out)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "sched_yield" => Box::new(|_caller, _args| Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])),
            "fd_fdstat_get" => Box::new(|caller, args| {
                let fd = arg_i32(args, 0);
                let stat_ptr = arg_u32(args, 1);
                if !(0..=2).contains(&fd) {
                    return Ok(vec![WasmVal::I32(ERRNO_BADF)]);
                }
                // 24-byte `fdstat`: filetype = CHARACTER_DEVICE (2), rest 0.
                let mut stat = [0u8; 24];
                stat[0] = 2;
                caller.write_bytes(stat_ptr, &stat)?;
                Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])
            }),
            "fd_prestat_get" => Box::new(|_caller, _args| Ok(vec![WasmVal::I32(ERRNO_BADF)])),
            "fd_close" => Box::new(|_caller, _args| Ok(vec![WasmVal::I32(ERRNO_SUCCESS)])),
            _ => Box::new(move |_caller, _args| match result_ty {
                Some(crate::WasmValType::I32) => Ok(vec![WasmVal::I32(ERRNO_NOSYS)]),
                Some(ty) => Ok(vec![WasmVal::default_for(ty)]),
                None => Ok(Vec::new()),
            }),
        };
        map.insert((mod_name, fn_name), cb);
    }
    Ok(map)
}

#[inline]
fn arg_i32(args: &[WasmVal], idx: usize) -> i32 {
    match args.get(idx) {
        Some(WasmVal::I32(v)) => *v,
        _ => 0,
    }
}

#[inline]
fn arg_u32(args: &[WasmVal], idx: usize) -> u32 {
    arg_i32(args, idx) as u32
}
