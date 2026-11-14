//! Algebraic floats, native-endian bytes, and bool methods.

mod common;

#[test]
fn algebraic_nan_and_zero_match() {
    let source = r#"
        fn main() {
            let matched = 1.5.algebraic_add(2.25) == 3.75
                and 5.0.algebraic_sub(1.5) == 3.5
                and 2.0.algebraic_mul(3.0) == 6.0
                and 9.0.algebraic_div(2.0) == 4.5
                and 7.0.algebraic_rem(2.0) == 1.0
                and 1.0.nan_to(8.0) == 1.0
                and nan().nan_to(8.0) == 8.0
                and infinity().nan_to(0.0) == infinity()
                and 3.0.abs_diff(5.0) == 2.0
                and (-3.0).abs_diff(5.0) == 8.0
                and 0.0.is_zero()
                and copysign(0.0, -1.0).is_zero()
                and (not 1.0.is_zero())
                and 1.0.copysign(-2.0) == -1.0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn native_bytes_round_trip() {
    let source = r#"
        fn main() {
            let value = 12.5
            let native = value.to_ne_bytes()
            let back = f64_from_ne_bytes(native)
            let le = value.to_le_bytes()
            let be = value.to_be_bytes()
            let matched = back == value
                and native.len() == 8
                and (native == le or native == be)
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bool_methods_evaluate_both_sides() {
    let source = r#"
        fn main() {
            let matched = true.negate() == false
                and false.negate()
                and true.bitand(true)
                and (not true.bitand(false))
                and true.bitor(false)
                and (not false.bitor(false))
                and true.xor(false)
                and (not true.xor(true))
                and true.then(7) == 7
                and false.then(7) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn native_float_bytes_require_eight_bytes() {
    let error = common::run_value("fn main() { f64_from_ne_bytes([1]) }").unwrap_err();
    assert!(
        error.contains("float byte length does not match"),
        "{error}"
    );
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.algebraic_add(1.0) }"#,
        r#"fn main() { "a".is_zero() }"#,
        r#"fn main() { 1.negate() }"#,
        r#"fn main() { true.then() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
