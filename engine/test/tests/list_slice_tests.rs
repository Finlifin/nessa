//! List windows, exact chunks, rotation, and in-place edits.

mod common;

#[test]
fn windows_and_chunks_copy_without_changing_the_source() {
    let source = r#"
        fn main() {
            let xs = [1, 2, 3, 4, 5]
            let matched = xs.windows(3) == [[1, 2, 3], [2, 3, 4], [3, 4, 5]]
                and xs.windows(6).len() == 0
                and xs.windows(1) == [[1], [2], [3], [4], [5]]
                and [].windows(1).len() == 0
                and xs.chunks_exact(2) == [[1, 2], [3, 4]]
                and xs.chunks_remainder(2) == [5]
                and xs.rchunks(2) == [[4, 5], [2, 3], [1]]
                and xs.rchunks_exact(2) == [[4, 5], [2, 3]]
                and xs.rchunks_remainder(2) == [1]
                and xs.chunks_exact(1) == [[1], [2], [3], [4], [5]]
                and xs.rchunks(1) == [[5], [4], [3], [2], [1]]
                and xs.chunks_remainder(5).len() == 0
                and xs.rchunks_remainder(6) == [1, 2, 3, 4, 5]
                and xs == [1, 2, 3, 4, 5]
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn split_rotate_and_repeat_keep_the_source() {
    let source = r#"
        fn main() {
            let xs = [1, 2, 3, 4, 5]
            let matched = xs.split_at(2) == [[1, 2], [3, 4, 5]]
                and xs.split_at(0) == [[], [1, 2, 3, 4, 5]]
                and xs.split_at(5) == [[1, 2, 3, 4, 5], []]
                and xs.rotate_left(2) == [3, 4, 5, 1, 2]
                and xs.rotate_right(2) == [4, 5, 1, 2, 3]
                and xs.rotate_left(0) == xs
                and xs.rotate_left(5) == xs
                and xs.rotate_right(5) == xs
                and [].rotate_left(0).len() == 0
                and xs.repeat(2) == [1, 2, 3, 4, 5, 1, 2, 3, 4, 5]
                and xs.repeat(0).len() == 0
                and [].repeat(4).len() == 0
                and xs == [1, 2, 3, 4, 5]
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn in_place_edits_change_only_the_receiver() {
    let source = r#"
        fn main() {
            let xs = [1, 2, 3, 4]
            let removed = xs.swap_remove(1)
            let after_remove = removed == 2 and xs == [1, 4, 3]
            let tail = xs.split_off(2)
            let after_split = tail == [3] and xs == [1, 4]
            xs.truncate(1)
            let after_truncate = xs == [1]
            xs.truncate(5)
            let still_one = xs == [1]
            xs.resize(3, 8)
            let after_resize = xs == [1, 8, 8]
            xs.fill(2)
            let after_fill = xs == [2, 2, 2]
            xs.resize(0, 9)
            let cleared = xs.len() == 0
            if after_remove and after_split and after_truncate and still_one
                and after_resize and after_fill and cleared { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_zero_window_is_rejected() {
    let source = r#"fn main() { [1].windows(0) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("window size must not be zero"), "{error}");
}

#[test]
fn a_zero_chunk_is_rejected() {
    let source = r#"fn main() { [1].rchunks(0) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("chunk size must not be zero"), "{error}");
}

#[test]
fn a_long_rotation_is_rejected() {
    let source = r#"fn main() { [1, 2].rotate_left(3) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("rotate distance is out of range"), "{error}");
}

#[test]
fn a_negative_repeat_is_rejected() {
    let source = r#"fn main() { [1].repeat(-1) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(
        error.contains("repeat count must not be negative"),
        "{error}"
    );
}

#[test]
fn an_index_past_the_end_is_rejected() {
    for source in [
        r#"fn main() { [1].split_at(2) }"#,
        r#"fn main() { [1].swap_remove(1) }"#,
        r#"fn main() { [1].split_off(2) }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("InvalidIndex"), "{source}: {error}");
    }
}

#[test]
fn the_new_list_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.windows(1) }"#,
        r#"fn main() { true.fill(1) }"#,
        r#"fn main() { [1].rotate_left(-1) }"#,
        r#"fn main() { [1].resize(-1, 0) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
