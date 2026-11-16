//! Standard error printing, buffered file handle builtins, and filesystem operations (`588..=615`).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::UNIX_EPOCH;

use interpreter::resources::{
    IO_ALREADY_EXISTS, IO_INVALID_INPUT, IO_OK, STD_STDERR, STD_STDOUT, io_error_code,
};
use interpreter::{BuiltinCtx, NativeValue, VmError};
use runtime::Number;

// ---------------------------------------------------------------------------
// 588..=591: Standard error output & stream flushing
// ---------------------------------------------------------------------------

/// `588` `eprint(v: Any) -> Unit`
pub fn io_eprint(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let root = ctx.arg_rooted(0)?;
    let val = ctx.reload_root(&root)?;
    let s = ctx.format_value(val)?;
    eprint!("{s}");
    let _ = std::io::stderr().flush();
    ctx.return_unit();
    Ok(())
}

/// `589` `eprintln(v: Any) -> Unit`
pub fn io_eprintln(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let root = ctx.arg_rooted(0)?;
    let val = ctx.reload_root(&root)?;
    let s = ctx.format_value(val)?;
    eprintln!("{s}");
    ctx.return_unit();
    Ok(())
}

/// `590` `__io_flush_stdout() -> (i64, String)`
pub fn io_flush_stdout(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let (code, msg) = ctx.file_flush(STD_STDOUT);
    return_status_pair(ctx, code, msg)
}

/// `591` `__io_flush_stderr() -> (i64, String)`
pub fn io_flush_stderr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let (code, msg) = ctx.file_flush(STD_STDERR);
    return_status_pair(ctx, code, msg)
}

// ---------------------------------------------------------------------------
// 592..=604: Buffered file handle operations
// ---------------------------------------------------------------------------

/// `592` `__file_open(path: String, mode: String) -> (i64, i64, String)`
pub fn file_open(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        let mode = ctx.arg_string(1)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, id, msg) = io.files().open(&path, &mode);
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::I64(id),
                NativeValue::Str(msg),
            ]))
        }))
    })
}

/// `593` `__file_temp() -> (i64, i64, String)`
pub fn file_temp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.run_blocking_native(|_ctx| {
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, id, msg) = io.create_temp_file();
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::I64(id),
                NativeValue::Str(msg),
            ]))
        }))
    })
}

/// `594` `__file_close(id: i64) -> (i64, String)`
pub fn file_close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, msg) = io.files().close(id);
            Ok(status_pair_value(code, msg))
        }))
    })
}

/// `595` `__file_is_open(id: i64) -> bool`
pub fn file_is_open(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.return_bool(ctx.file_is_open(id));
    Ok(())
}

/// `596` `__file_flush(id: i64) -> (i64, String)`
pub fn file_flush(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, msg) = io.files().flush(id);
            Ok(status_pair_value(code, msg))
        }))
    })
}

/// `597` `__file_read_all(id: i64) -> (i64, String)`
pub fn file_read_all(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, text) = io.files().read_all(id)?;
            Ok(status_pair_value(code, text))
        }))
    })
}

/// `598` `__file_read_line(id: i64, keep_newline: bool) -> (i64, ?String, String)`
pub fn file_read_line(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        let keep_newline = ctx.arg_bool(1)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, line, msg) = io.files().read_line(id, keep_newline)?;
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::OptStr(line),
                NativeValue::Str(msg),
            ]))
        }))
    })
}

/// `599` `__file_read_chars(id: i64, count: i64) -> (i64, ?String, String)`
pub fn file_read_chars(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        let count = ctx.arg_i64(1)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, chars, msg) = io.files().read_chars(id, count)?;
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::OptStr(chars),
                NativeValue::Str(msg),
            ]))
        }))
    })
}

