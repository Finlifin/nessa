//! String bytes, scalar windows, character tests, and native list join/append.

mod common;

#[test]
fn bytes_and_char_indices_follow_utf8() {
    let source = r#"
        fn main() {
            let matched = "abc".bytes() == [97, 98, 99]
                and "abc".char_indices() == [0, 1, 2]
                and "é".bytes() == [195, 169]
                and "é".char_indices() == [0]
                and "aé".char_indices() == [0, 1]
                and "".bytes() == []
                and "".char_indices() == []
                and "abc".bytes() == "abc".encode_utf8()
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn take_drop_and_ends_use_unicode_scalars() {
    let source = r#"
        fn main() {
            let matched = "aéb".take(2) == "aé"
                and "aéb".drop(1) == "éb"
                and "aéb".take(8) == "aéb"
                and "aéb".drop(8) == ""
                and "".take(1) == ""
                and "aéb".first() == 'a'
                and "aéb".last() == 'b'
                and "".first() == null
                and "".last() == null
                and "aéb".starts_with_char('a')
                and "aéb".ends_with_char('b')
                and "aéb".contains_char('é')
                and (not "aéb".contains_char('z'))
                and "aéa".replace_char('a', 'x') == "xéx"
                and 'x'.repeat(3) == "xxx"
                and 'é'.repeat(0) == ""
                and "".is_empty()
                and (not "a".is_empty())
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn list_append_join_and_is_empty() {
    let source = r#"
        fn main() {
            let xs = [1, 2]
            xs.append([3, 4])
            let same = [7]
            same.append(same)
            let words = ["a", "b"]
            words.append(["c"])
            let matched = xs == [1, 2, 3, 4]
                and same == [7, 7]
                and words.join("-") == "a-b-c"
                and [].join(",") == ""
                and [].is_empty()
                and (not [0].is_empty())
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn negative_counts_and_non_string_join_fail() {
    for source in [
        r#"fn main() { "a".take(-1) }"#,
        r#"fn main() { "a".drop(-1) }"#,
        r#"fn main() { 'a'.repeat(-1) }"#,
        r#"fn main() { [1, 2].join(",") }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("must not be negative") || error.contains("TypeError"),
            "{source}: {error}"
        );
    }
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.bytes() }"#,
        r#"fn main() { 1.char_indices() }"#,
        r#"fn main() { true.take(1) }"#,
        r#"fn main() { 1.append([]) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
