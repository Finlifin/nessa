//! Angle conversion, ASCII whitespace, digit radix, and integer logs.

mod common;

#[test]
fn angles_ascii_text_and_logs_match_their_domains() {
    let source = r#"
        fn main() {
            let matched = (0.0).to_degrees() == 0 and (180.0).to_radians().to_degrees() == 180
                and (0.0).next_up() > 0 and (0.0).next_down().is_sign_negative()
                and (1.0).midpoint(3.0) == 2
                and (0.5).round_ties_even() == 0 and (1.5).round_ties_even() == 2
                and (-1.5).round_ties_even() == -2
                and 'a'.is_digit(16) and not ('g'.is_digit(16)) and 'a'.to_digit(16) == 10
                and '9'.to_digit(10) == 9 and 'é'.to_digit(10) == null
                and "  a\tb ".split_ascii_whitespace() == ["a", "b"]
                and " \t".split_ascii_whitespace().is_empty()
                and "a\u00a0b".split_ascii_whitespace() == ["a\u00a0b"]
                and "  A".trim_ascii_start() == "A" and "A  ".trim_ascii_end() == "A"
                and " a ".trim_ascii() == "a" and "a\u00a0".trim_ascii() == "a\u00a0"
                and "Ab".eq_ignore_ascii_case("aB") and not ("é".eq_ignore_ascii_case("É"))
                and 1000.ilog10() == 3 and 1.ilog10() == 0 and 0.ilog10() == null
                and (-5).ilog10() == null and (1000).as(i128).ilog10() == 3
                and 5.abs_diff(3) == 2 and (-1).abs_diff(1) == 2
                and (1).as(i128).abs_diff((-1).as(i128)) == 2
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_digit_radix_outside_2_to_36_is_rejected() {
    for source in [
        r#"fn main() { 'a'.is_digit(1) }"#,
        r#"fn main() { 'a'.is_digit(37) }"#,
        r#"fn main() { 'a'.to_digit(-1) }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("digit radix must be from 2 to 36"),
            "{source}: {error}"
        );
    }
}

#[test]
fn the_new_methods_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.to_degrees() }"#,
        r#"fn main() { 1.trim_ascii() }"#,
        r#"fn main() { true.is_digit(10) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