/// `600` `__file_read_bytes(id: i64, max_bytes: i64) -> (i64, List, String)`
pub fn file_read_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        let max_bytes = ctx.arg_i64(1)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, raw, msg) = io.files().read_bytes(id, max_bytes)?;
            let list_items = raw
                .into_iter()
                .map(|b| NativeValue::I64(i64::from(b)))
                .collect();
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::List(list_items),
                NativeValue::Str(msg),
            ]))
        }))
    })
}

/// `601` `__file_read_number(id: i64) -> (i64, ?f64, String)`
pub fn file_read_number(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, num, msg) = io.files().read_number(id);
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::OptF64(num),
                NativeValue::Str(msg),
            ]))
        }))
    })
}

/// `602` `__file_write_str(id: i64, s: String) -> (i64, String)`
pub fn file_write_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        let s = ctx.arg_string(1)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, msg) = io.files().write_bytes(id, s.as_bytes());
            Ok(status_pair_value(code, msg))
        }))
    })
}

/// `603` `__file_write_bytes(id: i64, bytes: List) -> (i64, String)`
pub fn file_write_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        let list_root = ctx.arg_rooted(1)?;
        let list_val = ctx.reload_root(&list_root)?;
        let len = ctx.list_len(list_val)?;

        let mut buf = Vec::with_capacity(len);
        let mut out_of_range = false;
        for idx in 0..len {
            let elem_root = ctx.list_get_rooted(&list_root, idx)?;
            let elem = ctx.reload_root(&elem_root)?;
            let num = ctx.number_of(elem)?;
            let byte_opt = match num {
                Number::I64(v) => u8::try_from(v).ok(),
                Number::U64(v) => u8::try_from(v).ok(),
                Number::I128(v) => u8::try_from(v).ok(),
                Number::U128(v) => u8::try_from(v).ok(),
                Number::F64(_) => return Err(VmError::TypeError),
            };
            if let Some(b) = byte_opt {
                if !out_of_range {
                    buf.push(b);
                }
            } else {
                out_of_range = true;
            }
        }
        if out_of_range {
            return_status_pair(ctx, IO_INVALID_INPUT, "byte out of range".to_string())?;
            return Ok(None);
        }

        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, msg) = io.files().write_bytes(id, &buf);
            Ok(status_pair_value(code, msg))
        }))
    })
}

/// `604` `__file_seek(id: i64, whence: i64, offset: i64) -> (i64, i64, String)`
pub fn file_seek(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.run_blocking_native(|ctx| {
        let id = ctx.arg_i64(0)?;
        let whence = ctx.arg_i64(1)?;
        let offset = ctx.arg_i64(2)?;
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let (code, pos, msg) = io.files().seek(id, whence, offset);
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::I64(pos),
                NativeValue::Str(msg),
            ]))
        }))
    })
}

// ---------------------------------------------------------------------------
// 605..=615: Filesystem queries & directory/file management
// ---------------------------------------------------------------------------

/// `605` `__fs_exists(path: String) -> bool`
pub fn fs_exists(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            Ok(NativeValue::Bool(Path::new(&path).exists()))
        }))
    })
}

/// `606` `__fs_is_file(path: String) -> bool`
pub fn fs_is_file(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            Ok(NativeValue::Bool(Path::new(&path).is_file()))
        }))
    })
}

/// `607` `__fs_is_dir(path: String) -> bool`
pub fn fs_is_dir(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            Ok(NativeValue::Bool(Path::new(&path).is_dir()))
        }))
    })
}

