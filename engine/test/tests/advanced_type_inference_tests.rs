//! Integration tests for Wave E3: substantially strengthened type inference
//! (deferred `null` local bindings, flow-sensitive control-flow narrowing,
//! homogeneous collection element/callback/for-loop provenance, local lambda
//! call-site parameter inference, `#Effect` bidirectional peeling, target-typed
//! `.variant` enum shorthand, and `usize` range bounds).

mod common;

use driver::Driver;

fn assert_runs_to(source: &str, expected: i64) {
    let val = common::run_value(source).unwrap_or_else(|e| panic!("failed `{source}`: {e}"));
    assert_eq!(val, expected, "source:\n{source}");
}

fn assert_compile_error(source: &str, expected_substr: &str) {
    let compiled = Driver::new().compile(source);
    assert!(
        compiled.has_errors,
        "expected compile error containing `{expected_substr}` for:\n{source}"
    );
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.message.contains(expected_substr)
                || d.helps.iter().any(|h| h.contains(expected_substr))),
        "expected `{expected_substr}` in diagnostics {:?} for:\n{source}",
        compiled.diagnostics
    );
}

#[test]
fn deferred_null_local_bindings_infer_optional_type_and_reject_incompatible_writes() {
    assert_runs_to(
        r#"
        fn main() {
            var found = null
            for i in 1..10 {
                if i == 7 {
                    found = i * 6
                }
            }
            if found != null {
                found
            } else {
                0
            }
        }
        "#,
        42,
    );

    assert_runs_to(
        r#"
        fn pick(flag: bool) -> i64 {
            var text = null
            if flag {
                text = "nessa!"
            }
            if text == null {
                return 0
            }
            text.len() * 7
        }
        fn main() { pick(true) }
        "#,
        42,
    );

    // Incompatible assignment after deferred `?i64` inference must be rejected statically.
    assert_compile_error(
        r#"
        fn main() {
            var x = null
            x = 42
            x = "wrong"
            0
        }
        "#,
        "type mismatch",
    );
}

#[test]
fn flow_sensitive_narrowing_handles_guards_short_circuit_matches_when_and_reassignment_soundness() {
    // `!= null`, `== null` early return, `and` / `or` short-circuit, `matches`, `when`, and assignment narrowing.
    assert_runs_to(
        r#"
        fn length_or_zero(s: ?String) -> i64 {
            if s != null and s.len() > 2 {
                return s.len()
            }
            0
        }

        fn or_guard(s: ?String) -> i64 {
            if s == null or s.len() == 0 {
                0
            } else {
                s.len()
            }
        }

        fn when_narrow(x: ?i64) -> i64 {
            when {
                x == null => 0,
                else => x + 10,
            }
        }

        fn dyn_matches(v: Any) -> i64 {
            if v matches _: String {
                v.len()
            } else if v matches _: i64 {
                v * 2
            } else {
                0
            }
        }

        fn main() {
            var local_opt: ?i64 = null
            local_opt = 12
            let a = local_opt + 3
            let b = length_or_zero("hello")
            let c = or_guard("abcd")
            let d = when_narrow(8)
            let e = dyn_matches("xy")
            a + b + c + d + e
        }
        "#,
        44,
    );

    // Soundness 1: reassigning `x = null` in a branch must invalidate narrowing afterwards.
    assert_compile_error(
        r#"
        fn bad(flag: bool) -> i64 {
            var x: ?i64 = 10
            if x != null {
                if flag {
                    x = null
                }
            }
            x + 1
        }
        fn main() { bad(true) }
        "#,
        "arithmetic requires compatible numeric operands",
    );

    // Soundness 2: local mutated inside a closure must NOT be lexically narrowed across calls.
    assert_compile_error(
        r#"
        fn main() {
            var x: ?i64 = 41
            let wipe = || { x = null }
            if x != null {
                wipe()
                x + 1
            } else {
                0
            }
        }
        "#,
        "arithmetic requires compatible numeric operands",
    );
}

