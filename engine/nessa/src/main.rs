//! Nessa CLI — the command-line interface for the Nessa language.

use std::path::Path;
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
         \x20 run <file.ns|file.nsbc|directory>  Run source, package or archive\n\
         \x20 build <file.ns|directory>  Compile to .nsbc archive\n\
         \x20 check <file.ns|directory>  Type-check without running\n\
         \x20 version            Print version\n\
         \x20 help               Print this help\n\
         \n\
         Options:\n\
         \x20 --emit-ast-dump    Write a source AST dump beside the input"
    );
}

/// Extract the input file path and flags without assuming a text input.
struct CmdArgs<'a> {
    file: &'a str,
    emit_ast_dump: bool,
}

fn parse_cmd_args(args: &[String]) -> CmdArgs<'_> {
    let mut file = None;
    let mut emit_ast_dump = false;

    for arg in args {
        match arg.as_str() {
            "--emit-ast-dump" => emit_ast_dump = true,
            _ if file.is_none() && !arg.starts_with('-') => file = Some(arg.as_str()),
            other => {
                eprintln!("unknown option: {other}");
                process::exit(1);
            }
        }
    }

    let Some(file) = file else {
        eprintln!("error: missing input file");
        process::exit(1);
    };

    CmdArgs {
        file,
        emit_ast_dump,
    }
}

fn read_source(cmd: &CmdArgs<'_>) -> String {
    match std::fs::read_to_string(cmd.file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", cmd.file);
            process::exit(1);
        }
    }
}

fn maybe_emit_ast_dump(cmd: &CmdArgs<'_>, source: Option<&str>) {
    if !cmd.emit_ast_dump {
        return;
    }
    let input = Path::new(cmd.file);
    let output_path = match source {
        Some(_) => input.with_extension("lisp"),
        None => input.join("package.lisp"),
    };
    let driver = driver::Driver::new();
    let result = match source {
        Some(source) => driver.emit_ast_dump(source, &output_path),
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

fn cmd_run(args: &[String]) {
    let cmd = parse_cmd_args(args);
    let driver = driver::Driver::new();
    let input = Path::new(cmd.file);
    let result = if input.is_dir() {
        maybe_emit_ast_dump(&cmd, None);
        driver.run_package(input)
    } else if input
        .extension()
        .is_some_and(|extension| extension == "nsbc")
    {
        if cmd.emit_ast_dump {
            eprintln!("error: --emit-ast-dump requires source input");
            process::exit(1);
        }
        driver.run_archive_file(Path::new(cmd.file))
    } else {
        let source = read_source(&cmd);
        maybe_emit_ast_dump(&cmd, Some(&source));
        driver.run(&source)
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
    let cmd = parse_cmd_args(args);
    let input = Path::new(cmd.file);
    let driver = driver::Driver::new();
    let (output_path, result) = if input.is_dir() {
        maybe_emit_ast_dump(&cmd, None);
        let output = input.join("package.nsbc");
        let result = driver.compile_package_to_archive(input, &output);
        (output, result)
    } else {
        let source = read_source(&cmd);
        maybe_emit_ast_dump(&cmd, Some(&source));
        let output = input.with_extension("nsbc");
        let result = driver.compile_to_archive(&source, &output);
        (output, result)
    };
    match result {
        Ok(()) => {
            println!("wrote {}", output_path.display());
        }
        Err(e) => {
            eprintln!("build failed: {e}");
            process::exit(1);
        }
    }
}

fn cmd_check(args: &[String]) {
    let cmd = parse_cmd_args(args);
    let driver = driver::Driver::new();
    let input = Path::new(cmd.file);
    let result = if input.is_dir() {
        maybe_emit_ast_dump(&cmd, None);
        driver.compile_package(input)
    } else {
        let source = read_source(&cmd);
        maybe_emit_ast_dump(&cmd, Some(&source));
        driver.compile(&source)
    };
    if result.has_errors {
        eprintln!("check failed with errors");
        process::exit(1);
    } else {
        println!("ok");
    }
}