/// `608` `__fs_metadata(path: String) -> (i64, i64, bool, bool, bool, i64, String)`
pub fn fs_metadata(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            let lstat = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(err) => {
                    return Ok(NativeValue::Tuple(vec![
                        NativeValue::I64(io_error_code(&err)),
                        NativeValue::I64(0),
                        NativeValue::Bool(false),
                        NativeValue::Bool(false),
                        NativeValue::Bool(false),
                        NativeValue::I64(0),
                        NativeValue::Str(err.to_string()),
                    ]));
                }
            };
            let is_symlink = lstat.file_type().is_symlink();
            let stat = fs::metadata(&path).unwrap_or(lstat);
            let size = i64::try_from(stat.len()).unwrap_or(i64::MAX);
            let is_file = stat.is_file();
            let is_dir = stat.is_dir();
            let mtime_secs = match stat.modified() {
                Ok(sys_time) => match sys_time.duration_since(UNIX_EPOCH) {
                    Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                    Err(neg) => -(i64::try_from(neg.duration().as_secs()).unwrap_or(i64::MAX)),
                },
                Err(_) => 0,
            };

            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(IO_OK),
                NativeValue::I64(size),
                NativeValue::Bool(is_file),
                NativeValue::Bool(is_dir),
                NativeValue::Bool(is_symlink),
                NativeValue::I64(mtime_secs),
                NativeValue::Str(String::new()),
            ]))
        }))
    })
}

/// `609` `__fs_create_dir(path: String, recursive: bool) -> (i64, String)`
pub fn fs_create_dir(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        let recursive = ctx.arg_bool(1)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            let res = if recursive {
                fs::create_dir_all(&path)
            } else {
                fs::create_dir(&path)
            };
            match res {
                Ok(()) => Ok(status_pair_value(IO_OK, String::new())),
                Err(err) => Ok(status_pair_value(io_error_code(&err), err.to_string())),
            }
        }))
    })
}

/// `610` `__fs_remove(path: String, recursive: bool) -> (i64, String)`
pub fn fs_remove(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        let recursive = ctx.arg_bool(1)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            let lstat = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(err) => return Ok(status_pair_value(io_error_code(&err), err.to_string())),
            };

            let res = if lstat.file_type().is_symlink() || !lstat.is_dir() {
                fs::remove_file(&path)
            } else if recursive {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_dir(&path)
            };

            match res {
                Ok(()) => Ok(status_pair_value(IO_OK, String::new())),
                Err(err) => Ok(status_pair_value(io_error_code(&err), err.to_string())),
            }
        }))
    })
}

/// `611` `__fs_rename(from: String, to: String) -> (i64, String)`
pub fn fs_rename(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let from = ctx.arg_string(0)?;
        let to = ctx.arg_string(1)?;
        Ok(Some(
            move |_io: &interpreter::VmIoContext| match fs::rename(&from, &to) {
                Ok(()) => Ok(status_pair_value(IO_OK, String::new())),
                Err(err) => Ok(status_pair_value(io_error_code(&err), err.to_string())),
            },
        ))
    })
}

/// `612` `__fs_copy(from: String, to: String) -> (i64, i64, String)`
pub fn fs_copy(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_blocking_native(|ctx| {
        let from = ctx.arg_string(0)?;
        let to = ctx.arg_string(1)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            match fs::copy(&from, &to) {
                Ok(bytes) => Ok(NativeValue::Tuple(vec![
                    NativeValue::I64(IO_OK),
                    NativeValue::I64(i64::try_from(bytes).unwrap_or(i64::MAX)),
                    NativeValue::Str(String::new()),
                ])),
                Err(err) => Ok(NativeValue::Tuple(vec![
                    NativeValue::I64(io_error_code(&err)),
                    NativeValue::I64(0),
                    NativeValue::Str(err.to_string()),
                ])),
            }
        }))
    })
}

/// `613` `__fs_read_dir(path: String) -> (i64, List, String)`
pub fn fs_read_dir(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            let entries = match fs::read_dir(&path) {
                Ok(it) => it,
                Err(err) => {
                    return Ok(NativeValue::Tuple(vec![
                        NativeValue::I64(io_error_code(&err)),
                        NativeValue::List(Vec::new()),
                        NativeValue::Str(err.to_string()),
                    ]));
                }
            };

            let mut names = Vec::new();
            for entry_res in entries {
                match entry_res {
                    Ok(entry) => names.push(entry.file_name().to_string_lossy().into_owned()),
                    Err(err) => {
                        return Ok(NativeValue::Tuple(vec![
                            NativeValue::I64(io_error_code(&err)),
                            NativeValue::List(Vec::new()),
                            NativeValue::Str(err.to_string()),
                        ]));
                    }
                }
            }
            names.sort();
            let list_items = names.into_iter().map(NativeValue::Str).collect();
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(IO_OK),
                NativeValue::List(list_items),
                NativeValue::Str(String::new()),
            ]))
        }))
    })
}

