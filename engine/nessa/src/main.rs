//! Nessa CLI — the command-line interface for the Nessa language.

use std::path::{Path, PathBuf};
use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let command = args[1].as_str();
    match command {
        "run" => cmd_run(&args[2..]),
        "build" => cmd_build(&args[2..]),
        "check" => cmd_check(&args[2..]),
        "disasm" => cmd_disasm(&args[2..]),
        "version" | "--version" | "-V" => {
            println!("nessa {}", env!("CARGO_PKG_VERSION"));
        }
        "help" | "--help" | "-h" => print_usage(),
        _ => {
            eprintln!("unknown command: {command}");
            print_usage();
            process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage: nessa <command> [options]\n\
         \n\
         Commands:\n\
         \x20 run <file.ns|file.nsbc|directory> [-- <args...>]  Run source, package or archive\n\
         \x20 build <file.ns|directory>                         Compile to .nsbc archive\n\
         \x20 check <file.ns|directory>                         Type-check without running\n\
         \x20 disasm <file.ns|file.nsbc|directory>              Disassemble bytecode to stdout\n\
         \x20 version                                           Print version\n\
         \x20 help                                              Print this help\n\
         \n\
         Options:\n\
         \x20 -O0, -O1, -O2, -O3, -O      Set AOT optimization level (`-O` aliases `-O2`; default: `-O0`)\n\
         \x20 --opt-level <0|1|2|3>       Set AOT optimization level explicitly\n\
         \x20 --locked                    Require and verify package.lock without modifying it\n\
         \x20 --jit <auto|off|tier1|tier2> Configure tiered JIT mode for `run` (default: auto)\n\
         \x20 --emit-ast-dump             Write a source AST dump beside the input\n\
         \x20 --emit-bc-dump              Write a human-readable bytecode disassembly beside the input\n\
         \n\
         Environment:\n\
         \x20 NESSA_OPT_LEVEL    Default AOT optimization level (`0`, `1`, `2`, `3`; overridden by `-O*`)\n\
         \x20 NESSA_JIT          Default JIT mode (`auto`, `off`, `tier1`, `tier2`; overridden by `--jit`)\n\
         \x20 NESSA_CACHE_DIR    Directory for content-addressed .nsbc cache (set to 'off' to disable)\n\
         \x20 NESSA_NO_CACHE=1   Disable automatic .nsbc compilation caching"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct CompileCmdArgs<'a> {
    file: &'a str,
    locked: bool,
    emit_ast_dump: bool,
    emit_bc_dump: bool,
    opt_level: Option<driver::OptLevel>,
}

#[derive(Debug, PartialEq, Eq)]
struct RunCmdArgs<'a> {
    file: &'a str,
    locked: bool,
    emit_ast_dump: bool,
    emit_bc_dump: bool,
    jit_mode: Option<driver::JitMode>,
    opt_level: Option<driver::OptLevel>,
    /// `[target_display_string, ...trailing_args]` forwarded to `Driver::with_script_args`.
    script_args: Vec<String>,
}

fn parse_cli_opt_level(raw: &str) -> Result<driver::OptLevel, String> {
    driver::OptLevel::parse_str(raw).ok_or_else(|| {
        format!("error: invalid optimization level '{raw}' (expected 0, 1, 2, or 3)")
    })
}

fn parse_cli_jit_mode(raw: &str) -> Result<driver::JitMode, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(driver::JitMode::Auto),
        "off" => Ok(driver::JitMode::Off),
        "tier1" => Ok(driver::JitMode::Tier1),
        "tier2" => Ok(driver::JitMode::Tier2),
        _ => Err(format!(
            "error: invalid --jit mode '{raw}' (expected auto, off, tier1, or tier2)"
        )),
    }
}

