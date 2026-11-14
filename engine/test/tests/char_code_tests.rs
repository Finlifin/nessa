//! Char values are Unicode scalar values, and the reverse conversion rejects
//! surrogates.

mod common;

fn code_is(source_char: &str, code: i64) -> String {
    format!(
        r#"fn restored(value: ?char) -> bool {{
            value match {{
                found? => found == {source_char},
                _ => false,
            }}
        }}
        fn main() {{
            if {source_char}.to_code() == {code} and restored(char_from_code({code})) {{ 42 }} else {{ 0 }}
        }}"#
    )
}

#[test]
fn codes_round_trip_and_reject_non_scalars() {
    let source = r#"
        fn restored(value: ?char, code: i64) -> bool {
            value match {
                found? => found.to_code() == code,
                _ => false,
            }
        }
        fn main() {
            let matched = 'a'.to_code() == 97 and ' '.to_code() == 32 and '\n'.to_code() == 10
                and 'é'.to_code() == 233 and '\0'.to_code() == 0
                and '\u00e9' == 'é' and '\u00E9'.to_code() == 233 and '\x41' == 'A'
                and restored(char_from_code(97), 97) and restored(char_from_code(0), 0)
                and restored(char_from_code(0x1F600), 0x1F600)
                and char_from_code(-1) == null and char_from_code(0xD800) == null
                and char_from_code(0xDFFF) == null and char_from_code(0x110000) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
    assert_eq!(common::run_value(&code_is("'é'", 233)).unwrap(), 42);
}

#[test]
fn bad_character_escapes_are_rejected() {
    for source in [
        r"fn main() { '\uD800' }",
        r"fn main() { '\u{41}' }",
        r"fn main() { '\x' }",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}

#[test]
fn the_wrong_receiver_or_argument_is_rejected() {
    for source in [
        r#"fn main() { "a".to_code() }"#,
        r#"fn main() { char_from_code("233") }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