/// `644` `__fs_read_dir_entries(path: String) -> (i64, List, String)`
/// Each item in the returned `List` is a 7-element `List`:
/// `[name, full_path, is_file, is_dir, is_symlink, size, mtime_secs]`.
pub fn fs_read_dir_entries(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            let entries = match fs::read_dir(&path) {
                Ok(it) => it,
                Err(err) => {
                    return Ok(NativeValue::Tuple(vec![
                        NativeValue::I64(io_error_code(&err)),
                        NativeValue::List(Vec::new()),
                        NativeValue::Str(err.to_string()),
                    ]));
                }
            };

            struct RawEntry {
                name: String,
                full_path: String,
                is_file: bool,
                is_dir: bool,
                is_symlink: bool,
                size: i64,
                mtime_secs: i64,
            }

            let mut raw_entries = Vec::new();
            for entry_res in entries {
                let entry = match entry_res {
                    Ok(e) => e,
                    Err(err) => {
                        return Ok(NativeValue::Tuple(vec![
                            NativeValue::I64(io_error_code(&err)),
                            NativeValue::List(Vec::new()),
                            NativeValue::Str(err.to_string()),
                        ]));
                    }
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                let entry_path = entry.path();
                let full_path = entry_path.to_string_lossy().into_owned();
                let is_symlink = entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);
                let (is_file, is_dir, size, mtime_secs) = match fs::metadata(&entry_path) {
                    Ok(stat) => {
                        let sz = i64::try_from(stat.len()).unwrap_or(i64::MAX);
                        let mt = stat
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
                            .unwrap_or(0);
                        (stat.is_file(), stat.is_dir(), sz, mt)
                    }
                    Err(_) => (false, false, 0, 0),
                };
                raw_entries.push(RawEntry {
                    name,
                    full_path,
                    is_file,
                    is_dir,
                    is_symlink,
                    size,
                    mtime_secs,
                });
            }

            raw_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
            let list_items = raw_entries
                .into_iter()
                .map(|e| {
                    NativeValue::List(vec![
                        NativeValue::Str(e.name),
                        NativeValue::Str(e.full_path),
                        NativeValue::Bool(e.is_file),
                        NativeValue::Bool(e.is_dir),
                        NativeValue::Bool(e.is_symlink),
                        NativeValue::I64(e.size),
                        NativeValue::I64(e.mtime_secs),
                    ])
                })
                .collect();
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(IO_OK),
                NativeValue::List(list_items),
                NativeValue::Str(String::new()),
            ]))
        }))
    })
}

