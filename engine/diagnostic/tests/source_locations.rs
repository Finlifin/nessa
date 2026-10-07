//! Source-only diagnostics must still expose their authenticated location.
use diagnostic::{DiagnosticBuilder, DiagnosticContext, LocatedSourceSpan};
use rustc_span::source_map::FilePathMapping;
use rustc_span::{BytePos, DUMMY_SP, FileName, SourceMap, Span};

#[test]
fn render_location_probe() {
    let Ok(mode) = std::env::var("NESSA_DIAGNOSTIC_LOCATION_PROBE") else {
        return;
    };
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(
        FileName::Custom("location-owner".into()),
        "\u{feff}x".into(),
    );
    let context = DiagnosticContext::new(&map);
    let builder = DiagnosticBuilder::error("location-message".into());
    let builder = if mode.ends_with("anchored") {
        builder.with_primary_source_span(LocatedSourceSpan {
            span: DUMMY_SP,
            file_start: Some(file.start_pos),
        })
    } else if mode.ends_with("outside") {
        builder.with_primary_source_span(LocatedSourceSpan {
            span: Span::new(BytePos(0), BytePos(99)),
            file_start: Some(file.start_pos),
        })
    } else {
        builder.with_primary_span(DUMMY_SP)
    };
    if mode.starts_with("context") {
        builder.emit(&context);
    } else {
        diagnostic::emitter::AriadneEmitter::new_default()
            .emit_diagnostic(&builder.build(), &context);
    }
}

#[test]
fn primary_without_labels_shows_real_zero_but_never_claims_dummy_or_invalid_source() {
    for emitter in ["context", "standalone"] {
        for kind in ["anchored", "dummy", "outside"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "render_location_probe", "--nocapture"])
                .env(
                    "NESSA_DIAGNOSTIC_LOCATION_PROBE",
                    format!("{emitter}-{kind}"),
                )
                .output()
                .unwrap();
            assert!(output.status.success());
            let text = format!(
                "{}{}",
                String::from_utf8(output.stdout).unwrap(),
                String::from_utf8(output.stderr).unwrap()
            );
            assert!(text.contains("location-message"), "{text}");
            assert_eq!(
                text.contains("location-owner"),
                kind == "anchored",
                "{emitter}-{kind}: {text}"
            );
            if kind == "anchored" {
                assert!(text.contains("1:1"), "{text}");
            }
        }
    }
}