#[test]
fn homogeneous_collections_infer_callback_parameters_inductive_fold_and_for_binders() {
    // Chained List literal -> map -> filter -> fold without `.as(i64)`.
    assert_runs_to(
        r#"
        fn main() {
            let sum = [1, 2, 3, 4, 5]
                .map(|x| x * 3)
                .filter(|x| x > 5)
                .fold(0, |acc, x| acc + x)
            sum
        }
        "#,
        42,
    );

    // Empty `List()` + homogeneous `.push(...)` + `for x in xs` + `String.split` + `Set` + `Deque` + `Map.map_values`.
    assert_runs_to(
        r#"
        fn main() {
            let words = List()
            words.push("aa")
            words.push("bbbb")
            var total = 0
            for w in words {
                total += w.len()
            }

            let csv_len = "10,20,300".split(",").map(|part| part.len()).fold(0, |a, b| a + b)

            let s = Set.new()
            s.insert("hey")
            for item in s {
                total += item.len()
            }

            let dq = Deque.new()
            dq.push_back(10)
            dq.push_front(16)
            for n in dq {
                total += n
            }

            let mp = Map()
            mp.set("k", 0)
            let mp2 = mp.map_values(|k, v| k.len() + v)

            total + csv_len + mp2.get("k").as(i64) - 1
        }
        "#,
        42,
    );
}

#[test]
fn local_unannotated_lambda_infers_parameter_types_from_block_call_sites() {
    assert_runs_to(
        r#"
        fn main() {
            let combine = |s, scale| s.len() * scale
            combine("nessa", 6) + combine("ok", 6)
        }
        "#,
        42,
    );
}

#[test]
fn effect_qualified_return_types_propagate_bidirectional_expected_payload_types() {
    assert_runs_to(
        r#"
        effect Audit(code: i64) -> Unit

        fn compute() -> #Audit i32 {
            Audit(1)#
            42
        }

        fn main() {
            let v: i32 = compute()# { Audit(c) => () }
            v.as(i64)
        }
        "#,
        42,
    );
}

#[test]
fn target_typed_enum_variant_shorthand_works_in_expressions_errors_and_patterns() {
    assert_runs_to(
        r#"
        enum Op {
            noop,
            add(delta: i64),
            scale(factor: i64),
        }

        enum MathErr {
            negInput,
            tooLarge(limit: i64),
        }

        fn apply_op(base: i64, op: Op) -> i64 {
            op match {
                .noop => base,
                .add(d) => base + d,
                .scale(f) => base * f,
            }
        }

        fn checked(n: i64) -> !MathErr i64 {
            if n < 0 {
                return error .negInput
            }
            if n > 100 {
                return error .tooLarge(100)
            }
            n
        }

        fn main() {
            let first: Op = .add(11)
            let a = apply_op(10, first)
            let b = apply_op(a, .scale(2))
            let err_recover = checked(-5) ! {
                .negInput => 0,
                .tooLarge(lim) => lim,
            }
            if first matches .add(_) {
                b + err_recover
            } else {
                0
            }
        }
        "#,
        42,
    );

    // Standalone `.foo` without an enum target type remains rejected.
    assert_compile_error(
        "fn main() {\n    let x = .orphan\n    0\n}\n",
        "standalone symbol literals `.name` are not supported",
    );

    // Typo on `.variant` with a known enum target suggests the closest variant.
    assert_compile_error(
        "enum Color { red, green, blue }\nfn main() {\n    let c: Color = .reed\n    0\n}\n",
        "did you mean `.red`?",
    );
}

#[test]
fn range_literals_accept_usize_bounds_and_convert_safely() {
    assert_runs_to(
        r#"
        use std.bytes.Bytes
        fn main() {
            let buf = Bytes.from_string("abcdef")
            var acc = 0
            for i in 0..buf.len() {
                acc += (i + 1)
            }
            acc * 2
        }
        "#,
        42,
    );
}