/// `645` `__fs_walk_dir(root: String, max_depth: i64, include_hidden: bool) -> (i64, List, String)`
pub fn fs_walk_dir(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.run_blocking_native(|ctx| {
        let root_str = ctx.arg_string(0)?;
        let max_depth = ctx.arg_i64(1)?;
        let include_hidden = ctx.arg_bool(2)?;
        Ok(Some(move |_io: &interpreter::VmIoContext| {
            let root_path = Path::new(&root_str);
            if !root_path.exists() {
                let err = std::io::Error::new(std::io::ErrorKind::NotFound, "directory not found");
                return Ok(NativeValue::Tuple(vec![
                    NativeValue::I64(io_error_code(&err)),
                    NativeValue::List(Vec::new()),
                    NativeValue::Str(err.to_string()),
                ]));
            }
            let depth_limit = if max_depth < 0 {
                usize::MAX
            } else {
                usize::try_from(max_depth).unwrap_or(usize::MAX)
            };

            let mut collected = Vec::new();
            let mut stack: Vec<(std::path::PathBuf, String, usize)> =
                vec![(root_path.to_path_buf(), String::new(), 0)];
            const MAX_WALK_FILES: usize = 50_000;

            while let Some((dir_path, rel_prefix, depth)) = stack.pop() {
                if depth > depth_limit || collected.len() >= MAX_WALK_FILES {
                    continue;
                }
                let Ok(iter) = fs::read_dir(&dir_path) else {
                    continue;
                };
                let mut subdirs = Vec::new();
                for entry_res in iter {
                    let Ok(entry) = entry_res else {
                        continue;
                    };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !include_hidden && name.starts_with('.') {
                        continue;
                    }
                    let Ok(ft) = entry.file_type() else {
                        continue;
                    };
                    if ft.is_symlink() {
                        continue;
                    }
                    let rel = if rel_prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{rel_prefix}/{name}")
                    };
                    if ft.is_dir() {
                        if depth < depth_limit {
                            subdirs.push((entry.path(), rel, depth + 1));
                        }
                    } else if ft.is_file() {
                        collected.push(rel);
                        if collected.len() >= MAX_WALK_FILES {
                            break;
                        }
                    }
                }
                stack.extend(subdirs);
            }

            collected.sort();
            let list_items = collected.into_iter().map(NativeValue::Str).collect();
            Ok(NativeValue::Tuple(vec![
                NativeValue::I64(IO_OK),
                NativeValue::List(list_items),
                NativeValue::Str(String::new()),
            ]))
        }))
    })
}

/// `614` `__fs_canonicalize(path: String) -> (i64, String)`
pub fn fs_canonicalize(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let path = ctx.arg_string(0)?;
        Ok(Some(
            move |_io: &interpreter::VmIoContext| match fs::canonicalize(&path) {
                Ok(canon) => Ok(status_pair_value(
                    IO_OK,
                    canon.to_string_lossy().into_owned(),
                )),
                Err(err) => Ok(status_pair_value(io_error_code(&err), err.to_string())),
            },
        ))
    })
}

/// `615` `__fs_temp_path(prefix: String) -> (i64, String)`
pub fn fs_temp_path(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_blocking_native(|ctx| {
        let prefix = ctx.arg_string(0)?;
        if prefix.contains(['/', '\\', '\0']) || prefix == "." || prefix == ".." {
            return_status_pair(
                ctx,
                IO_INVALID_INPUT,
                "temporary file prefix must not contain path separators".to_string(),
            )?;
            return Ok(None);
        }
        Ok(Some(move |io: &interpreter::VmIoContext| {
            let clean_prefix = if prefix.is_empty() { "nessa_" } else { &prefix };
            let dir = std::env::temp_dir();

            for _ in 0..64 {
                let rand_val = io.rng_next_u64();
                let candidate = dir.join(format!(
                    "{clean_prefix}{}_{:016x}",
                    std::process::id(),
                    rand_val
                ));
                let mut opts = OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                match opts.open(&candidate) {
                    Ok(_file) => {
                        return Ok(status_pair_value(
                            IO_OK,
                            candidate.to_string_lossy().into_owned(),
                        ));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => return Ok(status_pair_value(io_error_code(&err), err.to_string())),
                }
            }

            Ok(status_pair_value(
                IO_ALREADY_EXISTS,
                "failed to create unique temporary file".to_string(),
            ))
        }))
    })
}

fn status_pair_value(code: i64, text: String) -> NativeValue {
    NativeValue::Tuple(vec![NativeValue::I64(code), NativeValue::Str(text)])
}

fn return_status_pair(ctx: &mut BuiltinCtx<'_>, code: i64, text: String) -> Result<(), VmError> {
    ctx.return_native(status_pair_value(code, text))
}
