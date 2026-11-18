//! The `nessa-fmt` CLI driver, shared between the standalone binary, `nessa fmt`, and library tests.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use crate::config::{CONFIG_FILE_NAME, ConfigError, FormatConfig};
use crate::diff::render_check_diff;
use crate::formatter::format_source_internal;

pub const EXIT_OK: i32 = 0;
pub const EXIT_DIFF_OR_FORMAT_ERR: i32 = 1;
pub const EXIT_CLI_OR_CONFIG_ERR: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitMode {
    Files,
    Stdout,
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintConfigMode {
    Default,
    Minimal,
    Current,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    fn enabled(self, stream_is_tty: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => stream_is_tty && std::env::var_os("NO_COLOR").is_none(),
        }
    }
}

#[derive(Debug, Clone)]
struct CliOptions {
    check_flag: bool,
    emit: Option<EmitMode>,
    backup: bool,
    files_with_diff: bool,
    stdin_filepath: Option<PathBuf>,
    config_path: Option<PathBuf>,
    config_overrides: Vec<String>,
    print_config: Option<PrintConfigMode>,
    color: ColorChoice,
    verbose: bool,
    quiet: bool,
    help: bool,
    version: bool,
    inputs: Vec<String>,
}

impl Default for CliOptions {
    fn default() -> Self {
        Self {
            check_flag: false,
            emit: None,
            backup: false,
            files_with_diff: false,
            stdin_filepath: None,
            config_path: None,
            config_overrides: Vec::new(),
            print_config: None,
            color: ColorChoice::Auto,
            verbose: false,
            quiet: false,
            help: false,
            version: false,
            inputs: Vec::new(),
        }
    }
}

/// Entry point using standard process stdio streams.
/// Returns exit code: `0` clean, `1` diff/parse/io/verify error, `2` bad CLI/config.
pub fn run_cli<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut stdin = io::stdin().lock();
    let stdout_tty = io::stdout().is_terminal();
    let stderr_tty = io::stderr().is_terminal();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_cli_with_io(
        args,
        &mut stdin,
        &mut stdout,
        &mut stderr,
        stdout_tty,
        stderr_tty,
    )
}

