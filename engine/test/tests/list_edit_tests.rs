//! In-place List insert, remove, swap, clear and append.

mod common;

#[test]
fn edits_shift_elements_and_share_the_list_identity() {
    let source = r#"
        fn main() {
            let xs = ["a", "b", "c"]
            xs.insert(0, "z")
            xs.insert(xs.len().as(usize), "w")
            xs.insert(2, "m")
            let mid = xs.remove(2)
            let front = xs.remove(0)
            let back = xs.remove((xs.len() - 1).as(usize))
            xs.swap(0, 1)
            xs.swap(0, 0)
            let alias = xs
            let grown = []
            var index = 0
            while index < 30 { grown.insert(0, index); index = index + 1 }
            let empty = []
            empty.insert(0, "only")
            let only = empty.remove(0)
            let matched = xs.join(",") == "b,a,c" and alias.join(",") == "b,a,c" and mid == "m" and front == "z" and back == "w" and grown.len() == 30 and grown.first() == 29 and grown.last() == 0 and only == "only" and empty.len() == 0
            if matched {
                xs.clear()
                if alias.len() == 0 and xs.is_empty() { 42 } else { 0 }
            } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn indexes_outside_the_list_are_invalid() {
    for source in [
        "fn main() { [1].insert(2, 9); 0 }",
        "fn main() { [1].remove(1); 0 }",
        "fn main() { [].remove(0); 0 }",
        "fn main() { [1].swap(0, 1); 0 }",
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("InvalidIndex"), "{source}: {error}");
    }
    let compiled = driver::Driver::new().compile("fn main() { [1].remove(-1) }");
    assert!(compiled.has_errors, "negative index should be rejected");
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("out of range for `usize`")),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn append_copies_a_snapshot_and_can_append_itself() {
    let source = r#"
        fn main() {
            let xs = ["a", "b"]
            let extra = ["c"]
            xs.append(extra)
            extra.push("d")
            xs.append(xs)
            xs.append([])
            if xs.join(",") == "a,b,c,a,b,c" and extra.join(",") == "c,d" { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
