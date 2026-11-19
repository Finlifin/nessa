//! CLI entrypoint for `nessa-lsp` and in-process `nessa lsp`.
//! Stdout is reserved strictly for JSON-RPC frames in `--stdio` server mode.

use std::ffi::OsString;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use crate::analysis::{ServerConfig, WorkspaceAnalyzer, lsp_severity};
use crate::server::{LanguageServer, NESSA_LSP_VERSION};

pub fn help_text() -> String {
    format!(
        "\
Usage: nessa-lsp [options]
Language Server for the Nessa programming language (nessa-lsp {NESSA_LSP_VERSION})

Options:
  --stdio             Communicate over standard input/output (default)
  --sysroot <DIR>     Explicit Nessa standard library directory
  --check <PATH>      Batch-check a .ns source file or package directory
  -h, --help          Print help information
  -V, --version       Print version information
"
    )
}

#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    Help,
    Version,
    Serve {
        sysroot: Option<PathBuf>,
    },
    Check {
        sysroot: Option<PathBuf>,
        target: PathBuf,
    },
}

fn parse_cli_args<I, S>(args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut sysroot: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut stdio = false;
    let mut iter = args.into_iter().map(Into::into);

    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-V" | "--version") => return Ok(CliAction::Version),
            Some("--stdio") => {
                if stdio {
                    return Err("--stdio may only be specified once".into());
                }
                stdio = true;
            }
            Some(flag @ ("--sysroot" | "--check")) => {
                let val = iter
                    .next()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("{flag} requires a path argument"))?;
                let slot = if flag == "--sysroot" {
                    &mut sysroot
                } else {
                    &mut target
                };
                if slot.replace(PathBuf::from(val)).is_some() {
                    return Err(format!("{flag} may only be specified once"));
                }
            }
            Some(s) if s.starts_with("--sysroot=") => {
                let val = &s["--sysroot=".len()..];
                if val.is_empty() {
                    return Err("--sysroot requires a non-empty path".into());
                }
                if sysroot.replace(PathBuf::from(val)).is_some() {
                    return Err("--sysroot may only be specified once".into());
                }
            }
            Some(s) if s.starts_with("--check=") => {
                let val = &s["--check=".len()..];
                if val.is_empty() {
                    return Err("--check requires a non-empty path".into());
                }
                if target.replace(PathBuf::from(val)).is_some() {
                    return Err("--check may only be specified once".into());
                }
            }
            _ => {
                return Err(format!(
                    "unrecognized option {:?} (try `nessa-lsp --help`)",
                    arg
                ));
            }
        }
    }

    match target {
        Some(_) if stdio => Err("--check cannot be combined with --stdio".into()),
        Some(target) => Ok(CliAction::Check { sysroot, target }),
        None => Ok(CliAction::Serve { sysroot }),
    }
}

/// Run `nessa-lsp` CLI with default [`ServerConfig`], returning the process exit code (`0`, `1`, or `2`).
pub fn run_cli<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    run_cli_with_config(args, ServerConfig::default())
}

/// Run `nessa-lsp` CLI with caller-supplied [`ServerConfig`] (including custom formatter hooks).
pub fn run_cli_with_config<I, S>(args: I, mut config: ServerConfig) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let action = match parse_cli_args(args) {
        Ok(action) => action,
        Err(err) => {
            eprintln!("nessa-lsp: {err}");
            return 2;
        }
    };

    match action {
        CliAction::Help => {
            print!("{}", help_text());
            0
        }
        CliAction::Version => {
            println!("nessa-lsp {NESSA_LSP_VERSION}");
            0
        }
        CliAction::Check { sysroot, target } => {
            if sysroot.is_some() {
                config.sysroot_override = sysroot;
            }
            run_batch_check(config, &target)
        }
        CliAction::Serve { sysroot } => {
            if sysroot.is_some() {
                config.sysroot_override = sysroot;
            }
            let stdout = io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            match LanguageServer::new(config).run_stdio(&mut writer) {
                Ok(code) => code,
                Err(err) => {
                    eprintln!("nessa-lsp: fatal I/O error: {err}");
                    1
                }
            }
        }
    }
}

fn run_batch_check(config: ServerConfig, target: &Path) -> i32 {
    if let Err(err) = target.metadata() {
        eprintln!("nessa-lsp: cannot access {}: {err}", target.display());
        return 1;
    }
    let mut analyzer = WorkspaceAnalyzer::new(config);
    let snapshot = analyzer.ensure_snapshot(target);
    let mut has_errors = snapshot.has_errors;

    let mut files: Vec<&PathBuf> = snapshot.diagnostics.keys().collect();
    files.sort();
    for file in files {
        for diag in &snapshot.diagnostics[file] {
            let level_label = match diag.severity {
                lsp_severity::ERROR => {
                    has_errors = true;
                    "error"
                }
                lsp_severity::WARNING => "warning",
                lsp_severity::INFORMATION => "note",
                _ => "help",
            };
            let code_suffix = diag
                .code
                .as_ref()
                .map(|c| format!("[{c}]"))
                .unwrap_or_default();
            eprintln!(
                "{}:{}:{}: {}{}: {}",
                file.display(),
                diag.range.start.line + 1,
                diag.range.start.character + 1,
                level_label,
                code_suffix,
                diag.message
            );
        }
    }

    if has_errors { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_argument_parsing_enforces_flags_and_modes() {
        assert_eq!(
            parse_cli_args(Vec::<&str>::new()).unwrap(),
            CliAction::Serve { sysroot: None }
        );
        assert_eq!(
            parse_cli_args(["--stdio", "--sysroot", "library"]).unwrap(),
            CliAction::Serve {
                sysroot: Some(PathBuf::from("library"))
            }
        );
        assert_eq!(
            parse_cli_args(["--check", "main.ns", "--sysroot=library"]).unwrap(),
            CliAction::Check {
                sysroot: Some(PathBuf::from("library")),
                target: PathBuf::from("main.ns"),
            }
        );
        assert_eq!(parse_cli_args(["--help"]).unwrap(), CliAction::Help);
        assert_eq!(parse_cli_args(["-V"]).unwrap(), CliAction::Version);

        for invalid in [
            vec!["--unknown"],
            vec!["--check"],
            vec!["--sysroot"],
            vec!["--sysroot="],
            vec!["--check="],
            vec!["--stdio", "--check", "main.ns"],
            vec!["--stdio", "--stdio"],
        ] {
            assert!(parse_cli_args(invalid.clone()).is_err(), "{invalid:?}");
        }
    }
}
