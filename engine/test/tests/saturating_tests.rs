//! Saturating division, checked float truncation, and the remaining constants.

mod common;

#[test]
fn saturating_division_truncation_and_constants_match() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let matched = (-7).saturating_div(3) == -2
                and (-7).saturating_div_euclid(3) == -3
                and min.saturating_div(-1) == 9223372036854775807
                and min.saturating_div_euclid(-1) == 9223372036854775807
                and (5).as(u64).saturating_div((2).as(u64)) == 2
                and (1.9).trunc_i64() == 1
                and (-1.9).trunc_i64() == -1
                and (-0.4).trunc_u64() == 0
                and (-1.0).trunc_u64() == null
                and infinity().trunc_i64() == null
                and f64_max().trunc_i64() == null
                and (1.9).trunc_i128() == 1
                and (1.9).trunc_u128() == 1
                and '7'.is_ascii_octdigit()
                and (not ('8'.is_ascii_octdigit()))
                and (not ('a'.is_ascii_octdigit()))
                and frac_1_pi() * pi() == 1.0
                and frac_2_pi() * pi() == 2.0
                and frac_pi_8() * 8.0 == pi()
                and frac_pi_3() > 1.047 and frac_pi_3() < 1.048
                and frac_pi_6() > 0.523 and frac_pi_6() < 0.524
                and frac_2_sqrt_pi() > 1.128 and frac_2_sqrt_pi() < 1.129
                and f64_min() < 0.0 and f64_max() > 0.0
                and f64_min().is_finite() and f64_max() == -f64_min()
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_zero_divisor_is_rejected() {
    for source in [
        r#"fn main() { 1.saturating_div(0) }"#,
        r#"fn main() { 1.saturating_div_euclid(0) }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("divisor must not be zero"),
            "{source}: {error}"
        );
    }
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.saturating_div(1) }"#,
        r#"fn main() { true.is_ascii_octdigit() }"#,
        r#"fn main() { 1.trunc_i64() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
