//! Integration tests for Wave E1 & E2: multi-layer diagnostic overhaul,
//! multi-file spans, parser synchronization recovery, and heuristic syntax/semantic
//! misuse diagnostics.

use diagnostic::emitter::{AriadneEmitter, EmitterConfig};
use diagnostic::{DiagnosticBuilder, DiagnosticContext, NessaError};
use driver::Driver;
use rustc_span::{BytePos, DUMMY_SP, FileName, SourceMap, Span, source_map::FilePathMapping};

fn compile_messages_and_render(source: &str) -> (Vec<diagnostic::Diagnostic>, String) {
    let sm = SourceMap::new(FilePathMapping::empty());
    let sf = sm.new_source_file(FileName::Custom("diag_test.ns".into()), source.to_owned());
    let ctx = DiagnosticContext::new(&sm);
    let (tokens, lex_errs) = lexer::tokenize(source);
    for err in &lex_errs {
        err.emit(&ctx, sf.start_pos);
    }
    if !ctx.has_errors() {
        let ast = parser::Parser::new(&tokens, source, &ctx, sf.start_pos).parse();
        if !ctx.has_errors() {
            let _ = resolution::resolve(ast, &ctx);
        }
    }
    let diags = ctx.diagnostics().to_vec();
    let emitter = AriadneEmitter::new(EmitterConfig {
        use_colors: false,
        ..EmitterConfig::default()
    });
    let mut rendered = String::new();
    for d in &diags {
        rendered.push_str(&emitter.render_to_string(d, &ctx));
    }
    (diags, rendered)
}

#[test]
fn multi_file_ariadne_emitter_renders_cross_file_labels_and_skips_dummy_spans() {
    let sm = SourceMap::new(FilePathMapping::empty());
    let f1 = sm.new_source_file(
        FileName::Custom("pkg/defs.ns".into()),
        "pub struct Config {\n    pub port: i64,\n}\n".into(),
    );
    let f2 = sm.new_source_file(
        FileName::Custom("pkg/main.ns".into()),
        "fn main() {\n    let c = Config { prot: 8080 }\n}\n".into(),
    );
    let ctx = DiagnosticContext::new(&sm);
    let primary = Span::new(f2.start_pos + BytePos(29), f2.start_pos + BytePos(33));
    let secondary = Span::new(f1.start_pos + BytePos(4), f1.start_pos + BytePos(17));
    let diag = DiagnosticBuilder::error("unknown field `prot` on struct `Config`".into())
        .with_code(diagnostic::codes::UNKNOWN_MEMBER)
        .with_primary_span(primary)
        .with_error_label(primary, "unknown field".into())
        .with_note_label(secondary, "`Config` defined here".into())
        .with_note_label(DUMMY_SP, "dummy label must be ignored".into())
        .with_help("did you mean `port`?".into())
        .build();

    let emitter = AriadneEmitter::new(EmitterConfig {
        use_colors: false,
        ..EmitterConfig::default()
    });
    let text = emitter.render_to_string(&diag, &ctx);
    assert!(text.contains("[4003]"), "{text}");
    assert!(text.contains("pkg/main.ns"), "{text}");
    assert!(text.contains("pkg/defs.ns"), "{text}");
    assert!(text.contains("did you mean `port`?"), "{text}");
    assert!(!text.contains("dummy label must be ignored"), "{text}");
}

#[test]
fn lexer_reports_actionable_escape_quote_and_comment_hints() {
    let (_, rendered_unicode) =
        compile_messages_and_render("fn main() {\n    let s = \"\\u{0041}\"\n}\n");
    assert!(
        rendered_unicode.contains("\\uXXXX") || rendered_unicode.contains("\\xHH"),
        "{rendered_unicode}"
    );

    let (_, rendered_fullwidth) = compile_messages_and_render("fn main（） {\n    42\n}\n");
    assert!(
        rendered_fullwidth.contains("ASCII") || rendered_fullwidth.contains("("),
        "{rendered_fullwidth}"
    );

    let (_, rendered_comment) =
        compile_messages_and_render("{- outer {- inner -} still open\nfn main() { 42 }\n");
    assert!(rendered_comment.contains("-}"), "{rendered_comment}");
}

#[test]
fn parser_synchronizes_across_statements_to_report_multiple_syntax_errors_in_one_pass() {
    let src = r#"
fn main() {
    let mut a = 10
    if a === 10 {
        let s = 'hello'
    }
    a
}
"#;
    let compiled = Driver::new().compile(src);
    assert!(compiled.has_errors);
    assert!(
        compiled.diagnostics.len() >= 3,
        "expected >= 3 diagnostics in one pass, got {}: {:?}",
        compiled.diagnostics.len(),
        compiled.diagnostics
    );
}

