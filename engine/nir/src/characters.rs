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
            "\\" => Ok('\\'),
            "\"" => Ok('"'),
            "'" => Ok('\''),
            _ => Err("unsupported character escape"),
        };
    }
    let mut scalars = content.chars();
    let value = scalars.next().ok_or("character literal cannot be empty")?;
    if scalars.next().is_some() || value == '\n' || value == '\'' {
        return Err("character literal must contain one scalar value");
    }
    Ok(value)
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
            "'\\x41'",
            "'\\u{41}'",
            "'\\q'",
            "'\n'",
        ] {
            assert!(parse_literal(source).is_err(), "{source:?}");
        }
    }
}
