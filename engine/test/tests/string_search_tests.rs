//! Last-match search, non-overlapping counts, and affix removal.

mod common;

#[test]
fn rfind_count_and_strip_use_scalar_indexes() {
    let source = r#"
        fn text(value: ?String) -> String {
            value match {
                found? => found,
                _ => "none",
            }
        }
        fn main() {
            let word = "héllo"
            let matched = word.rfind("l") == 3 and word.rfind("él") == 1 and word.rfind("") == 5
                and "".rfind("a") == null and "aaa".rfind("aa") == 1
                and "aaa".count("a") == 3 and "aaa".count("aa") == 1
                and "".count("") == 1 and "a".count("") == 2 and word.count("l") == 2
                and text(word.strip_prefix("hé")) == "llo"
                and text(word.strip_prefix("")) == word
                and text(word.strip_prefix("x")) == "none"
                and text(word.strip_suffix("llo")) == "hé"
                and text(word.strip_suffix("héllo")) == ""
                and text(word.strip_prefix("héllo")) == ""
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
