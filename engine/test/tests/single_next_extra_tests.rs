//! Single-step iteration must preserve consumption, filtering and scoped bindings.

mod common;

const COUNTING_ITERATOR: &str = r#"
global next_calls: i64 = 0
struct Counter { index: i64, end: i64 }
impl Iterator for Counter {
    assoc Item: Type = i64
    pub fn next(self) -> IterationStep(i64) {
        next_calls = next_calls + 1
        if self.index < self.end {
            self.index = self.index + 1
            IterationStep(i64).yielded(self.index)
        } else { IterationStep(i64).done }
    }
}
"#;

fn assert_answer(source: &str) {
    assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
}

#[test]
fn next_is_called_once_per_yield_and_once_for_done() {
    for (end, calls, sum) in [(0, 1, 0), (3, 4, 6)] {
        let source = format!(
            "{COUNTING_ITERATOR}\nfn main() {{ let total: i64 = 0; let iter = Counter {{ index: 0, end: {end} }}; for x in iter {{ total = total + x }}; if next_calls == {calls} and total == {sum} {{ 42 }} else {{ 0 }} }}"
        );
        assert_answer(&source);
    }
}

#[test]
fn iterable_expression_and_conversion_each_execute_once() {
    let source = format!(
        r#"{COUNTING_ITERATOR}
global source_calls: i64 = 0
global conversion_calls: i64 = 0
struct Source {{}}
impl IntoIterator for Source {{
    assoc Iter: Type = Counter
    pub fn into_iter(self) -> Counter {{
        conversion_calls = conversion_calls + 1
        Counter {{ index: 0, end: 3 }}
    }}
}}
fn make_source() -> Source {{ source_calls = source_calls + 1; Source {{}} }}
fn main() {{
    let total: i64 = 0
    for x in make_source() {{ total = total + x }}
    if source_calls == 1 and conversion_calls == 1 and next_calls == 4 and total == 6 {{ 42 }} else {{ 0 }}
}}
"#
    );
    assert_answer(&source);
}

#[test]
fn break_does_not_request_another_step_and_continue_requests_one() {
    for (body, calls, total) in [
        ("total = total + x; break", 1, 1),
        ("if x == 2 { continue }; total = total + x", 4, 4),
    ] {
        let source = format!(
            "{COUNTING_ITERATOR}\nfn main() {{ let total: i64 = 0; let iter = Counter {{ index: 0, end: 3 }}; for x in iter {{ {body} }}; if next_calls == {calls} and total == {total} {{ 42 }} else {{ 0 }} }}"
        );
        assert_answer(&source);
    }
}

#[test]
fn nested_labels_resume_the_selected_loop_without_extra_consumption() {
    let source = format!(
        r#"{COUNTING_ITERATOR}
fn main() {{
    let total: i64 = 0
    let outer = Counter {{ index: 0, end: 3 }}
    for: outer_loop x in outer {{
        let inner = Counter {{ index: 0, end: 3 }}
        for y in inner {{
            if x == 2 {{ continue outer_loop }}
            total = total + x + y
            if x == 3 {{ break outer_loop }}
            break
        }}
    }}
    if total == 6 and next_calls == 6 {{ 42 }} else {{ 0 }}
}}
"#
    );
    assert_answer(&source);
}

#[test]
fn tuple_loop_patterns_filter_nonmatching_items() {
    assert_answer(
        r#"
struct Pairs { index: i64 }
impl Iterator for Pairs {
    assoc Item: Type = (i64, i64)
    pub fn next(self) -> IterationStep((i64, i64)) {
        self.index = self.index + 1
        if self.index == 1 { IterationStep((i64, i64)).yielded((0, 99)) }
        else if self.index == 2 { IterationStep((i64, i64)).yielded((1, 20)) }
        else if self.index == 3 { IterationStep((i64, i64)).yielded((2, 99)) }
        else if self.index == 4 { IterationStep((i64, i64)).yielded((1, 22)) }
        else { IterationStep((i64, i64)).done }
    }
}
fn main() {
    let total: i64 = 0
    let iter = Pairs { index: 0 }
    for (1, value) in iter { total = total + value }
    if total == 42 and iter.index == 5 { 42 } else { 0 }
}
"#,
    );
}

#[test]
fn guard_loop_patterns_filter_nonmatching_items() {
    assert_answer(
        "fn main() { let total: i64 = 0; for value if value > 10 in [1, 20, 2, 22] { total = total + value }; total }",
    );
}