fn parse_compile_args(
    args: &[String],
    allow_dump_flags: bool,
) -> Result<CompileCmdArgs<'_>, String> {
    let mut file = None;
    let mut locked = false;
    let mut emit_ast_dump = false;
    let mut emit_bc_dump = false;
    let mut opt_level = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--locked" if allow_dump_flags => locked = true,
            "--emit-ast-dump" if allow_dump_flags => emit_ast_dump = true,
            "--emit-bc-dump" if allow_dump_flags => emit_bc_dump = true,
            "-O0" if allow_dump_flags => opt_level = Some(driver::OptLevel::O0),
            "-O1" if allow_dump_flags => opt_level = Some(driver::OptLevel::O1),
            "-O2" | "-O" if allow_dump_flags => opt_level = Some(driver::OptLevel::O2),
            "-O3" if allow_dump_flags => opt_level = Some(driver::OptLevel::O3),
            "--opt-level" if allow_dump_flags => {
                let Some(raw) = iter.next() else {
                    return Err("error: --opt-level requires a level (0|1|2|3)".to_owned());
                };
                opt_level = Some(parse_cli_opt_level(raw)?);
            }
            s if allow_dump_flags && s.starts_with("--opt-level=") => {
                opt_level = Some(parse_cli_opt_level(&s["--opt-level=".len()..])?);
            }
            _ if file.is_none() && !arg.starts_with('-') => file = Some(arg.as_str()),
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let Some(file) = file else {
        return Err("error: missing input file".to_owned());
    };

    Ok(CompileCmdArgs {
        file,
        locked,
        emit_ast_dump,
        emit_bc_dump,
        opt_level,
    })
}

fn parse_run_args(args: &[String]) -> Result<RunCmdArgs<'_>, String> {
    let mut file: Option<&str> = None;
    let mut locked = false;
    let mut emit_ast_dump = false;
    let mut emit_bc_dump = false;
    let mut jit_mode: Option<driver::JitMode> = None;
    let mut opt_level: Option<driver::OptLevel> = None;
    let mut trailing_args: Vec<String> = Vec::new();
    let mut after_separator = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if after_separator {
            if file.is_none() {
                file = Some(arg.as_str());
            } else {
                trailing_args.push(arg.clone());
            }
            continue;
        }

        match arg.as_str() {
            "--" => {
                after_separator = true;
            }
            "--locked" if trailing_args.is_empty() => {
                locked = true;
            }
            "--emit-ast-dump" if trailing_args.is_empty() => {
                emit_ast_dump = true;
            }
            "--emit-bc-dump" if trailing_args.is_empty() => {
                emit_bc_dump = true;
            }
            "-O0" if trailing_args.is_empty() => {
                opt_level = Some(driver::OptLevel::O0);
            }
            "-O1" if trailing_args.is_empty() => {
                opt_level = Some(driver::OptLevel::O1);
            }
            "-O2" | "-O" if trailing_args.is_empty() => {
                opt_level = Some(driver::OptLevel::O2);
            }
            "-O3" if trailing_args.is_empty() => {
                opt_level = Some(driver::OptLevel::O3);
            }
            "--opt-level" if trailing_args.is_empty() => {
                let Some(raw) = iter.next() else {
                    return Err("error: --opt-level requires a level (0|1|2|3)".to_owned());
                };
                opt_level = Some(parse_cli_opt_level(raw)?);
            }
            s if trailing_args.is_empty() && s.starts_with("--opt-level=") => {
                opt_level = Some(parse_cli_opt_level(&s["--opt-level=".len()..])?);
            }
            "--jit" if trailing_args.is_empty() => {
                let Some(raw_mode) = iter.next() else {
                    return Err("error: --jit requires a mode (auto|off|tier1|tier2)".to_owned());
                };
                jit_mode = Some(parse_cli_jit_mode(raw_mode)?);
            }
            s if trailing_args.is_empty() && s.starts_with("--jit=") => {
                jit_mode = Some(parse_cli_jit_mode(&s["--jit=".len()..])?);
            }
            _ if !arg.starts_with('-') => {
                if file.is_none() {
                    file = Some(arg.as_str());
                } else {
                    trailing_args.push(arg.clone());
                }
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let Some(file) = file else {
        return Err("error: missing input file".to_owned());
    };

    let mut script_args = Vec::with_capacity(1 + trailing_args.len());
    script_args.push(file.to_owned());
    script_args.extend(trailing_args);

    Ok(RunCmdArgs {
        file,
        locked,
        emit_ast_dump,
        emit_bc_dump,
        jit_mode,
        opt_level,
        script_args,
    })
}

fn parse_compile_or_exit(args: &[String], allow_dump_flags: bool) -> CompileCmdArgs<'_> {
    match parse_compile_args(args, allow_dump_flags) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
    }
}