#[test]
fn parser_syntax_misuse_heuristics_give_nessa_specific_guidance() {
    let cases: &[(&str, &str)] = &[
        ("fn main() {\n    // c-style comment\n    42\n}", "--"),
        ("fn main() {\n    /* block comment */\n    42\n}", "{-"),
        (
            "fn main() {\n    let s = 'hello'\n    0\n}",
            "double quotes",
        ),
        (
            "fn main() {\n    if true && false { 1 } else { 0 }\n}",
            "`and`",
        ),
        (
            "fn main() {\n    if true || false { 1 } else { 0 }\n}",
            "`or`",
        ),
        ("fn main() {\n    if 1 === 1 { 1 } else { 0 }\n}", "`==`"),
        ("fn main() {\n    if 1 !== 2 { 1 } else { 0 }\n}", "`!=`"),
        (
            "fn main() {\n    let ok = true\n    if !ok { 0 } else { 1 }\n}",
            "`not ",
        ),
        (
            "fn main() {\n    var x = 1\n    if x = 2 { 1 } else { 0 }\n}",
            "`==`",
        ),
        ("fn main() {\n    let mut x = 1\n    x\n}", "`var x"),
        (
            "fn main() {\n    if false { 1 } elif true { 2 } else { 3 }\n}",
            "`else if`",
        ),
        (
            "fn main() {\n    match 1 {\n        1 => 42,\n        _ => 0,\n    }\n}",
            "postfix",
        ),
        (
            "async fn fetch() -> i64 { 42 }\nfn main() { 0 }",
            "async effect",
        ),
        (
            "fn add(a: i64, b: i64): i64 { a + b }\nfn main() { 0 }",
            "`->`",
        ),
        (
            "fn main() {\n    let f = |x| => x + 1\n    0\n}",
            "without `=>`",
        ),
        (
            "fn main() {\n    1 match {\n        1 -> 42,\n        _ => 0,\n    }\n}",
            "`=>`",
        ),
        ("def helper() { 42 }\nfn main() { 0 }", "`fn`"),
    ];

    for &(source, expected_hint) in cases {
        let (_, rendered) = compile_messages_and_render(source);
        assert!(
            rendered.contains(expected_hint),
            "source:\n{source}\nexpected hint `{expected_hint}` in:\n{rendered}"
        );
    }
}

#[test]
fn semantic_heuristics_report_typos_imports_concat_wrappers_and_capture_provenance() {
    // 1. Identifier typo suggestion + error code [3001].
    let (_, r1) = compile_messages_and_render(
        "fn main() {\n    let counter_value = 42\n    conter_value\n}\n",
    );
    assert!(r1.contains("[3001]"), "{r1}");
    assert!(r1.contains("did you mean `counter_value`?"), "{r1}");

    // 2. Unimported std module suggestion (`fs.exists` -> `use std.fs`).
    let compiled_fs = Driver::new().compile("fn main() {\n    fs.exists(\"a\")\n}\n");
    assert!(compiled_fs.has_errors);
    assert!(
        compiled_fs
            .diagnostics
            .iter()
            .any(|d| d.helps.iter().any(|h| h.contains("use std.fs"))),
        "{:?}",
        compiled_fs.diagnostics
    );

    // 3. String `+` misuse suggests `++`.
    let compiled_concat =
        Driver::new().compile("fn main() {\n    let s = \"hello\" + \" world\"\n    0\n}\n");
    assert!(compiled_concat.has_errors);
    assert!(
        compiled_concat
            .diagnostics
            .iter()
            .any(|d| d.helps.iter().any(|h| h.contains("++"))),
        "{:?}",
        compiled_concat.diagnostics
    );

    // 4. Optional `?.` / Future `.await` / Error `!.` wrapper member hints.
    let compiled_opt = Driver::new().compile(
        "struct User { name: String }\nfn main() {\n    let u: ?User = User { name: \"a\" }\n    u.name\n}\n",
    );
    assert!(compiled_opt.has_errors);
    assert!(
        compiled_opt.diagnostics.iter().any(|d| d
            .helps
            .iter()
            .any(|h| h.contains("?.") || h.contains("unwrap"))),
        "{:?}",
        compiled_opt.diagnostics
    );

    let compiled_fut = Driver::new()
        .compile("use std.builtin.Future\nfn check(f: Future) {\n    f.v\n}\nfn main() { 0 }\n");
    assert!(compiled_fut.has_errors);
    assert!(
        compiled_fut
            .diagnostics
            .iter()
            .any(|d| d.helps.iter().any(|h| h.contains(".await"))),
        "{:?}",
        compiled_fut.diagnostics
    );

    // 5. Inner named `fn` capturing outer local suggests closure `|...|` + declaration label.
    let compiled_cap = Driver::new().compile(
        "fn main() {\n    let factor = 3\n    fn scale(x: i64) -> i64 { x * factor }\n    scale(14)\n}\n",
    );
    assert!(compiled_cap.has_errors);
    assert!(
        compiled_cap
            .diagnostics
            .iter()
            .any(|d| d.code == Some(diagnostic::codes::NAMED_FN_CAPTURE)
                && d.helps.iter().any(|h| h.contains("|...|"))),
        "{:?}",
        compiled_cap.diagnostics
    );

    // 6. `const` reassignment points to `const` declaration and suggests `let` or `var`.
    let compiled_imm =
        Driver::new().compile("fn main() {\n    const count = 1\n    count = 2\n    count\n}\n");
    assert!(compiled_imm.has_errors);
    assert!(
        compiled_imm
            .diagnostics
            .iter()
            .any(|d| d.code == Some(diagnostic::codes::IMMUTABLE_ASSIGNMENT)
                && d.helps.iter().any(|h| h.contains("`var`"))),
        "{:?}",
        compiled_imm.diagnostics
    );

    // 7. `List.append(scalar)` suggests `List.push(...)`.
    let compiled_push =
        Driver::new().compile("fn main() {\n    let xs = [1, 2]\n    xs.append(3)\n    0\n}\n");
    assert!(compiled_push.has_errors);
    assert!(
        compiled_push
            .diagnostics
            .iter()
            .any(|d| d.helps.iter().any(|h| h.contains(".push("))),
        "{:?}",
        compiled_push.diagnostics
    );
}
