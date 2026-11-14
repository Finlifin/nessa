//! Integer bit methods, f64 methods, and bool.to_i64.

mod common;

#[test]
fn bit_counts_follow_the_runtime_width() {
    let source = r#"
        fn main() {
            let matched = (0b1100).count_ones() == 2 and (-1).count_ones() == 64 and 0.count_zeros() == 64
                and 1.leading_zeros() == 63 and 0.trailing_zeros() == 64 and 1.trailing_zeros() == 0
                and (-1).leading_ones() == 64 and 1.trailing_ones() == 1
                and (0b1100).reverse_bits() == 3458764513820540928
                and 256.swap_bytes() == 281474976710656
                and 1.rotate_left(3) == 8 and 1.rotate_left(64) == 1 and 2.rotate_right(1) == 1
                and 8.is_power_of_two() and 1.is_power_of_two() and not (0.is_power_of_two())
                and not (3.is_power_of_two()) and not ((-2).is_power_of_two())
                and 8.ilog2() == 3 and 1.ilog2() == 0 and 0.ilog2() == null and (-1).ilog2() == null
                and (-4).is_negative() and 4.is_positive() and 0.is_zero()
                and (1).as(i128).leading_zeros() == 127 and (-1).as(i128).count_ones() == 128
                and (1).as(i128).rotate_left(128) == 1 and (1).as(u128).trailing_zeros() == 0
                and (1).as(usize).count_ones() == 1 and (1).as(isize).leading_zeros() == 63
                and (-1.5).abs() == 1.5 and (1.25).fract() == 0.25 and (9.0).sqrt() == 3
                and log2(-1).is_nan() and log2(0).is_infinite() and (0.0).is_finite()
                and (-0.0).is_sign_negative() and (1.5).sign() == 1
                and true.to_i64() == 1 and false.to_i64() == 0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn negative_rotate_distance_is_rejected() {
    let error = common::run_value(r#"fn main() { 1.rotate_left(-1) }"#).unwrap_err();
    assert!(
        error.contains("rotate distance must not be negative"),
        "{error}"
    );
}

#[test]
fn the_new_methods_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.count_ones() }"#,
        r#"fn main() { "a".is_nan() }"#,
        r#"fn main() { 1.rotate_left("a") }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