fn parse_run_or_exit(args: &[String]) -> RunCmdArgs<'_> {
    match parse_run_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
    }
}

fn read_source(file: &str) -> String {
    match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            process::exit(1);
        }
    }
}

fn bc_dump_path(input: &Path) -> PathBuf {
    if input.is_dir() {
        input.join("package.bc")
    } else {
        input.with_extension("bc")
    }
}

fn write_bc_dump_or_exit(output_path: &Path, dump: &str) {
    match std::fs::write(output_path, dump) {
        Ok(()) => {
            eprintln!("wrote {}", output_path.display());
        }
        Err(e) => {
            eprintln!("error writing bytecode dump: {e}");
            process::exit(1);
        }
    }
}

fn maybe_emit_ast_dump(file: &str, emit_ast_dump: bool, locked: bool, source: Option<&str>) {
    if !emit_ast_dump {
        return;
    }
    let input = Path::new(file);
    let output_path = match source {
        Some(_) => input.with_extension("lisp"),
        None => input.join("package.lisp"),
    };
    let driver = driver::Driver::new();
    let result = match source {
        Some(source) => driver.emit_ast_dump(source, &output_path),
        None if locked => driver.emit_package_ast_dump_locked(input, &output_path),
        None => driver.emit_package_ast_dump(input, &output_path),
    };
    match result {
        Ok(()) => {
            eprintln!("wrote {}", output_path.display());
        }
        Err(e) => {
            eprintln!("error writing AST dump: {e}");
            process::exit(1);
        }
    }
}

fn is_nsbc_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "nsbc")
}

fn reject_locked_for_non_package(locked: bool) {
    if locked {
        eprintln!("error: --locked requires a package directory");
        process::exit(1);
    }
}

fn cmd_run(args: &[String]) {
    let cmd = parse_run_or_exit(args);
    let mut driver = driver::Driver::new().with_script_args(cmd.script_args);
    driver.set_jit_mode(cmd.jit_mode);
    driver.set_opt_level(cmd.opt_level);
    let input = Path::new(cmd.file);
    let result = if input.is_dir() {
        let compiled = if cmd.locked {
            driver.compile_package_locked_cached_artifact(input)
        } else {
            driver.compile_package_cached_artifact(input)
        };
        match compiled {
            Ok(artifact) => {
                maybe_emit_ast_dump(cmd.file, cmd.emit_ast_dump, cmd.locked, None);
                if cmd.emit_bc_dump {
                    let dump = driver::disassemble_artifact(&artifact);
                    write_bc_dump_or_exit(&bc_dump_path(input), &dump);
                }
                driver.run_artifact(artifact)
            }
            Err(diagnostics) => driver::RunResult::CompileError(diagnostics),
        }
    } else if is_nsbc_file(input) {
        reject_locked_for_non_package(cmd.locked);
        if cmd.emit_ast_dump {
            eprintln!("error: --emit-ast-dump requires source input");
            process::exit(1);
        }
        if cmd.emit_bc_dump {
            match driver.disassemble_archive_file(input) {
                Ok(dump) => write_bc_dump_or_exit(&bc_dump_path(input), &dump),
                Err(error) => {
                    eprintln!("runtime error: archive load failed: {error}");
                    process::exit(1);
                }
            }
        }
        driver.run_archive_file(input)
    } else {
        reject_locked_for_non_package(cmd.locked);
        let source = read_source(cmd.file);
        match driver.compile_cached_artifact(&source) {
            Ok(artifact) => {
                maybe_emit_ast_dump(cmd.file, cmd.emit_ast_dump, false, Some(&source));
                if cmd.emit_bc_dump {
                    let dump = driver::disassemble_artifact(&artifact);
                    write_bc_dump_or_exit(&bc_dump_path(input), &dump);
                }
                driver.run_artifact(artifact)
            }
            Err(diagnostics) => driver::RunResult::CompileError(diagnostics),
        }
    };
    match result {
        driver::RunResult::Ok => {}
        driver::RunResult::CompileError(diagnostics) => {
            for diag in &diagnostics {
                eprintln!("error: {}", diag.message);
            }
            if diagnostics.is_empty() {
                eprintln!("compilation failed");
            }
            process::exit(1);
        }
        driver::RunResult::RuntimeError(e) => {
            eprintln!("runtime error: {e}");
            process::exit(1);
        }
    }
}

