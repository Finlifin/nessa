//! Native lines, once-splits, map snapshots, and list grouping.

mod common;

#[test]
fn find_char_and_strip_affixes() {
    let source = r#"
        fn main() {
            let matched = "héllo".find_char('l') == 2
                and "héllo".rfind_char('l') == 3
                and "héllo".find_char('z') == null
                and "".find_char('a') == null
                and "héllo".count_char('l') == 2
                and "héllo".count_char('é') == 1
                and "héllo".strip_prefix("hé") == "llo"
                and "héllo".strip_prefix("") == "héllo"
                and "héllo".strip_prefix("x") == null
                and "héllo".strip_suffix("lo") == "hél"
                and "héllo".strip_suffix("x") == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn map_copy_and_entries_are_independent_snapshots() {
    let source = r#"
        fn main() {
            let m = Map()
            m.set("a", 1)
            m.set("b", 2)
            let copied = m.copy()
            m.set("a", 9)
            copied.set("c", 3)
            let matched = copied.get("a") == 1
                and m.get("a") == 9
                and copied.contains("c")
                and (not m.contains("c"))
                and m.is_empty() == false
                and Map().is_empty()
                and copied.len() == 3
                and m.len() == 2
                and m.entries().len() == 2
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn empty_separator_is_the_split_error() {
    for source in [
        r#"fn main() { "a".split_once(""); 0 }"#,
        r#"fn main() { "a".rsplit_once(""); 0 }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("split separator must not be empty"),
            "{source}: {error}"
        );
    }
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.lines() }"#,
        r#"fn main() { true.split_once("a") }"#,
        r#"fn main() { 1.enumerate() }"#,
        r#"fn main() { true.copy() }"#,
        r#"fn main() { 1.find_char('a') }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