/// Testable CLI driver accepting explicit `Read` / `Write` handles and TTY flags.
pub fn run_cli_with_io<I, S>(
    args: I,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    stdout_is_tty: bool,
    stderr_is_tty: bool,
) -> i32
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut raw_args: Vec<String> = args
        .into_iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();

    // Strip leading binary name if `argv[0]` (`nessa-fmt` / `.../nessa-fmt[.exe]`) was included.
    if let Some(first) = raw_args.first()
        && is_program_name(first)
    {
        raw_args.remove(0);
    }

    let opts = match parse_cli_options(&raw_args) {
        Ok(o) => o,
        Err(msg) => {
            let _ = writeln!(stderr, "error: {msg}");
            let _ = writeln!(stderr, "For more information, try 'nessa-fmt --help'.");
            return EXIT_CLI_OR_CONFIG_ERR;
        }
    };

    if opts.help {
        let _ = write!(stdout, "{}", render_help());
        return EXIT_OK;
    }

    if opts.version {
        let _ = writeln!(stdout, "nessa-fmt {}", env!("CARGO_PKG_VERSION"));
        return EXIT_OK;
    }

    let effective_emit = match (opts.check_flag, opts.emit) {
        (true, Some(EmitMode::Files | EmitMode::Stdout)) => {
            let _ = writeln!(
                stderr,
                "error: `--check` cannot be combined with `--emit files` or `--emit stdout`"
            );
            return EXIT_CLI_OR_CONFIG_ERR;
        }
        (true, _) | (false, Some(EmitMode::Check)) => EmitMode::Check,
        (false, Some(mode)) => mode,
        (false, None) => EmitMode::Files,
    };

    // Validate `--config` overrides upfront so malformed overrides fail with exit code 2 immediately.
    let mut probe_cfg = FormatConfig::default();
    for ov in &opts.config_overrides {
        if let Err(err) = probe_cfg.apply_cli_overrides(ov) {
            let _ = writeln!(stderr, "error: {err}");
            return EXIT_CLI_OR_CONFIG_ERR;
        }
    }

    // Handle `--print-config <default|minimal|current>`.
    if let Some(pc_mode) = opts.print_config {
        match pc_mode {
            PrintConfigMode::Default => {
                let text = FormatConfig::default().to_toml_string(false);
                let _ = write!(stdout, "{text}");
                return EXIT_OK;
            }
            PrintConfigMode::Minimal | PrintConfigMode::Current => {
                let anchor_dir = resolve_anchor_dir(&opts);
                let mut cfg = match FormatConfig::discover(&anchor_dir, opts.config_path.as_deref())
                {
                    Ok(c) => c,
                    Err(err) => {
                        let _ = writeln!(stderr, "error: {err}");
                        return EXIT_CLI_OR_CONFIG_ERR;
                    }
                };
                for ov in &opts.config_overrides {
                    if let Err(err) = cfg.apply_cli_overrides(ov) {
                        let _ = writeln!(stderr, "error: {err}");
                        return EXIT_CLI_OR_CONFIG_ERR;
                    }
                }
                let text = cfg.to_toml_string(pc_mode == PrintConfigMode::Minimal);
                let _ = write!(stdout, "{text}");
                return EXIT_OK;
            }
        }
    }

    let has_dash = opts.inputs.iter().any(|s| s == "-");
    if has_dash && opts.inputs.len() > 1 {
        let _ = writeln!(
            stderr,
            "error: cannot mix `-` (stdin) with file or directory paths"
        );
        return EXIT_CLI_OR_CONFIG_ERR;
    }

    let stdin_mode = has_dash;

    if !stdin_mode && opts.stdin_filepath.is_some() {
        let _ = writeln!(
            stderr,
            "error: `--stdin-filepath` can only be used when reading from stdin (`-`)"
        );
        return EXIT_CLI_OR_CONFIG_ERR;
    }

    let stdout_color = opts.color.enabled(stdout_is_tty);
    let stderr_color = opts.color.enabled(stderr_is_tty);

    let pinned_config = if let Some(explicit_cfg) = opts.config_path.as_deref() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut cfg = match FormatConfig::discover(&cwd, Some(explicit_cfg)) {
            Ok(c) => c,
            Err(err) => {
                let _ = writeln!(stderr, "error: {err}");
                return EXIT_CLI_OR_CONFIG_ERR;
            }
        };
        for ov in &opts.config_overrides {
            if let Err(err) = cfg.apply_cli_overrides(ov) {
                let _ = writeln!(stderr, "error: {err}");
                return EXIT_CLI_OR_CONFIG_ERR;
            }
        }
        Some(cfg)
    } else {
        None
    };

    if stdin_mode {
        return run_stdin_mode(
            &opts,
            effective_emit,
            pinned_config,
            stdin,
            stdout,
            stderr,
            stdout_color,
            stderr_color,
        );
    }

    let raw_paths: Vec<PathBuf> = if opts.inputs.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        opts.inputs.iter().map(PathBuf::from).collect()
    };

    let mut config_cache = ConfigResolver {
        pinned: pinned_config,
        overrides: opts.config_overrides.clone(),
        by_dir: HashMap::new(),
    };

    let mut targets: Vec<(PathBuf, FormatConfig)> = Vec::new();
    let mut had_io_or_fmt_error = false;

    for root_input in &raw_paths {
        if !root_input.exists() {
            let _ = writeln!(
                stderr,
                "error: failed to find input path `{}`",
                root_input.display()
            );
            had_io_or_fmt_error = true;
            continue;
        }
        if root_input.is_file() {
            let parent = parent_or_cwd(root_input);
            let cfg = match config_cache.resolve(&parent) {
                Ok(c) => c,
                Err(err) => {
                    let _ = writeln!(stderr, "error: {err}");
                    return EXIT_CLI_OR_CONFIG_ERR;
                }
            };
            if cfg.is_ignored(root_input, &parent) {
                if opts.verbose && !opts.quiet {
                    let _ = writeln!(stderr, "Ignoring `{}`", root_input.display());
                }
                continue;
            }
            targets.push((root_input.clone(), cfg));
        } else if root_input.is_dir()
            && let Err(cfg_err) = collect_directory_targets(
                root_input,
                root_input,
                true,
                &opts,
                &mut config_cache,
                &mut targets,
                stderr,
                &mut had_io_or_fmt_error,
            )
        {
            let _ = writeln!(stderr, "error: {cfg_err}");
            return EXIT_CLI_OR_CONFIG_ERR;
        }
    }

    let multi_stdout_headers = effective_emit == EmitMode::Stdout && targets.len() > 1;
    let mut any_diff_in_check = false;

    for (idx, (path, cfg)) in targets.iter().enumerate() {
        let display_name = path.display().to_string();
        if opts.verbose && !opts.quiet {
            let _ = writeln!(stderr, "Formatting `{display_name}`");
        }

        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => {
                let _ = writeln!(stderr, "error: failed to read `{display_name}`: {err}");
                had_io_or_fmt_error = true;
                continue;
            }
        };

        let result = match format_source_internal(&src, &display_name, cfg, stderr_color) {
            Ok(r) => r,
            Err(err) => {
                let _ = writeln!(stderr, "error: {err}");
                had_io_or_fmt_error = true;
                continue;
            }
        };

        if !opts.quiet {
            for warn in &result.warnings {
                let _ = writeln!(stderr, "warning in `{display_name}`: {warn}");
            }
        }

        let changed = result.changed;

        match effective_emit {
            EmitMode::Check => {
                if changed {
                    any_diff_in_check = true;
                    if opts.files_with_diff {
                        if !opts.quiet {
                            let _ = writeln!(stdout, "{display_name}");
                        }
                    } else if !opts.quiet {
                        let diff_text =
                            render_check_diff(&display_name, &src, &result.formatted, stdout_color);
                        let _ = write!(stdout, "{diff_text}");
                    }
                }
            }
            EmitMode::Stdout => {
                if opts.files_with_diff {
                    if changed && !opts.quiet {
                        let _ = writeln!(stdout, "{display_name}");
                    }
                } else {
                    if multi_stdout_headers {
                        if idx > 0 {
                            let _ = writeln!(stdout);
                        }
                        let _ = writeln!(stdout, "{display_name}:\n");
                    }
                    let _ = write!(stdout, "{}", result.formatted);
                }
            }
            EmitMode::Files => {
                if changed {
                    if opts.backup {
                        let mut bk_os = path.as_os_str().to_os_string();
                        bk_os.push(".bk");
                        let bk_path = PathBuf::from(bk_os);
                        if let Err(err) = fs::write(&bk_path, &src) {
                            let _ = writeln!(
                                stderr,
                                "error: failed to write backup `{}`: {err}",
                                bk_path.display()
                            );
                            had_io_or_fmt_error = true;
                            continue;
                        }
                    }
                    if let Err(err) = fs::write(path, &result.formatted) {
                        let _ = writeln!(stderr, "error: failed to write `{display_name}`: {err}");
                        had_io_or_fmt_error = true;
                        continue;
                    }
                    if opts.files_with_diff && !opts.quiet {
                        let _ = writeln!(stdout, "{display_name}");
                    }
                }
            }
        }
    }

    if had_io_or_fmt_error || any_diff_in_check {
        EXIT_DIFF_OR_FORMAT_ERR
    } else {
        EXIT_OK
    }
}

