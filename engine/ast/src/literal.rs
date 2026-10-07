//! Parse integer token text consistently across type checking and lowering.

/// Decode an unsigned literal magnitude, including binary, octal and hexadecimal.
/// The sign belongs to the AST unary operator rather than the token.
pub fn integer_magnitude(text: &str) -> Result<u128, std::num::ParseIntError> {
    let (digits, radix) = match text.get(..2) {
        Some("0b" | "0B") => (&text[2..], 2),
        Some("0o" | "0O") => (&text[2..], 8),
        Some("0x" | "0X") => (&text[2..], 16),
        _ => (text, 10),
    };
    u128::from_str_radix(digits, radix)
}

#[cfg(test)]
mod tests {
    use super::integer_magnitude;

    #[test]
    fn integer_tokens_preserve_radix_and_reject_overflow() {
        for text in ["42", "0b101010", "0o52", "0x2A", "0X2a"] {
            assert_eq!(integer_magnitude(text), Ok(42));
        }
        assert_eq!(integer_magnitude(&u128::MAX.to_string()), Ok(u128::MAX));
        for text in [
            "",
            "0x",
            "0b2",
            "-1",
            "340282366920938463463374607431768211456",
        ] {
            assert!(integer_magnitude(text).is_err(), "{text}");
        }
    }
}
