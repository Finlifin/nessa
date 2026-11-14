//! `sign` reports the exact numeric order against zero.

mod common;

#[test]
fn sign_is_minus_one_zero_or_one() {
    let source = r#"
        fn main() {
            let nan = sqrt(-1.0)
            let matched = sign(-5) == -1 and sign(0) == 0 and sign(5) == 1
                and sign(-1.5) == -1 and sign(1.5) == 1 and sign(0.0) == 0 and sign(-0.0) == 0
                and sign(-2.0) == -1 and type_of(sign(1.5)) == i64
                and sign(-9223372036854775808) == -1
                and sign(nan) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn non_numbers_have_no_sign() {
    for source in [
        r#"fn main() { sign("a"); 0 }"#,
        r#"fn main() { sign(true); 0 }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}