fn is_program_name(arg: &str) -> bool {
    if arg == "nessa-fmt" || arg == "nessa-fmt.exe" {
        return true;
    }
    let p = Path::new(arg);
    p.extension().is_none_or(|e| e == "exe")
        && p.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s == "nessa-fmt")
}

#[allow(clippy::too_many_arguments)]
fn run_stdin_mode(
    opts: &CliOptions,
    effective_emit: EmitMode,
    pinned_config: Option<FormatConfig>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    stdout_color: bool,
    stderr_color: bool,
) -> i32 {
    let mut src = String::new();
    if let Err(err) = stdin.read_to_string(&mut src) {
        let _ = writeln!(stderr, "error: failed to read stdin: {err}");
        return EXIT_DIFF_OR_FORMAT_ERR;
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (display_name, discover_dir) = if let Some(sfp) = &opts.stdin_filepath {
        (sfp.display().to_string(), parent_or_cwd(sfp))
    } else {
        ("<stdin>".to_string(), cwd)
    };

    let cfg = if let Some(pinned) = pinned_config {
        pinned
    } else {
        let mut c = match FormatConfig::discover(&discover_dir, None) {
            Ok(c) => c,
            Err(err) => {
                let _ = writeln!(stderr, "error: {err}");
                return EXIT_CLI_OR_CONFIG_ERR;
            }
        };
        for ov in &opts.config_overrides {
            if let Err(err) = c.apply_cli_overrides(ov) {
                let _ = writeln!(stderr, "error: {err}");
                return EXIT_CLI_OR_CONFIG_ERR;
            }
        }
        c
    };

    if let Some(sfp) = &opts.stdin_filepath
        && cfg.is_ignored(sfp, &discover_dir)
    {
        if opts.verbose && !opts.quiet {
            let _ = writeln!(stderr, "Ignoring `{}`", sfp.display());
        }
        if effective_emit != EmitMode::Check && !opts.files_with_diff {
            let _ = write!(stdout, "{src}");
        }
        return EXIT_OK;
    }

    let result = match format_source_internal(&src, &display_name, &cfg, stderr_color) {
        Ok(r) => r,
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}");
            return EXIT_DIFF_OR_FORMAT_ERR;
        }
    };

    if !opts.quiet {
        for warn in &result.warnings {
            let _ = writeln!(stderr, "warning in `{display_name}`: {warn}");
        }
    }

    match effective_emit {
        EmitMode::Check => {
            if result.changed {
                if opts.files_with_diff {
                    if !opts.quiet {
                        let _ = writeln!(stdout, "{display_name}");
                    }
                } else if !opts.quiet {
                    let diff_text =
                        render_check_diff(&display_name, &src, &result.formatted, stdout_color);
                    let _ = write!(stdout, "{diff_text}");
                }
                EXIT_DIFF_OR_FORMAT_ERR
            } else {
                EXIT_OK
            }
        }
        EmitMode::Files | EmitMode::Stdout => {
            if opts.files_with_diff {
                if result.changed && !opts.quiet {
                    let _ = writeln!(stdout, "{display_name}");
                }
            } else {
                let _ = write!(stdout, "{}", result.formatted);
            }
            EXIT_OK
        }
    }
}

