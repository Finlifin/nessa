//! Limited string edits and the remaining character classes.

mod common;

#[test]
fn trim_replace_and_split_limits_keep_rust_order() {
    let source = r#"
        fn main() {
            let matched = "xxxy".trim_start_matches("x") == "y"
                and "abxx".trim_end_matches("x") == "ab"
                and "aba".trim_matches("a") == "b"
                and "abc".trim_matches("") == "abc"
                and "aaa".replacen("a", "b", 2) == "bba"
                and "aaa".replacen("a", "b", 0) == "aaa"
                and "ab".replacen("", "-", 2) == "-a-b"
                and "a,b,c".splitn(",", 2) == ["a", "b,c"]
                and "a,b,c".splitn(",", 0).is_empty()
                and "a,b,c".rsplit(",") == ["c", "b", "a"]
                and "a,b,c".rsplitn(",", 2) == ["c", "a,b"]
                and '\n'.is_ascii_control() and '\n'.is_control()
                and '\u0080'.is_control() and not ('\u0080'.is_ascii_control())
                and ' '.is_ascii_whitespace() and not (' '.is_ascii_graphic())
                and 'A'.is_ascii_uppercase() and 'a'.is_ascii_lowercase()
                and 'a'.is_ascii_alphanumeric() and 'a'.is_ascii_graphic()
                and '!'.is_ascii_punctuation() and not ('é'.is_ascii_graphic())
                and '\0'.is_ascii_control()
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn negative_counts_and_empty_separators_are_runtime_errors() {
    for (source, message) in [
        (
            r#"fn main() { "a".replacen("a", "b", -1) }"#,
            "replace count must not be negative",
        ),
        (
            r#"fn main() { "a".splitn(",", -1) }"#,
            "split limit must not be negative",
        ),
        (
            r#"fn main() { "a".rsplit("") }"#,
            "split separator must not be empty",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}

#[test]
fn the_new_methods_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.trim_matches("a") }"#,
        r#"fn main() { 'a'.replacen("a", "b", 1) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
