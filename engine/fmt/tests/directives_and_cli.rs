//! Comprehensive unit and integration tests for `nessa-fmt`:
//! `--%` directives (including unparseable `.off` and `.skip`), comment preservation,
//! Nessa syntax coverage, `format_range`, `nessafmt.toml`, and CLI exit codes (`0`/`1`/`2`).

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use nessa_fmt::{
    DIRECTIVE_SPECS, EXIT_CLI_OR_CONFIG_ERR, EXIT_DIFF_OR_FORMAT_ERR, EXIT_OK, FormatConfig,
    NewlineStyle, SmallHeuristics, format_range, format_source, run_cli_with_io,
};

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn make_temp_dir(prefix: &str) -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "nessa_fmt_test_{prefix}_{}_{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn assert_idempotent(src: &str, cfg: &FormatConfig) -> String {
    let pass1 = format_source(src, cfg).expect("pass 1 should succeed");
    let pass2 = format_source(&pass1.formatted, cfg).expect("pass 2 should succeed");
    assert_eq!(
        pass1.formatted, pass2.formatted,
        "formatting must be idempotent"
    );
    assert!(!pass2.changed, "pass 2 must report changed = false");
    pass1.formatted
}

#[test]
fn directive_specs_table_exports_all_supported_directives() {
    let names: Vec<&str> = DIRECTIVE_SPECS.iter().map(|s| s.name).collect();
    for expected in [
        ".skip",
        ".preserve",
        ".off",
        ".on",
        ".max_width",
        ".tab_spaces",
        ".newline_style",
        ".use_small_heuristics",
        ".reorder_imports",
        ".normalize_comments",
    ] {
        assert!(
            names.contains(&expected),
            "missing `{expected}` in DIRECTIVE_SPECS"
        );
    }
    for spec in DIRECTIVE_SPECS {
        assert!(!spec.description.is_empty());
        assert!(spec.value_hint.is_some());
    }
}

#[test]
fn directive_canonicalization_warnings_and_inline_comment_distinction() {
    let src = "\
--%.max_width=80, .normalize_comments=true
--% .unknown_directive 123
fn compute_sum(first_val: i64, second_val: i64) -> i64 {
    let total = first_val+second_val --% .off inline comment, not a directive!
    --tight_comment
    --- banner comment stays untouched
    total
}
";
    let res = format_source(src, &FormatConfig::default()).expect("format succeeds with warning");
    assert_eq!(res.warnings.len(), 1);
    assert!(res.warnings[0].message.contains("unknown_directive"));
    assert!(
        res.formatted
            .contains("--% .max_width 80, .normalize_comments true\n")
    );
    assert!(res.formatted.contains("--% .unknown_directive 123\n"));
    // Trailing `--%` was not a directive (so `first_val+second_val` WAS formatted) AND `--%` was not split into `-- %`!
    assert!(
        res.formatted.contains(
            "let total = first_val + second_val --% .off inline comment, not a directive!"
        )
    );
    assert!(res.formatted.contains("    -- tight_comment\n"));
    assert!(
        res.formatted
            .contains("    --- banner comment stays untouched\n")
    );

    let pass2 = format_source(&res.formatted, &FormatConfig::default()).unwrap();
    assert_eq!(res.formatted, pass2.formatted);
}

#[test]
fn directive_off_and_on_including_unparseable_regions_and_brace_close() {
    let src = "\
fn build_table() -> i64 {
    let before=1+2
    --% .off
    @@@ completely unparseable raw DSL block {{ ???
        weird   spacing   1 +   2
    --% .on
    let middle=3+4
    if before > 0 {
        --% .off
        unparseable_until_closing_brace $$$ !!!
    }
    before+middle
}
";
    let formatted = assert_idempotent(src, &FormatConfig::default());
    assert!(formatted.contains("    let before = 1 + 2\n"));
    assert!(formatted.contains(
        "    @@@ completely unparseable raw DSL block {{ ???\n        weird   spacing   1 +   2\n"
    ));
    assert!(formatted.contains("    let middle = 3 + 4\n"));
    assert!(formatted.contains("        unparseable_until_closing_brace $$$ !!!\n    }\n"));
    assert!(formatted.contains("    before + middle\n"));
}

#[test]
fn directive_skip_handles_both_valid_and_unparseable_nodes_with_indent_shift() {
    let src = "\
fn matrix_demo() -> i64 {
        --%.skip
        let matrix = [
            1,   0,   0,
            0,  10,   0,
        ]
        --% .skip
        @@@ future_syntax_item(a, b) {
            raw_line   1
        }
        let clean=1+2
        clean
}
";
    let formatted = assert_idempotent(src, &FormatConfig::default());
    assert!(formatted.contains(
        "    --% .skip\n    let matrix = [\n        1,   0,   0,\n        0,  10,   0,\n    ]\n"
    ));
    assert!(formatted.contains(
        "    --% .skip\n    @@@ future_syntax_item(a, b) {\n        raw_line   1\n    }\n"
    ));
    assert!(formatted.contains("    let clean = 1 + 2\n"));
}