struct ConfigResolver {
    pinned: Option<FormatConfig>,
    overrides: Vec<String>,
    by_dir: HashMap<PathBuf, FormatConfig>,
}

impl ConfigResolver {
    fn resolve(&mut self, dir: &Path) -> Result<FormatConfig, ConfigError> {
        if let Some(pinned) = &self.pinned {
            return Ok(pinned.clone());
        }
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if let Some(cached) = self.by_dir.get(&key) {
            return Ok(cached.clone());
        }
        let mut cfg = FormatConfig::discover(dir, None)?;
        for ov in &self.overrides {
            cfg.apply_cli_overrides(ov)?;
        }
        self.by_dir.insert(key, cfg.clone());
        Ok(cfg)
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_directory_targets(
    scan_root: &Path,
    dir: &Path,
    is_root: bool,
    opts: &CliOptions,
    resolver: &mut ConfigResolver,
    targets: &mut Vec<(PathBuf, FormatConfig)>,
    stderr: &mut dyn Write,
    had_io_error: &mut bool,
) -> Result<(), ConfigError> {
    let dir_cfg = resolver.resolve(dir)?;
    if !is_root && dir_cfg.is_ignored(dir, scan_root) {
        if opts.verbose && !opts.quiet {
            let _ = writeln!(stderr, "Ignoring directory `{}`", dir.display());
        }
        return Ok(());
    }

    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "error: failed to read directory `{}`: {err}",
                dir.display()
            );
            *had_io_error = true;
            return Ok(());
        }
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) => paths.push(e.path()),
            Err(err) => {
                let _ = writeln!(
                    stderr,
                    "error: failed to read entry in `{}`: {err}",
                    dir.display()
                );
                *had_io_error = true;
            }
        }
    }
    paths.sort();

    for entry_path in paths {
        let Some(name_os) = entry_path.file_name() else {
            continue;
        };
        let name = name_os.to_string_lossy();
        if entry_path.is_dir() {
            if name.starts_with('.') || name == "target" {
                continue;
            }
            collect_directory_targets(
                scan_root,
                &entry_path,
                false,
                opts,
                resolver,
                targets,
                stderr,
                had_io_error,
            )?;
        } else if entry_path.is_file() {
            if name == CONFIG_FILE_NAME {
                continue;
            }
            if entry_path.extension().and_then(|e| e.to_str()) != Some("ns") {
                continue;
            }
            if dir_cfg.is_ignored(&entry_path, scan_root) {
                if opts.verbose && !opts.quiet {
                    let _ = writeln!(stderr, "Ignoring `{}`", entry_path.display());
                }
                continue;
            }
            targets.push((entry_path, dir_cfg.clone()));
        }
    }

    Ok(())
}