fn cmd_build(args: &[String]) {
    let cmd = parse_compile_or_exit(args, true);
    let input = Path::new(cmd.file);
    let mut driver = driver::Driver::new();
    driver.set_opt_level(cmd.opt_level);
    let (output_path, result) = if input.is_dir() {
        let output = input.join("package.nsbc");
        let result = if cmd.locked {
            driver.compile_package_to_archive_locked(input, &output)
        } else {
            driver.compile_package_to_archive(input, &output)
        };
        if result.is_ok() {
            maybe_emit_ast_dump(cmd.file, cmd.emit_ast_dump, cmd.locked, None);
        }
        (output, result)
    } else {
        reject_locked_for_non_package(cmd.locked);
        let source = read_source(cmd.file);
        let output = input.with_extension("nsbc");
        let result = driver.compile_to_archive(&source, &output);
        if result.is_ok() {
            maybe_emit_ast_dump(cmd.file, cmd.emit_ast_dump, false, Some(&source));
        }
        (output, result)
    };
    match result {
        Ok(()) => {
            if cmd.emit_bc_dump {
                let dump_path = bc_dump_path(input);
                match driver.emit_archive_bc_dump(&output_path, &dump_path) {
                    Ok(()) => eprintln!("wrote {}", dump_path.display()),
                    Err(e) => {
                        eprintln!("error writing bytecode dump: {e}");
                        process::exit(1);
                    }
                }
            }
            println!("wrote {}", output_path.display());
        }
        Err(e) => {
            eprintln!("build failed: {e}");
            process::exit(1);
        }
    }
}

fn cmd_check(args: &[String]) {
    let cmd = parse_compile_or_exit(args, true);
    let mut driver = driver::Driver::new();
    driver.set_opt_level(cmd.opt_level);
    let input = Path::new(cmd.file);
    let result = if input.is_dir() {
        let res = if cmd.locked {
            driver.compile_package_locked(input)
        } else {
            driver.compile_package(input)
        };
        if !res.has_errors {
            maybe_emit_ast_dump(cmd.file, cmd.emit_ast_dump, cmd.locked, None);
        }
        res
    } else {
        reject_locked_for_non_package(cmd.locked);
        let source = read_source(cmd.file);
        let res = driver.compile(&source);
        if !res.has_errors {
            maybe_emit_ast_dump(cmd.file, cmd.emit_ast_dump, false, Some(&source));
        }
        res
    };
    if result.has_errors {
        for diag in &result.diagnostics {
            eprintln!("error: {}", diag.message);
        }
        if result.diagnostics.is_empty() {
            eprintln!("check failed with errors");
        }
        process::exit(1);
    }
    if cmd.emit_bc_dump {
        match result.into_artifact() {
            Ok(artifact) => {
                let dump = driver::disassemble_artifact(&artifact);
                write_bc_dump_or_exit(&bc_dump_path(input), &dump);
            }
            Err(e) => {
                eprintln!("error writing bytecode dump: {e}");
                process::exit(1);
            }
        }
    }
    println!("ok");
}