#[test]
fn directive_preserve_keeps_line_breaks_and_alignment_while_normalizing_outer_indent() {
    let src = "\
fn aligned_bindings() -> i64 {
        --% .preserve
        let value = 10 +   20 + --note
                    30 +   40
        let normal=1+2
        value+normal
}
";
    let formatted = assert_idempotent(src, &FormatConfig::default());
    assert!(formatted.contains(
        "    --% .preserve\n    let value = 10 +   20 + -- note\n                30 +   40\n"
    ));
    assert!(formatted.contains("    let normal = 1 + 2\n"));
}

#[test]
fn scoped_directives_pop_at_enclosing_block_boundary() {
    let src = "\
fn outer_fn(alpha: i64, beta: i64, gamma: i64) -> i64 {
    {
        --% .tab_spaces 2, .max_width 35, .normalize_comments false
        if alpha > 0 {
            --raw_inner
            let sum_val = alpha + beta + gamma + 100
            sum_val
        }
    }
    --raw_outer
    alpha + beta
}
";
    let formatted = assert_idempotent(src, &FormatConfig::default());
    // Inside the inner `{ ... }`, `--raw_inner` stays unnormalized and indent increases by 2 spaces.
    assert!(formatted.contains("--raw_inner"));
    // Outside the inner `{ ... }`, `.normalize_comments` reverted to `true`!
    assert!(formatted.contains("    -- raw_outer\n"));
}

#[test]
fn reorder_imports_groups_std_external_and_intrapackage_separately_from_pub_use() {
    let src = "\
use .utils.validate
use json.parse
use std.io.{write_file, read_file}
use @config.Settings
use http_lib.{Response, Client, Request}
use std.collections.{Set, Map}

pub use @api.Router
pub use std.fmt.Display

fn run_app() -> i64 = 0
";
    let formatted = assert_idempotent(src, &FormatConfig::default());
    let expected = "\
use std.collections.{Map, Set}
use std.io.{read_file, write_file}

use http_lib.{Client, Request, Response}
use json.parse

use @config.Settings
use .utils.validate

pub use std.fmt.Display

pub use @api.Router

fn run_app() -> i64 = 0
";
    assert_eq!(formatted, expected);

    // Verify `.reorder_imports false` leaves order untouched.
    let disabled = format!("--% .reorder_imports false\n{src}");
    let kept = assert_idempotent(&disabled, &FormatConfig::default());
    assert!(kept.contains("use .utils.validate\nuse json.parse\n"));
}

#[test]
fn zero_comment_loss_across_all_tricky_positions_and_aligned_trailing_comments() {
    let src = "\
-- file header comment
enum Color {
    red,     -- primary red
    green,   -- primary green
    blue,    -- primary blue
}

struct EmptyHolder {
    -- comment inside empty struct
}

fn tricky_comments(
    -- comment in empty-ish params before arg
    input_val: Color,
) -> i64 {
    {- outer block comment
       {- nested block comment -}
       end of outer -}
    let empty_list = [
        -- inside empty list
    ]
    if input_val matches Color.red {
        1
    } -- comment between then and else
    else {
        -- inside else
        2
    }
    input_val match {
        -- before first arm
        Color.red => 10,
        Color.green => {
            -- inside arm block
            20
        }
        Color.blue => 30,
        -- after last arm
    }
}
-- trailing EOF comment
";
    let formatted = assert_idempotent(src, &FormatConfig::default());
    // Aligned trailing comments on consecutive enum variants remain aligned!
    assert!(formatted.contains(
        "    red,     -- primary red\n    green,   -- primary green\n    blue,    -- primary blue\n"
    ));
    assert!(formatted.contains("-- comment inside empty struct"));
    assert!(formatted.contains("{- nested block comment -}"));
    assert!(formatted.contains("-- inside empty list"));
    assert!(formatted.contains("-- comment between then and else"));
    assert!(formatted.contains("-- before first arm"));
    assert!(formatted.contains("-- after last arm"));
    assert!(formatted.contains("-- trailing EOF comment"));
}

