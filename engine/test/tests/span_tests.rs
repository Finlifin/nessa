//! Unicode string classes and shallow list copies.

mod common;

#[test]
fn unicode_and_ascii_spans_match() {
    let source = r#"
        fn main() {
            let unicode = "".is_numeric() and "123".is_numeric() and (not "12a".is_numeric())
                and "\u00B2".is_numeric()
                and "Ab".is_alphabetic() and (not "A1".is_alphabetic())
                and "\u00E9".is_alphabetic() and (not "\u00E9".is_ascii_alphabetic())
                and "\u00E91".is_alphanumeric() and (not "A ".is_alphanumeric())
                and "\u00E9".is_lowercase() and (not "\u00C9".is_lowercase())
                and "\u00C9".is_uppercase() and (not "\u00C9".is_ascii_uppercase())
                and " \u00A0".is_whitespace() and (not " \u00A0".is_ascii_whitespace())
                and "\n".is_control() and (not " ".is_control())
            let ascii = "A!".is_ascii_graphic() and (not "A ".is_ascii_graphic())
                and ",.;".is_ascii_punctuation() and (not "a".is_ascii_punctuation())
                and "\n\t".is_ascii_control() and (not " ".is_ascii_control())
                and "".is_ascii_graphic() and "".is_control()
            if unicode and ascii { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn copies_leave_the_source_unchanged() {
    let source = r#"
        fn main() {
            let xs = [1, 2, 3, 4]
            let copied = xs.copy()
            copied.set(0, 9)
            let reversed = xs.reverse()
            let joined = xs.concat([5, 6])
            let self_joined = xs.concat(xs)
            let matched = copied == [9, 2, 3, 4]
                and xs == [1, 2, 3, 4]
                and reversed == [4, 3, 2, 1]
                and joined == [1, 2, 3, 4, 5, 6]
                and self_joined == [1, 2, 3, 4, 1, 2, 3, 4]
                and [1].concat([]).len() == 1
                and [].concat([1]) == [1]
                and [].reverse().len() == 0
                and [].copy().len() == 0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn swap_and_clear_edit_in_place() {
    let source = r#"
        fn main() {
            let xs = [1, 2, 3]
            xs.swap(0, 2)
            let swapped = xs == [3, 2, 1]
            xs.swap(1, 1)
            let same = xs == [3, 2, 1]
            xs.clear()
            let cleared = xs.len() == 0
            xs.push(7)
            let reused = xs == [7]
            if swapped and same and cleared and reused { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_swap_past_the_end_is_rejected() {
    let error = common::run_value("fn main() { [1].swap(0, 1) }").unwrap_err();
    assert!(error.contains("InvalidIndex"), "{error}");
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.is_numeric() }"#,
        r#"fn main() { true.is_whitespace() }"#,
        r#"fn main() { "a".copy() }"#,
        r#"fn main() { [1].swap(-1, 0) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
