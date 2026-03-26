//! Nessa CLI — the command-line interface for the Nessa language.

use std::path::PathBuf;
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
         \x20 run <file.ns>      Compile and run a source file\n\
         \x20 build <file.ns>    Compile to .nsbc archive\n\
         \x20 check <file.ns>    Type-check without running\n\
         \x20 version            Print version\n\
         \x20 help               Print this help\n\
         \n\
         Options:\n\
         \x20 --emit-ast-dump    Write AST dump to ast.lisp"
    );
}

/// Extract the source file path and flags from command arguments.
struct CmdArgs<'a> {
    file: &'a str,
    source: String,
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
        eprintln!("error: missing source file");
        process::exit(1);
    };

    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            process::exit(1);
        }
    };

    CmdArgs {
        file,
        source,
        emit_ast_dump,
    }
}

fn maybe_emit_ast_dump(cmd: &CmdArgs<'_>) {
    if !cmd.emit_ast_dump {
        return;
    }
    let output_path = PathBuf::from(cmd.file).with_extension("lisp");
    let driver = driver::Driver::new();
    match driver.emit_ast_dump(&cmd.source, &output_path) {
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
    maybe_emit_ast_dump(&cmd);

    let driver = driver::Driver::new();
    match driver.run(&cmd.source) {
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
    maybe_emit_ast_dump(&cmd);

    let output_path = PathBuf::from(cmd.file).with_extension("nsbc");
    let driver = driver::Driver::new();
    match driver.compile_to_archive(&cmd.source, &output_path) {
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
    maybe_emit_ast_dump(&cmd);

    let driver = driver::Driver::new();
    let result = driver.compile(&cmd.source);
    if result.has_errors {
        eprintln!("check failed with errors");
        process::exit(1);
    } else {
        println!("ok");
    }
}
