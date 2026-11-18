//! Whole-repository in-memory corpus gate across `../../library/**/*.ns` and `../../tests/**/*.ns`.
//! Never modifies any repository `.ns` files on disk.

use std::fs;
use std::path::{Path, PathBuf};

use diagnostic::DiagnosticContext;
use nessa_fmt::{FormatConfig, format_source_with_path};
use parser::Parser;
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

fn collect_ns_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|x| x.path())).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            collect_ns_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("ns") {
            out.push(p);
        }
    }
}

fn is_parseable_nessa(src: &str, path: &Path) -> bool {
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(
        FileName::Custom(path.display().to_string()),
        src.to_string(),
    );
    let diag_ctx = DiagnosticContext::new_silent(&map);
    let (tokens, lex_errors) = lexer::tokenize(src);
    if !lex_errors.is_empty() {
        return false;
    }
    let ast = Parser::new(&tokens, src, &diag_ctx, file.start_pos).parse();
    !diag_ctx.has_errors() && !ast.root.is_null()
}

#[test]
fn format_and_verify_entire_library_and_tests_corpus_idempotently() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("nessa2 repository root");

    let mut files = Vec::new();
    collect_ns_files(&repo_root.join("library"), &mut files);
    collect_ns_files(&repo_root.join("tests"), &mut files);

    assert!(
        !files.is_empty(),
        "expected `.ns` files in library/ and tests/"
    );

    let cfg = FormatConfig::default();
    let mut formatted_count = 0usize;
    let mut skipped_negative_tests = 0usize;
    let mut failures = Vec::new();

    for path in &files {
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };
        if !is_parseable_nessa(&src, path) {
            skipped_negative_tests += 1;
            continue;
        }

        match format_source_with_path(&src, Some(path), &cfg) {
            Ok(pass1) => match format_source_with_path(&pass1.formatted, Some(path), &cfg) {
                Ok(pass2) => {
                    if pass2.formatted != pass1.formatted {
                        let diff = nessa_fmt::render_check_diff(
                            &path.display().to_string(),
                            &pass1.formatted,
                            &pass2.formatted,
                            false,
                        );
                        failures.push(format!("NON-IDEMPOTENT on `{}`:\n{diff}", path.display()));
                    } else {
                        formatted_count += 1;
                    }
                }
                Err(err) => {
                    failures.push(format!(
                        "PASS 2 FAILED on `{}`: {err}\n--- pass1 ---\n{}",
                        path.display(),
                        pass1.formatted
                    ));
                }
            },
            Err(err) => {
                failures.push(format!("PASS 1 FAILED on `{}`: {err}", path.display()));
            }
        }
    }

    assert!(
        formatted_count > 10,
        "expected at least 10 parseable `.ns` files, got {formatted_count}"
    );
    if !failures.is_empty() {
        panic!(
            "{} / {} parseable corpus files failed (skipped {} unparseable negative tests):\n\n{}",
            failures.len(),
            formatted_count + failures.len(),
            skipped_negative_tests,
            failures
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n====================\n")
        );
    }
}
