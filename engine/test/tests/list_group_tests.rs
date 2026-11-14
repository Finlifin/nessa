//! `chunks` groups elements. `flatten` expands one level of nested lists.

mod common;

#[test]
fn chunks_and_flatten_copy_elements_without_changing_the_source() {
    let source = r#"
        fn main() {
            let xs = [1, 2, 3, 4, 5]
            let parts = xs.chunks(2)
            parts(0).as(List).set(0, 9)
            let inner = [1, 2]
            let flat = [inner, [3], []].flatten()
            flat.set(0, 8)
            let matched = parts == [[9, 2], [3, 4], [5]] and xs == [1, 2, 3, 4, 5]
                and [].chunks(3).len() == 0 and [1].chunks(3) == [[1]]
                and flat == [8, 2, 3] and inner == [1, 2]
                and [[1, 2], [3]].flatten() == [1, 2, 3]
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn an_empty_chunk_size_panics_and_a_non_list_cannot_be_flattened() {
    let panic = common::run_value("fn main() { [1].chunks(0); 0 }").unwrap_err();
    assert!(panic.contains("chunk size must not be zero"), "{panic}");
    let error = common::run_value("fn main() { [1].flatten(); 0 }").unwrap_err();
    assert!(error.contains("TypeError"), "{error}");
    let compiled = driver::Driver::new().compile("fn main() { [1].chunks(-1) }");
    assert!(
        compiled.has_errors,
        "negative chunk size should be rejected"
    );
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("out of range for `usize`")),
        "{:?}",
        compiled.diagnostics
    );
}
