//! End-to-end integration tests for Wave 1 language features across lexer, parser, resolution, NIR, codegen, and VM.

mod common;

fn assert_rejected(source: &str) {
    let result = driver::Driver::new().compile(source);
    assert!(
        result.has_errors,
        "expected compilation error for:\n{source}\ngot diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn arbitrary_backtick_identifiers_and_comment_diagnostics() {
    let src = r#"
        struct `my-box` {
            `match`: i64
            `in`: i64
        }

        fn `plus-weighted`(lhs: i64, rhs: i64, .`scale-by`: i64 = 1) -> i64 {
            lhs + rhs * `scale-by`
        }

        fn main() -> i64 {
            let `use` = `my-box` { `match`: 10, `in`: 16 }
            let a = `plus-weighted`(`use`.`match`, `use`.`in`, `scale-by` = 2)
            let b = 5 `plus-weighted` 3
            a - b + 8 - 8
        }
    "#;
    // a = 10 + 16 * 2 = 42, b = 5 + 3 * 1 = 8 => 42 - 8 = 34
    assert_eq!(common::run_value(src).unwrap(), 34);

    // Empty backticks and unterminated block comments must be rejected.
    assert_rejected("fn main() -> i64 { let `` = 1; 0 }");
    assert_rejected("fn main() -> i64 { {- unterminated comment\n 0 }");
}

#[test]
fn when_expression_pipeline_and_infix_calls() {
    let src = r#"
        global ticks: i64 = 0

        fn bump_and_get(v: i64) -> i64 {
            ticks += 1
            v + 5
        }

        fn mul_add(x: i64, factor: i64, extra: i64) -> i64 {
            x * factor + extra
        }

        fn double(x: i64) -> i64 = x * 2

        fn classify(n: i64) -> i64 {
            when {
                n < 0 => -1
                n == 0 => 0
                n < 10 => 10
                else => 20
            }
        }

        fn main() -> i64 {
            let piped = bump_and_get(3) |> mul_add(2, 1) |> double
            let sum = classify(-4) + classify(0) + classify(7) + classify(99)
            if ticks == 1 { piped + sum } else { -999 }
        }
    "#;
    // bump_and_get(3) = 8 (ticks=1) -> mul_add(8, 2, 1) = 17 -> double(17) = 34
    // sum = -1 + 0 + 10 + 20 = 29 => 34 + 29 = 63
    assert_eq!(common::run_value(src).unwrap(), 63);
}

#[test]
fn case_map_alternatives_and_runtime_exhaustion() {
    let src = r#"
        fn apply_fn(f: fn(i64) -> i64, x: i64) -> i64 = f(x)

        fn main() -> i64 {
            let bias = 100
            let f: fn(i64) -> i64 = case 0 => bias | case n => n * 3 + 1
            let g = case n: i64 if n == 1 => 10 | case m: i64 => m + 2
            apply_fn(f, 0) + apply_fn(f, 5) + g(1) + g(7)
        }
    "#;
    // 100 + 16 + 10 + 9 = 135
    assert_eq!(common::run_value(src).unwrap(), 135);

    let non_matching = r#"
        fn main() -> i64 {
            let f: fn(i64) -> i64 = case 1 => 10 | case 2 => 20
            f(3)
        }
    "#;
    let err = common::run_value(non_matching).unwrap_err();
    assert!(err.contains("NoMatchingCase"), "{err}");
}

#[test]
fn defer_lifo_across_block_return_loop_and_try() {
    let src = r#"
        global trace: i64 = 0

        fn step(d: i64) {
            trace = trace * 10 + d
        }

        fn with_return(flag: bool) -> i64 {
            defer step(1)
            defer step(2)
            if flag {
                defer step(3)
                return 10
            }
            20
        }

        fn with_opt(v: ?i64) -> ?i64 {
            defer step(4)
            let x = v?
            defer step(5)
            x + 1
        }

        fn with_loop() {
            var i = 0
            while i < 3 {
                i += 1
                defer step(i)
                if i == 2 {
                    break
                }
            }
        }

        fn main() -> i64 {
            let r1 = with_return(true)
            let _ = with_opt(null)
            let r2 = with_opt(7) match { n? => n, _ => 0 }
            with_loop()
            trace + r1 + r2
        }
    "#;
    // with_return(true): 3, 2, 1 => trace = 321 (r1 = 10)
    // with_opt(null): 4 => trace = 3214
    // with_opt(7): 5, 4 => trace = 321454 (r2 = 8)
    // with_loop(): i=1 -> step(1), i=2 -> break runs step(2) => trace = 32145412
    assert_eq!(common::run_value(src).unwrap(), 32145412 + 10 + 8);

    // Control-flow exit out of `defer` must be rejected.
    assert_rejected("fn f() -> i64 { defer { return 1 }; 0 }\nfn main() -> i64 { f() }");
    assert_rejected("fn main() -> i64 { while true { defer { break }; break }; 0 }");
}

#[test]
fn let_const_var_else_and_module_tuple_destructuring() {
    let src = r#"
        let (A, (B, C)) = (10, (20, 12))

        fn extract(opt: ?(i64, i64)) -> i64 {
            let (x, y)? = opt else {
                return -1
            }
            var z? = (if x > 0 { y } else { null }) else {
                return -2
            }
            z += A + B + C
            z
        }

        fn main() -> i64 {
            extract(null) + extract((-3, 5)) + extract((4, 7))
        }
    "#;
    // -1 + -2 + (7 + 42) = 46
    assert_eq!(common::run_value(src).unwrap(), 46);

    // Non-diverging `else` branch must be rejected.
    assert_rejected("fn main() -> i64 { var x? = null else { 1 }; 0 }");
}

#[test]
fn struct_pattern_destructuring_error_unwrap_bound_methods_and_spread() {
    let src = r#"
        struct Pair {
            left: i64
            right: (i64, i64)
            extra: i64
        }

        impl Pair {
            pub fn weighted(self, k: i64) -> i64 {
                self.left + (self.right.0 + self.right.1) * k + self.extra
            }
        }

        enum Bad { Boom }

        fn ok_or_boom(ok: bool) -> !Bad i64 {
            if ok { 9 } else { error Bad.Boom }
        }

        fn sum3(a: i64, b: i64, c: i64) -> i64 = a + b + c

        fn main() -> i64 {
            let p = Pair { left: 2, right: (3, 4), extra: 1 }
            let Pair { left, right: (r0, r1), ... } = p
            let bound = p.weighted
            let tuple_args = (left, r0, r1)
            let s = sum3(...tuple_args)
            let dyn_fn: fn(i64, i64, i64) -> i64 = sum3
            let s2 = dyn_fn(...[1, 2, 3])
            bound(2) + s + s2 + ok_or_boom(true).unwrap()
        }
    "#;
    // bound(2) = 2 + 7*2 + 1 = 17
    // s = 2 + 3 + 4 = 9
    // s2 = 6
    // unwrap = 9
    // total = 41
    assert_eq!(common::run_value(src).unwrap(), 41);

    let err_unwrap = r#"
        enum Bad { Boom }
        fn fail() -> !Bad i64 { error Bad.Boom }
        fn main() -> i64 { fail().unwrap() }
    "#;
    assert!(common::run_value(err_unwrap).is_err());

    // Struct pattern missing field without `...` and unknown field must be rejected.
    assert_rejected(
        "struct P { x: i64, y: i64 }\nfn main() -> i64 { let P { x } = P { x: 1, y: 2 }; x }",
    );
    assert_rejected("struct P { x: i64 }\nfn main() -> i64 { let P { x, z } = P { x: 1 }; x }");
}

#[test]
fn non_exhaustive_match_emits_warning_without_blocking_compilation() {
    let src = r#"
        enum Color { red, green, blue }
        fn check(c: Color) -> i64 {
            c match {
                Color.red => 1
                Color.green => 2
            }
        }
        fn main() -> i64 { check(Color.red) }
    "#;
    let compiled = driver::Driver::new().compile(src);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.level == diagnostic::Level::Warning && d.message.contains("non-exhaustive")),
        "expected non-exhaustive match warning, got: {:?}",
        compiled.diagnostics
    );
}
