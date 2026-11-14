//! Float remainder, encoding lengths, and escape text.

mod common;

#[test]
fn remainder_lengths_and_escapes_match_rust() {
    let source = r#"
        fn main() {
            let wide = char_from_code(0x1F600) match { face? => face.utf16_len() == 2, _ => false }
            let matched = (-0.0).signum() == -1 and (0.0).signum() == 1
                and (0.0).recip().is_infinite() and (2.0).mul_add(3.0, 4.0) == 10
                and (-7.0).div_euclid(3.0) == -3 and (-7.0).rem_euclid(3.0) == 2
                and (log2(-1).signum()).is_nan()
                and 'a'.utf8_len() == 1 and 'é'.utf8_len() == 2 and 'é'.utf16_len() == 1
                and wide
                and '\n'.escape_debug() == "\\n" and 'a'.escape_debug() == "a"
                and '\0'.escape_debug() == "\\0" and 'é'.escape_debug() == "é"
                and '\n'.escape_default() == "\\n" and ' '.escape_default() == " "
                and '\n'.escape_unicode().starts_with("\\u") and '\n'.escape_unicode().char_len() == 5
                and 'a'.escape_unicode().char_len() == 6
                and "a\n".escape_debug() == "a\\n"
                and "é".escape_default().char_len() == 6
                and "a,b,c".split_inclusive(",") == ["a,", "b,", "c"]
                and "aaa".find_all("aa") == ["aa"]
                and "ab".find_all("").len() == 3
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn an_empty_inclusive_separator_is_rejected() {
    let error = common::run_value(r#"fn main() { "a".split_inclusive("") }"#).unwrap_err();
    assert!(
        error.contains("split separator must not be empty"),
        "{error}"
    );
}

#[test]
fn the_new_methods_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.signum() }"#,
        r#"fn main() { 1.escape_debug() }"#,
        r#"fn main() { true.find_all("a") }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