#[test]
fn nessa_specific_syntax_constructs_round_trip_and_obey_code_style() {
    let src = "\
private struct InternalCounter {
    pub count: i64 = 0,
}

enum IoError {
    NotFound(path: String),
    TimedOut,
}

enum Shape {
    circle(radius: f64),
    rectangle(width: f64, height: f64),
    point,
}

effect read_token() -> String
async effect fetch_remote(endpoint: String) -> String

trait Measurable(Printable, Cloneable) {
    assoc Metric: Type = f64
    fn area(self) -> f64
    derive fn is_empty(self) -> bool = self.area() == 0.0
}

fn exercise_syntax(items: [i64], maybe_val: ?i64) -> !IoError i64 handles read_token {
    let unwrapped? = maybe_val else { return 0 }
    defer {
        let cleanup_done = true
    }
    var acc = -(-unwrapped)
    let neg_space = - -acc
    while :outer_loop acc < 100 {
        for :inner_loop item in items {
            break outer_loop if item == 99
            continue inner_loop if item < 0
            acc += item
        }
        break
    }
    handles(read_token) let tok = read_token()#
    let piped = items
        |> map(_0 + 1)
        |> filter(|elem| elem > 0)
    let infix_val = 10 `custom_add` 20
    let greeting = \"hello {{literal}} {tok} count={acc + 1}\"
    let view_val = items'length
    let casted = acc.as(i64)
    let sym = .ready
    when {
        casted > 10 => casted,
        else => 0,
    }
}

fn handler_forms() -> String {
    handles(read_token)() -> String = \"inline\"
    read_token()#
}

fn arrow_handler_forms() -> String {
    handles read_token() -> String => \"sync\"
    handles async fetch_remote(url) -> String => url
    read_token()#
}
";
    let formatted = assert_idempotent(src, &FormatConfig::default());
    assert!(formatted.contains("trait Measurable(Printable, Cloneable) {"));
    assert!(formatted.contains("while :outer_loop acc < 100 {"));
    assert!(formatted.contains("for :inner_loop item in items {"));
    assert!(formatted.contains("break outer_loop if item == 99"));
    assert!(formatted.contains("continue inner_loop if item < 0"));
    assert!(formatted.contains("- -acc"));
    assert!(formatted.contains("\"hello {{literal}} {tok} count={acc + 1}\""));
    assert!(formatted.contains("items'length"));
    assert!(formatted.contains("10 `custom_add` 20"));
}

#[test]
fn format_range_formats_only_requested_lines_without_reordering_imports() {
    let src = "\
use .local_mod.helper
use std.io.read_file

fn untouched_fn() -> i64 {
    let   badly_spaced=1+2
    badly_spaced
}

fn target_fn() -> i64 {
    let   target_val=10+20
    target_val
}
";
    // Lines 8..=11 (0-based) cover `target_fn` only.
    let res = format_range(src, (8, 11), &FormatConfig::default()).expect("range format succeeds");
    assert!(res.changed);
    // Imports at lines 0..1 and `untouched_fn` at lines 3..6 remain untouched!
    assert!(
        res.formatted
            .starts_with("use .local_mod.helper\nuse std.io.read_file\n")
    );
    assert!(res.formatted.contains("    let   badly_spaced=1+2\n"));
    assert!(res.formatted.contains("    let target_val = 10 + 20\n"));
}

