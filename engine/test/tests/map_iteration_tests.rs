//! Map traversal preserves precise pair types and independent shallow snapshots.

mod common;

fn executes_42(source: &str) {
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        42,
        "{source}"
    );
}

#[test]
fn empty_snapshots_and_iterators_report_done_repeatedly() {
    executes_42(
        r#"
fn main() {
    let map = Map()
    let keys: List = map.keys()
    let values: List = map.values()
    let entries: List = map.entries()
    let iter: std.collections.MapIterator = map.into_iter()
    let first: IterationStep((String, Any)) = iter.next()
    let second: IterationStep((String, Any)) = iter.next()
    let first_done = first match { IterationStep((String, Any)).done => true, _ => false }
    let second_done = second match { IterationStep((String, Any)).done => true, _ => false }
    let count: i64 = 0
    for _ in map { count += 1 }
    if keys.len() == 0 and values.len() == 0 and entries.len() == 0
        and count == 0 and first_done and second_done { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn keys_values_and_entries_have_exact_contents_including_null_and_unit() {
    executes_42(
        r#"
typealias Pair = (String, Any)
fn main() {
    let map = Map()
    map("number") = 42
    map("null") = null
    map("unit") = ()
    let keys = map.keys()
    let values = map.values()
    let entries = map.entries()
    let unseen = Map()
    unseen("number") = true
    unseen("null") = true
    unseen("unit") = true
    for raw in keys {
        let key: String = raw.as(String)
        if not unseen.contains(key) { return 0 }
        unseen.remove(key)
    }
    let numbers: i64 = 0
    let nulls: i64 = 0
    let units: i64 = 0
    for value in values {
        if value == 42 { numbers += 1 }
        else if value == null { nulls += 1 }
        else if value == () { units += 1 }
        else { return 0 }
    }
    let restored = Map()
    for raw in entries {
        if type_of(raw) != Pair { return 0 }
        let pair: (String, Any) = raw.as((String, Any))
        if restored.contains(pair.0) { return 0 }
        restored(pair.0) = pair.1
    }
    if keys.len() == 3 and unseen.len() == 0 and values.len() == 3
        and numbers == 1 and nulls == 1 and units == 1 and entries.len() == 3
        and restored.len() == 3 and restored("number") == 42
        and restored.contains("null") and restored("null") == null
        and restored.contains("unit") and restored("unit") == () { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn returned_lists_are_independent_snapshots_after_replace_delete_and_add() {
    executes_42(
        r#"
fn main() {
    let map = Map()
    map("number") = 42
    map("null") = null
    map("unit") = ()
    let keys = map.keys()
    let values = map.values()
    let entries = map.entries()
    map("number") = 99
    map.remove("null")
    map("added") = false
    let old_keys = Map()
    for key in keys { old_keys(key.as(String)) = true }
    let numbers: i64 = 0
    let nulls: i64 = 0
    let units: i64 = 0
    for value in values {
        if value == 42 { numbers += 1 }
        else if value == null { nulls += 1 }
        else if value == () { units += 1 }
        else { return 0 }
    }
    let old_entries = Map()
    for raw in entries {
        let (key, value): (String, Any) = raw.as((String, Any))
        old_entries(key) = value
    }
    keys.push("local")
    values.push(true)
    entries.pop()
    if old_keys.len() == 3 and old_keys.contains("number")
        and old_keys.contains("null") and old_keys.contains("unit")
        and numbers == 1 and nulls == 1 and units == 1
        and old_entries.len() == 3 and old_entries("number") == 42
        and old_entries.contains("null") and old_entries("null") == null
        and old_entries("unit") == () and map.len() == 3
        and map("number") == 99 and map("added") == false
        and not map.contains("null") and not map.contains("local") { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn iterator_cursors_are_independent_and_keep_precise_step_types() {
    executes_42(
        r#"
fn main() {
    let map = Map()
    map("answer") = 42
    let first = map.into_iter()
    let second = map.into_iter()
    let first_value = first.next() match {
        IterationStep((String, Any)).yielded((key, value)) => if key == "answer" and value == 42 { true } else { false },
        _ => false
    }
    let first_done = first.next() match { IterationStep((String, Any)).done => true, _ => false }
    let second_value = second.next() match {
        IterationStep((String, Any)).yielded((key, value)) => if key == "answer" and value == 42 { true } else { false },
        _ => false
    }
    let second_done = second.next() match { IterationStep((String, Any)).done => true, _ => false }
    let still_done = first.next() match { IterationStep((String, Any)).done => true, _ => false }
    if first_value and first_done and second_value and second_done and still_done { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn into_iter_freezes_pairs_before_replace_delete_and_add() {
    executes_42(
        r#"
fn main() {
    let map = Map()
    map("number") = 42
    map("null") = null
    map("unit") = ()
    let iter = map.into_iter()
    map("number") = 99
    map.remove("null")
    map("added") = true
    let restored = Map()
    let count: i64 = 0
    for (key, value) in iter {
        if restored.contains(key) { return 0 }
        restored(key) = value
        count += 1
    }
    let done = iter.next() match { IterationStep((String, Any)).done => true, _ => false }
    let still_done = iter.next() match { IterationStep((String, Any)).done => true, _ => false }
    if count == 3 and restored.len() == 3 and restored("number") == 42
        and restored.contains("null") and restored("null") == null
        and restored.contains("unit") and restored("unit") == ()
        and not restored.contains("added") and done and still_done { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn snapshot_values_and_iterator_payloads_share_referenced_objects() {
    executes_42(
        r#"
struct Box { value: i64 }
fn main() {
    let object = Box { value: 1 }
    let map = Map()
    map("object") = object
    let values = map.values()
    let entries = map.entries()
    let iter = map.into_iter()
    object.value = 20
    map.remove("object")
    map("object") = Box { value: 99 }
    let value: Box = values(0).as(Box)
    let pair: (String, Any) = entries(0).as((String, Any))
    let entry_value: Box = pair.1.as(Box)
    if value.value != 20 or entry_value.value != 20 { return 0 }
    value.value = 42
    let shared = iter.next() match {
        IterationStep((String, Any)).yielded((key, payload)) => if key == "object" and payload.as(Box).value == 42 { true } else { false },
        _ => false
    }
    if shared and object.value == 42 and entry_value.value == 42
        and map("object").as(Box).value == 99 { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn for_tuple_bindings_and_fallible_patterns_filter_exact_pairs() {
    for source in [
        r#"
fn takes_string(key: String) -> i64 { key.len() }
fn main() {
    let map = Map()
    map("answer") = 42
    map("null") = null
    map("unit") = ()
    let count: i64 = 0
    let key_lengths: i64 = 0
    let nulls: i64 = 0
    let units: i64 = 0
    for (key, value) in map {
        let typed_key: String = key
        let typed_value: Any = value
        count += 1
        key_lengths += takes_string(typed_key)
        if typed_value == null { nulls += 1 }
        if typed_value == () { units += 1 }
    }
    if count == 3 and key_lengths == 14 and nulls == 1 and units == 1 { 42 } else { 0 }
}
"#,
        r#"
fn main() {
    let map = Map()
    map("answer") = 42
    map("null") = null
    map("unit") = ()
    let nulls: i64 = 0
    let units: i64 = 0
    let total: i64 = 0
    for (_, null) in map { nulls += 1 }
    for (_, ()) in map { units += 1 }
    for (key, value) if key == "answer" in map { total += value.as(i64) }
    if nulls == 1 and units == 1 { total } else { 0 }
}
"#,
    ] {
        executes_42(source);
    }
}

#[test]
fn break_preserves_the_remaining_cursor_and_continue_advances_it() {
    executes_42(
        r#"
fn main() {
    let map = Map()
    map("a") = 10
    map("b") = 20
    map("c") = 30
    let iter = map.into_iter()
    let seen = Map()
    let first_count: i64 = 0
    for (key, value) in iter {
        seen(key) = value
        first_count += 1
        break
    }
    let remaining: i64 = 0
    for (key, value) in iter {
        if seen.contains(key) { return 0 }
        seen(key) = value
        remaining += 1
        continue
        return 0
    }
    let selected: i64 = 0
    let total: i64 = 0
    for (key, value) in map {
        if key == "b" { continue }
        selected += 1
        total += value.as(i64)
    }
    if first_count == 1 and remaining == 2 and seen.len() == 3
        and seen("a") == 10 and seen("b") == 20 and seen("c") == 30
        and selected == 2 and total == 40 { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn string_keys_and_precise_pair_steps_reject_invalid_static_uses() {
    for (source, expected) in [
        (
            "fn main(){for (key,value) in Map(){let number:i64=key};42}",
            "type mismatch: expected `i64`, found `String`",
        ),
        (
            "fn takes_number(value:i64){};fn main(){for (key,value) in Map(){takes_number(key)};42}",
            "type mismatch in argument: expected `i64`, found `String`",
        ),
        (
            "fn main(){let step:IterationStep((i64,Any))=Map().into_iter().next();42}",
            "type mismatch: expected `IterationStep`, found `IterationStep`",
        ),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "{source}: {:?}",
            compiled.diagnostics
        );
        assert!(compiled.codegen_output.functions.is_empty(), "{source}");
        assert!(compiled.into_artifact().is_err(), "emitted {source}");
    }
}