fn resolve_anchor_dir(opts: &CliOptions) -> PathBuf {
    if let Some(sfp) = &opts.stdin_filepath {
        return parent_or_cwd(sfp);
    }
    if let Some(first) = opts.inputs.first()
        && first != "-"
    {
        let p = Path::new(first);
        if p.is_dir() {
            return p.to_path_buf();
        }
        return parent_or_cwd(p);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn parent_or_cwd(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

fn parse_cli_options(args: &[String]) -> Result<CliOptions, String> {
    let mut opts = CliOptions::default();
    let mut i = 0usize;
    let mut after_double_dash = false;

    while i < args.len() {
        let arg = &args[i];
        if after_double_dash {
            opts.inputs.push(arg.clone());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }

        if arg == "-" {
            opts.inputs.push("-".to_string());
            i += 1;
            continue;
        }

        if arg == "-h" || arg == "--help" {
            opts.help = true;
            i += 1;
            continue;
        }
        if arg == "-V" || arg == "--version" {
            opts.version = true;
            i += 1;
            continue;
        }
        if arg == "--check" {
            opts.check_flag = true;
            i += 1;
            continue;
        }
        if arg == "--backup" {
            opts.backup = true;
            i += 1;
            continue;
        }
        if arg == "-l" || arg == "--files-with-diff" {
            opts.files_with_diff = true;
            i += 1;
            continue;
        }
        if arg == "-v" || arg == "--verbose" {
            opts.verbose = true;
            i += 1;
            continue;
        }
        if arg == "-q" || arg == "--quiet" {
            opts.quiet = true;
            i += 1;
            continue;
        }

        if let Some(val) = take_opt_value(args, &mut i, "--emit")? {
            let mode = match val.as_str() {
                "files" => EmitMode::Files,
                "stdout" => EmitMode::Stdout,
                "check" => EmitMode::Check,
                other => {
                    return Err(format!(
                        "invalid `--emit` mode `{other}` (expected `files`, `stdout`, or `check`)"
                    ));
                }
            };
            opts.emit = Some(mode);
            continue;
        }

        if let Some(val) = take_opt_value(args, &mut i, "--stdin-filepath")? {
            opts.stdin_filepath = Some(PathBuf::from(val));
            continue;
        }

        if let Some(val) = take_opt_value(args, &mut i, "--config-path")? {
            opts.config_path = Some(PathBuf::from(val));
            continue;
        }

        if let Some(val) = take_opt_value(args, &mut i, "--config")? {
            opts.config_overrides.push(val);
            continue;
        }

        if let Some(val) = take_opt_value(args, &mut i, "--print-config")? {
            let mode = match val.as_str() {
                "default" => PrintConfigMode::Default,
                "minimal" => PrintConfigMode::Minimal,
                "current" => PrintConfigMode::Current,
                other => {
                    return Err(format!(
                        "invalid `--print-config` mode `{other}` (expected `default`, `minimal`, or `current`)"
                    ));
                }
            };
            opts.print_config = Some(mode);
            continue;
        }

        if let Some(val) = take_opt_value(args, &mut i, "--color")? {
            let choice = ColorChoice::parse(&val).ok_or_else(|| {
                format!("invalid `--color` value `{val}` (expected `auto`, `always`, or `never`)")
            })?;
            opts.color = choice;
            continue;
        }

        if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2 {
            for ch in arg[1..].chars() {
                match ch {
                    'l' => opts.files_with_diff = true,
                    'v' => opts.verbose = true,
                    'q' => opts.quiet = true,
                    'h' => opts.help = true,
                    'V' => opts.version = true,
                    other => return Err(format!("unknown option `-{other}`")),
                }
            }
            i += 1;
            continue;
        }

        if arg.starts_with('-') {
            return Err(format!("unknown option `{arg}`"));
        }

        opts.inputs.push(arg.clone());
        i += 1;
    }

    Ok(opts)
}

fn take_opt_value(args: &[String], idx: &mut usize, flag: &str) -> Result<Option<String>, String> {
    let cur = &args[*idx];
    if cur == flag {
        let next = args
            .get(*idx + 1)
            .ok_or_else(|| format!("option `{flag}` requires a value"))?;
        *idx += 2;
        return Ok(Some(next.clone()));
    }
    if let Some(rest) = cur.strip_prefix(flag)
        && let Some(val) = rest.strip_prefix('=')
    {
        *idx += 1;
        return Ok(Some(val.to_string()));
    }
    Ok(None)
}

pub fn render_help() -> String {
    String::from(
        "\
Format Nessa source code according to official style rules.

Usage: nessa-fmt [OPTIONS] [PATH...]

If no paths are given, recursively formats `.ns` files in the current directory.
Use `-` to format from stdin to stdout.

Options:
      --check                       Run in 'check' mode; exit with 1 if formatting differs
      --emit <files|stdout|check>   What data to emit and how (default: files)
      --backup                      Back up modified files with a `.bk` extension
  -l, --files-with-diff             Print the names of mismatched files
      --stdin-filepath <PATH>       File path context when reading from stdin
      --config-path <PATH>          Path to `nessafmt.toml` configuration file or directory
      --config <KEY=VAL,...>        Override configuration options from the command line
      --print-config <MODE>         Print `default`, `minimal`, or `current` configuration
      --color <auto|always|never>   Colorize diff and diagnostic output (default: auto)
  -v, --verbose                     Print verbose processing output
  -q, --quiet                       Suppress warnings and non-error output
  -h, --help                        Print help information
  -V, --version                     Print version information
",
    )
}
