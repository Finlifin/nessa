//! std String text operations: exact results, Unicode behavior and rejected domains.

mod common;

#[test]
fn predicates_and_trimming_return_exact_values() {
    for source in [
        r#"fn main() { if "  a b \t\n".trim() == "a b" { 42 } else { 0 } }"#,
        r#"fn main() { if "  ab  ".trim_start() == "ab  " and "  ab  ".trim_end() == "  ab" { 42 } else { 0 } }"#,
        // U+3000 IDEOGRAPHIC SPACE is Unicode White_Space.
        "fn main() { if \"\u{3000}x\u{3000}\".trim() == \"x\" { 42 } else { 0 } }",
        r#"fn main() { if "   ".trim().is_empty() and not "x".is_empty() and "".is_empty() { 42 } else { 0 } }"#,
        r#"fn main() { let s = "hello world"; if s.contains("o w") and s.contains("") and not s.contains("O") { 42 } else { 0 } }"#,
        r#"fn main() { let s = "nessa"; if s.starts_with("ne") and s.ends_with("sa") and s.starts_with("") and not s.ends_with("ne") { 42 } else { 0 } }"#,
        r#"fn main() { if "é".starts_with("é") and not "e".contains("é") { 42 } else { 0 } }"#,
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn case_mapping_repeat_and_replace_produce_exact_strings() {
    for source in [
        r#"fn main() { if "Hello, World".to_upper() == "HELLO, WORLD" and "Hello, World".to_lower() == "hello, world" { 42 } else { 0 } }"#,
        // Full case mapping may change the byte length.
        r#"fn main() { let upper = "straße".to_upper(); if upper == "STRASSE" and upper.len() == 7 and "ÀÉ".to_lower() == "àé" { 42 } else { 0 } }"#,
        r#"fn main() { if "ab".repeat(3) == "ababab" and "ab".repeat(0) == "" and "".repeat(1000) == "" { 42 } else { 0 } }"#,
        r#"fn main() { if "a-b-c".replace("-", "+") == "a+b+c" and "aaa".replace("aa", "b") == "ba" and "abc".replace("x", "y") == "abc" { 42 } else { 0 } }"#,
        r#"fn main() { if "ab".replace("", "-") == "-a-b-" and "héé".replace("é", "e") == "hee" { 42 } else { 0 } }"#,
        r#"fn main() { if "  Nessa  ".trim().to_lower().replace("ss", "s") ++ "!" == "nesa!" { 42 } else { 0 } }"#,
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn split_and_chars_build_fresh_lists_in_order() {
    for source in [
        r#"fn main() { let parts = "a,b,,c".split(","); if parts.len() == 4 and parts(0) == "a" and parts(1) == "b" and parts(2) == "" and parts(3) == "c" { 42 } else { 0 } }"#,
        r#"fn main() { let parts = "a::b".split("::"); if parts.len() == 2 and parts(0) == "a" and parts(1) == "b" { 42 } else { 0 } }"#,
        r#"fn main() { let parts = "".split(","); if parts.len() == 1 and parts(0) == "" { 42 } else { 0 } }"#,
        r#"fn main() { let parts = ",".split(","); if parts.len() == 2 and parts(0) == "" and parts(1) == "" { 42 } else { 0 } }"#,
        r#"fn main() { let cs = "héllo".chars(); if cs.len() == 5 and cs(1).as(char) == 'é' and cs(4).as(char) == 'o' and cs(0)'type == char { 42 } else { 0 } }"#,
        r#"fn main() { if "".chars().len() == 0 { 42 } else { 0 } }"#,
        // Lists returned by split are independent of each other and of the source.
        r#"fn main() { let s = "x,y"; let a = s.split(","); let b = s.split(","); a(0) = "changed"; if b(0) == "x" and s == "x,y" { 42 } else { 0 } }"#,
        r#"fn main() { "a b c".split(" ").fold(0, |acc, x| acc + x.as(String).len()).as(i64) + 39 }"#,
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn invalid_arguments_are_runtime_errors_with_their_domain() {
    for (source, expected) in [
        (
            r#"fn main() { "ab".repeat(-1).len() }"#,
            "repeat count must not be negative",
        ),
        (
            r#"fn main() { "a,b".split("").len() }"#,
            "split separator must not be empty",
        ),
        (
            r#"fn main() { "a".split_once(""); 0 }"#,
            "split separator must not be empty",
        ),
        (
            r#"fn main() { "a".rsplit_once(""); 0 }"#,
            "split separator must not be empty",
        ),
        // Size checks happen before the host builds the result.
        (
            r#"fn main() { "ab".repeat(9223372036854775807).len() }"#,
            "ObjectTooLarge",
        ),
        (
            r#"fn main() { "x".repeat(600000).len() }"#,
            "ObjectTooLarge",
        ),
        (
            r#"fn main() { "x".repeat(300000).replace("x", "xx").len() }"#,
            "ObjectTooLarge",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(expected), "{source}: {error}");
    }
    for source in [
        r#"fn main() { "ab".repeat("2") }"#,
        r#"fn main() { "ab".contains(1) }"#,
        r#"fn main() { 42.trim() }"#,
        r#"fn main() { "ab".split() }"#,
        r#"fn main() { "ab".char_at("0") }"#,
        r#"fn main() { "ab".slice(0) }"#,
        r#"fn main() { "a".classify() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}

#[test]
fn character_indexes_follow_unicode_scalars_and_lines_drop_endings() {
    for source in [
        r#"fn main() { let s = "héllo"; if s.len() == 6 and s.char_len() == 5 and s.char_at(1) == 'é' and s.char_at(4) == 'o' { 42 } else { 0 } }"#,
        r#"fn main() { let s = "héllo"; if s.char_at(-1) == null and s.char_at(5) == null and s.find("él") == 1 and s.find("z") == null and s.find("") == 0 { 42 } else { 0 } }"#,
        r#"fn main() { let s = "héllo"; if s.slice(1, 3) == "él" and s.slice(0, s.char_len()) == s and s.slice(5, 5) == "" and s.slice(2, 2) == "" { 42 } else { 0 } }"#,
        r#"fn main() { if "a\r\nb\nc\n".lines().join("|") == "a|b|c" and "".lines().len() == 0 and "\n".lines().len() == 1 and "\n".lines()(0) == "" and "a\rb".lines().join("|") == "a|b" { 42 } else { 0 } }"#,
        r#"fn main() { if "a\n\nb".lines().len() == 3 and "a\n\nb".lines()(1) == "" { 42 } else { 0 } }"#,
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    for source in [
        r#"fn main() { "héllo".slice(1, 9).len() }"#,
        r#"fn main() { "héllo".slice(3, 1).len() }"#,
        r#"fn main() { "héllo".slice(-1, 1).len() }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("string slice is out of range"),
            "{source}: {error}"
        );
    }
}

#[test]
fn char_classes_follow_unicode_and_ascii_case_mapping_is_narrow() {
    for source in [
        r#"fn main() { if ' '.is_whitespace() and '\n'.is_whitespace() and '\t'.is_whitespace() and "\u3000".chars()(0).as(char).is_whitespace() and not "\u200b".chars()(0).as(char).is_whitespace() { 42 } else { 0 } }"#,
        r#"fn main() { let han = "中".chars()(0).as(char); if 'é'.is_alphabetic() and han.is_alphabetic() and not '3'.is_alphabetic() and not ' '.is_alphabetic() { 42 } else { 0 } }"#,
        r#"fn main() { let half = "\u00bd".chars()(0).as(char); if '3'.is_numeric() and half.is_numeric() and not 'é'.is_numeric() and 'é'.is_alphanumeric() and '3'.is_alphanumeric() and not ' '.is_alphanumeric() { 42 } else { 0 } }"#,
        r#"fn main() { if 'É'.is_uppercase() and 'é'.is_lowercase() and not 'A'.is_lowercase() and not 'é'.is_uppercase() { 42 } else { 0 } }"#,
        r#"fn main() { if 'a'.is_ascii() and '7'.is_ascii_digit() and 'F'.is_ascii_alphabetic() and 'f'.is_ascii_hexdigit() and 'F'.is_ascii_hexdigit() and not 'é'.is_ascii() and not 'é'.is_ascii_digit() and not 'g'.is_ascii_hexdigit() and not '3'.is_ascii_alphabetic() { 42 } else { 0 } }"#,
        r#"fn main() { if 'é'.to_ascii_uppercase() == 'é' and 'b'.to_ascii_uppercase() == 'B' and 'B'.to_ascii_lowercase() == 'b' and ' '.to_ascii_lowercase() == ' ' { 42 } else { 0 } }"#,
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn split_once_keeps_the_unmatched_tail() {
    let source = r#"
        fn show(s: String, sep: String) -> String {
            s.split_once(sep) match {
                parts? => parts.0 ++ "|" ++ parts.1,
                _ => "none",
            }
        }
        fn main() {
            let matched = show("a=b=c", "=") == "a|b=c" and show("abc", "=") == "none" and show("=", "=") == "|" and show("a=", "=") == "a|" and show("=a", "=") == "|a" and show("héllo", "é") == "h|llo" and show("a::b::c", "::") == "a|b::c" and show("aaa", "aa") == "|a" and show("", ",") == "none"
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn rsplit_once_keeps_the_unmatched_head() {
    let source = r#"
        fn show(s: String, sep: String) -> String {
            s.rsplit_once(sep) match {
                parts? => parts.0 ++ "|" ++ parts.1,
                _ => "none",
            }
        }
        fn main() {
            let matched = show("a=b=c", "=") == "a=b|c" and show("abc", "=") == "none" and show("=", "=") == "|" and show("a=", "=") == "a|" and show("=a", "=") == "|a" and show("héllo", "l") == "hél|o" and show("a::b::c", "::") == "a::b|c" and show("aaa", "aa") == "a|" and show("", ",") == "none"
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_null_escape_is_a_real_scalar() {
    let source = r#"
        fn main() {
            let text = "a\0b"
            let matched = text.len() == 3 and text.char_len() == 3 and text.char_at(1) == '\0'
                and text.char_at(0) == 'a' and text.char_at(2) == 'b'
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