#[test]
fn newline_style_and_small_heuristics_configs_work() {
    let src = "fn add_pair(x: i64, y: i64) -> i64 = x+y\r\n";
    let auto_cfg = FormatConfig {
        newline_style: NewlineStyle::Auto,
        ..Default::default()
    };
    let res = format_source(src, &auto_cfg).unwrap();
    assert_eq!(
        res.formatted,
        "fn add_pair(x: i64, y: i64) -> i64 = x + y\r\n"
    );

    let off_cfg = FormatConfig {
        use_small_heuristics: SmallHeuristics::Off,
        ..Default::default()
    };
    let call_src = "fn demo() -> i64 = compute(1, 2)\n";
    let call_res = assert_idempotent(call_src, &off_cfg);
    assert!(call_res.contains("compute(\n    1,\n    2,\n)"));
}

#[test]
fn cli_flags_toml_discovery_ignore_backup_stdin_and_exit_codes() {
    let dir = make_temp_dir("cli");
    let config_file = dir.join("nessafmt.toml");
    fs::write(
        &config_file,
        "# Nessa formatter config\nmax_width = 80\nignore = [\"ignored_dir/**\"]\n",
    )
    .unwrap();

    let ignored_dir = dir.join("ignored_dir");
    fs::create_dir_all(&ignored_dir).unwrap();
    let ignored_file = ignored_dir.join("skip_me.ns");
    fs::write(&ignored_file, "fn skip_me()->i64=1+2\n").unwrap();

    let target_file = dir.join("sample.ns");
    fs::write(&target_file, "fn sample_fn(a:i64,b:i64)->i64=a+b\n").unwrap();

    // 1. `--check` on unformatted file -> exit code 1 + diff output
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        [
            "nessa-fmt",
            "--check",
            "--color",
            "never",
            dir.to_str().unwrap(),
        ],
        &mut Cursor::new(""),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_DIFF_OR_FORMAT_ERR);
    let diff_str = String::from_utf8(out).unwrap();
    assert!(diff_str.contains("Diff in"));
    assert!(diff_str.contains("+fn sample_fn(a: i64, b: i64) -> i64 = a + b"));

    // 2. Format in-place with `--backup` and `-l` -> exit code 0, creates `.bk`, leaves ignored file untouched!
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        ["--backup", "-l", dir.to_str().unwrap()],
        &mut Cursor::new(""),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_OK);
    assert!(String::from_utf8(out).unwrap().contains("sample.ns"));
    assert_eq!(
        fs::read_to_string(&target_file).unwrap(),
        "fn sample_fn(a: i64, b: i64) -> i64 = a + b\n"
    );
    assert!(dir.join("sample.ns.bk").is_file());
    assert_eq!(
        fs::read_to_string(&ignored_file).unwrap(),
        "fn skip_me()->i64=1+2\n"
    );

    // 3. `--check` now clean -> exit code 0
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        ["--check", dir.to_str().unwrap()],
        &mut Cursor::new(""),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_OK);

    // 4. Stdin `-` mode with `--stdin-filepath`
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        ["-", "--stdin-filepath", target_file.to_str().unwrap()],
        &mut Cursor::new("fn from_stdin(x:i64)->i64=x*2\n"),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "fn from_stdin(x: i64) -> i64 = x * 2\n"
    );

    // 5. `--print-config minimal` and `--print-config default` -> exit code 0
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        [
            "--print-config",
            "minimal",
            "--config-path",
            config_file.to_str().unwrap(),
        ],
        &mut Cursor::new(""),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_OK);
    let printed = String::from_utf8(out).unwrap();
    assert!(printed.contains("max_width = 80"));
    assert!(!printed.contains("tab_spaces"));

    // 6. Invalid CLI flag or bad `--config` -> exit code 2
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        ["--no-such-flag"],
        &mut Cursor::new(""),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_CLI_OR_CONFIG_ERR);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        ["--config", "max_width=0", "-"],
        &mut Cursor::new(""),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_CLI_OR_CONFIG_ERR);

    // 7. Syntax error in stdin -> exit code 1
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_cli_with_io(
        ["-"],
        &mut Cursor::new("fn broken( {"),
        &mut out,
        &mut err,
        false,
        false,
    );
    assert_eq!(code, EXIT_DIFF_OR_FORMAT_ERR);

    let _ = fs::remove_dir_all(&dir);
}