#[test]
fn enum_loop_patterns_filter_other_variants() {
    assert_answer(
        r#"
enum Entry { keep(value: i64), skip(value: i64) }
fn main() {
    let values = [Entry.skip(99), Entry.keep(20), Entry.skip(99), Entry.keep(22)]
    let total: i64 = 0
    for Entry.keep(value) in values { total = total + value }
    total
}
"#,
    );
}

#[test]
fn scoped_iterator_item_bindings_do_not_leak_between_modules() {
    assert_answer(
        r#"
struct Cursor { done: bool }
mod numbers {
    extend Iterator for Cursor {
        assoc Item: Type = i64
        pub fn next(self) -> IterationStep(i64) {
            if self.done { IterationStep(i64).done }
            else { self.done = true; IterationStep(i64).yielded(40) }
        }
    }
    pub fn answer() -> i64 {
        let total: i64 = 0
        let iter = Cursor { done: false }
        for item in iter { let number: i64 = item; total = total + number }
        total
    }
}
mod flags {
    extend Iterator for Cursor {
        assoc Item: Type = bool
        pub fn next(self) -> IterationStep(bool) {
            if self.done { IterationStep(bool).done }
            else { self.done = true; IterationStep(bool).yielded(true) }
        }
    }
    pub fn answer() -> i64 {
        let total: i64 = 0
        let iter = Cursor { done: false }
        for item in iter { let flag: bool = item; if flag { total = total + 2 } }
        total
    }
}
fn main() { numbers.answer() + flags.answer() }
"#,
    );
}

#[test]
fn incorrect_loop_item_usage_is_rejected_before_artifact_creation() {
    for body in ["let flag: bool = item", "sin(true)"] {
        let source = format!(
            "{COUNTING_ITERATOR}\nfn main() {{ let iter = Counter {{ index: 0, end: 1 }}; for item in iter {{ {body} }}; 42 }}"
        );
        let result = driver::Driver::new().compile(&source);
        assert!(result.has_errors, "accepted {source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|error| error.message.contains("type mismatch")),
            "{source}: {:?}",
            result.diagnostics
        );
        assert!(result.codegen_output.functions.is_empty(), "{source}");
        assert!(result.into_artifact().is_err(), "{source}");
    }
}

#[test]
fn default_method_self_alias_iteration_keeps_each_implementation() {
    assert_answer(
        r#"
trait Total {
    derive fn total(self) -> i64 {
        typealias Local = Self
        let iter: Local = self
        let total: i64 = 0
        for item in iter { total = total + item }
        total
    }
}

struct First { done: bool }
struct Second { done: bool }
impl Iterator for First {
    assoc Item: Type = i64
    pub fn next(self) -> IterationStep(i64) {
        if self.done { IterationStep(i64).done }
        else { self.done = true; IterationStep(i64).yielded(40) }
    }
}
impl Iterator for Second {
    assoc Item: Type = i64
    pub fn next(self) -> IterationStep(i64) {
        if self.done { IterationStep(i64).done }
        else { self.done = true; IterationStep(i64).yielded(2) }
    }
}
impl Total for First {}
impl Total for Second {}
fn main() { First { done: false }.total() + Second { done: false }.total() }
"#,
    );
}

#[test]
fn default_method_iteration_preserves_the_scoped_target_implementation() {
    assert_answer(
        r#"
trait Total {
    derive fn total(self) -> i64 {
        let iter: Self = self
        let total: i64 = 0
        for item in iter { total = total + item }
        total
    }
}
struct Cursor { done: bool }
mod first {
    extend Iterator for Cursor {
        assoc Item: Type = i64
        pub fn next(self) -> IterationStep(i64) {
            if self.done { IterationStep(i64).done }
            else { self.done = true; IterationStep(i64).yielded(40) }
        }
    }
    extend Total for Cursor {}
    pub fn answer() -> i64 { Cursor { done: false }.total() }
}
mod second {
    extend Iterator for Cursor {
        assoc Item: Type = i64
        pub fn next(self) -> IterationStep(i64) {
            if self.done { IterationStep(i64).done }
            else { self.done = true; IterationStep(i64).yielded(2) }
        }
    }
    extend Total for Cursor {}
    pub fn answer() -> i64 { Cursor { done: false }.total() }
}
fn main() { first.answer() + second.answer() }
"#,
    );
}
