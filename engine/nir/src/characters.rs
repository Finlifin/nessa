//! Decode the character token forms validated by the lexer.

pub(crate) fn parse_literal(text: &str) -> Result<char, &'static str> {
    let content = text
        .strip_prefix('\'')
        .and_then(|text| text.strip_suffix('\''))
        .ok_or("character literal must be quoted")?;
    if let Some(escaped) = content.strip_prefix('\\') {
        return match escaped {
            "n" => Ok('\n'),
            "t" => Ok('\t'),
            "r" => Ok('\r'),
            "0" => Ok('\0'),
            "\\" => Ok('\\'),
            "\"" => Ok('"'),
            "'" => Ok('\''),
            _ => decode_numeric_escape(escaped),
        };
    }
    let mut scalars = content.chars();
    let value = scalars.next().ok_or("character literal cannot be empty")?;
    if scalars.next().is_some() || value == '\n' || value == '\'' {
        return Err("character literal must contain one scalar value");
    }
    Ok(value)
}

/// `\xHH` is one byte, which is always a scalar. `\uXXXX` is four hex digits
/// and rejects surrogates. The brace form `\u{...}` is not a character escape.
fn decode_numeric_escape(escaped: &str) -> Result<char, &'static str> {
    let (digits, width) = if let Some(digits) = escaped.strip_prefix('x') {
        (digits, 2)
    } else if let Some(digits) = escaped.strip_prefix('u') {
        (digits, 4)
    } else {
        return Err("unsupported character escape");
    };
    if digits.len() != width || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("unsupported character escape");
    }
    let value = u32::from_str_radix(digits, 16).map_err(|_| "unsupported character escape")?;
    char::from_u32(value).ok_or("unicode escape is not a Unicode scalar value")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_literals_keep_unicode_and_every_supported_escape() {
        for (source, value) in [
            ("'a'", 'a'),
            ("'界'", '界'),
            ("'🦀'", '🦀'),
            ("'\\n'", '\n'),
            ("'\\t'", '\t'),
            ("'\\r'", '\r'),
            ("'\\\\'", '\\'),
            ("'\\\"'", '"'),
            ("'\\''", '\''),
            ("'\\0'", '\0'),
            ("'\\x41'", 'A'),
            ("'\\u00e9'", 'é'),
            ("'\\u00E9'", 'é'),
        ] {
            assert_eq!(parse_literal(source), Ok(value));
            let (mut resolved, _) = crate::literal::tests::resolved_integer("42", false, "i64");
            let node = resolved
                .ast
                .builder(ast::NodeKind::Char, rustc_span::DUMMY_SP)
                .set_str_id(str_interner::intern(source))
                .build();
            let mut builder =
                crate::builder::FunctionBuilder::new(nsbc::FuncId(0), str_interner::intern("char"));
            let mut block = builder.new_block();
            assert_eq!(
                crate::expr::lower_expr(&resolved, node, &mut builder, &mut block),
                crate::NirValue::ConstChar(value)
            );
        }
    }

    #[test]
    fn malformed_characters_are_rejected_without_fabricating_values() {
        for source in [
            "",
            "a",
            "''",
            "'ab'",
            "'é'",
            "'\\x4'",
            "'\\u12'",
            "'\\u{41}'",
            "'\\uD800'",
            "'\\q'",
            "'\n'",
        ] {
            assert!(parse_literal(source).is_err(), "{source:?}");
        }
    }
}