fn cmd_disasm(args: &[String]) {
    let cmd = parse_compile_or_exit(args, true);
    if cmd.emit_ast_dump {
        eprintln!("unknown option: --emit-ast-dump");
        process::exit(1);
    }
    let mut driver = driver::Driver::new();
    driver.set_opt_level(cmd.opt_level);
    let input = Path::new(cmd.file);
    let result = if input.is_dir() {
        if cmd.locked {
            driver.disassemble_package_locked(input)
        } else {
            driver.disassemble_package(input)
        }
    } else if is_nsbc_file(input) {
        reject_locked_for_non_package(cmd.locked);
        driver.disassemble_archive_file(input)
    } else {
        reject_locked_for_non_package(cmd.locked);
        let source = read_source(cmd.file);
        driver.disassemble_source(&source)
    };
    match result {
        Ok(disassembly) => {
            if cmd.emit_bc_dump {
                write_bc_dump_or_exit(&bc_dump_path(input), &disassembly);
            }
            print!("{disassembly}");
        }
        Err(e) => {
            eprintln!("disassembly failed: {e}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn run_args_pass_double_dash_flags_verbatim_to_script() {
        let raw = strings(&["script.ns", "--", "--some-flag", "foo", "--emit-ast-dump"]);
        let parsed = parse_run_args(&raw).unwrap();
        assert_eq!(parsed.file, "script.ns");
        assert!(!parsed.emit_ast_dump);
        assert!(!parsed.emit_bc_dump);
        assert_eq!(
            parsed.script_args,
            strings(&["script.ns", "--some-flag", "foo", "--emit-ast-dump"])
        );
    }

    #[test]
    fn run_args_accept_cli_dump_flags_before_separator_and_positional_args_without_separator() {
        let raw = strings(&[
            "--emit-bc-dump",
            "pkg_dir",
            "--emit-ast-dump",
            "--",
            "a",
            "b",
        ]);
        let parsed = parse_run_args(&raw).unwrap();
        assert_eq!(parsed.file, "pkg_dir");
        assert!(parsed.emit_ast_dump);
        assert!(parsed.emit_bc_dump);
        assert_eq!(parsed.script_args, strings(&["pkg_dir", "a", "b"]));

        let positional = strings(&["app.nsbc", "arg1", "arg2"]);
        let parsed_pos = parse_run_args(&positional).unwrap();
        assert_eq!(parsed_pos.file, "app.nsbc");
        assert_eq!(
            parsed_pos.script_args,
            strings(&["app.nsbc", "arg1", "arg2"])
        );
    }

    #[test]
    fn run_args_reject_unknown_cli_flags_before_double_dash_and_missing_target() {
        assert!(parse_run_args(&strings(&["script.ns", "--unknown"])).is_err());
        assert!(parse_run_args(&strings(&[])).is_err());
        assert!(parse_run_args(&strings(&["--"])).is_err());
    }

    #[test]
    fn compile_args_support_emit_bc_dump_and_reject_trailing_arguments() {
        let raw = strings(&["src/main.ns", "--emit-bc-dump", "--emit-ast-dump"]);
        let parsed = parse_compile_args(&raw, true).unwrap();
        assert_eq!(parsed.file, "src/main.ns");
        assert!(parsed.emit_ast_dump);
        assert!(parsed.emit_bc_dump);

        assert!(parse_compile_args(&strings(&["a.ns", "extra.ns"]), true).is_err());
        assert!(parse_compile_args(&strings(&["a.nsbc", "--emit-bc-dump"]), false).is_err());
    }

    #[test]
    fn run_args_parse_jit_modes_and_preserve_script_flags_after_double_dash() {
        for (raw, expected) in [
            (
                strings(&["--jit=auto", "script.ns"]),
                Some(driver::JitMode::Auto),
            ),
            (
                strings(&["--jit=off", "script.ns"]),
                Some(driver::JitMode::Off),
            ),
            (
                strings(&["--jit", "tier1", "script.ns"]),
                Some(driver::JitMode::Tier1),
            ),
            (
                strings(&["script.ns", "--jit=tier2"]),
                Some(driver::JitMode::Tier2),
            ),
        ] {
            let parsed = parse_run_args(&raw).unwrap();
            assert_eq!(parsed.file, "script.ns");
            assert_eq!(parsed.jit_mode, expected);
            assert_eq!(parsed.script_args, strings(&["script.ns"]));
        }

        let raw_pass = strings(&["script.ns", "--", "--jit=off"]);
        let passthrough = parse_run_args(&raw_pass).unwrap();
        assert_eq!(passthrough.jit_mode, None);
        assert_eq!(
            passthrough.script_args,
            strings(&["script.ns", "--jit=off"])
        );

        assert!(parse_run_args(&strings(&["--jit=bogus", "script.ns"])).is_err());
        assert!(parse_run_args(&strings(&["--jit"])).is_err());
    }
}
