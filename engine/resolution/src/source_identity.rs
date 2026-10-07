//! Checked temporary package provenance from schema-coded source tokens.

use lexer::TokenKind;
use pkg_manager::ManifestDocument;
use sha2::{Digest, Sha256};
use type_pool::{IdentityPathSegment, PackageTypeContext};

const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCE_TOKENS: usize = 1_000_000;

/// Build a schema-1 temporary manifest identity from a checked token stream.
/// Layout and literal contents remain significant; comments and token spacing do not.
pub fn scratch_package_identity(source: &str) -> Result<PackageTypeContext, String> {
    scratch_for_path(source, &[])
}

pub(crate) fn scratch_for_path(
    source: &str,
    path: &[IdentityPathSegment],
) -> Result<PackageTypeContext, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("scratch identity source exceeds 16 MiB".into());
    }
    let (tokens, errors) = lexer::tokenize(source);
    if !errors.is_empty() {
        return Err("scratch identity requires a valid source token stream".into());
    }
    if tokens.len() > MAX_SOURCE_TOKENS {
        return Err("scratch identity token count exceeds 1000000".into());
    }
    let mut digest = Sha256::new();
    digest.update(b"nessa.scratch.source\0");
    digest.update(1_u32.to_be_bytes());
    for token in tokens {
        if matches!(
            token.kind,
            TokenKind::Comment | TokenKind::Sof | TokenKind::Eof
        ) {
            continue;
        }
        if token.kind == TokenKind::Invalid {
            return Err("scratch identity contains an invalid source token".into());
        }
        let label = match token.kind {
            TokenKind::FStringExprStart => "<fstring_expression_start>",
            TokenKind::FStringExprEnd => "<fstring_expression_end>",
            other => other.lexeme(),
        };
        framed(&mut digest, label.as_bytes())?;
        if matches!(
            token.kind,
            TokenKind::Id
                | TokenKind::ArbitraryId
                | TokenKind::MacroContent
                | TokenKind::String
                | TokenKind::Integer
                | TokenKind::IntBin
                | TokenKind::IntOct
                | TokenKind::IntHex
                | TokenKind::Real
                | TokenKind::RealSci
                | TokenKind::Char
                | TokenKind::FStringLiteral
        ) {
            let text = source
                .get(token.from as usize..token.to as usize)
                .ok_or("scratch identity token has invalid source boundaries")?;
            framed(&mut digest, text.as_bytes())?;
        }
    }
    let source_hash = hexadecimal(&digest.finalize()[..16]);
    let mut manifest = format!(
        "[package]\nname = \"source\"\ndomain = \"org.nessa.scratch\"\nversion = \"0.0.0\"\ntype = \"tmp\"\nnormalization_schema = 1\nsource_hash = \"{source_hash}\"\n"
    );
    if !path.is_empty() {
        let mut root = Sha256::new();
        root.update(b"nessa.scratch.root\0");
        root.update(1_u32.to_be_bytes());
        for segment in path {
            match segment {
                IdentityPathSegment::Named(name) => {
                    root.update([0]);
                    framed(&mut root, name.as_bytes())?;
                }
                IdentityPathSegment::Lexical { kind, ordinal } => {
                    root.update([1, *kind]);
                    root.update(ordinal.to_be_bytes());
                }
            }
        }
        manifest.push_str(&format!(
            "root_hash = \"{}\"\n",
            hexadecimal(&root.finalize()[..16])
        ));
    }
    let document = ManifestDocument::parse(&manifest).map_err(|error| error.to_string())?;
    let metadata = document.manifest();
    Ok(PackageTypeContext {
        identity_schema: 1,
        identity: *document.local_identity().as_bytes(),
        qualified_name: metadata.qualified_name(),
        version: metadata.version.to_string(),
    })
}

fn framed(digest: &mut Sha256, bytes: &[u8]) -> Result<(), String> {
    let length = u32::try_from(bytes.len()).map_err(|_| "scratch identity token is too large")?;
    digest.update(length.to_be_bytes());
    digest.update(bytes);
    Ok(())
}

fn hexadecimal(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(char::from(HEX[(byte >> 4) as usize]));
        result.push(char::from(HEX[(byte & 15) as usize]));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_identity_ignores_spacing_and_comments_but_preserves_literals() {
        let plain = scratch_package_identity("let value = 1\n").unwrap();
        let formatted = scratch_package_identity("let  value=1 -- explanation\n").unwrap();
        assert_eq!(plain, formatted);
        assert_ne!(
            plain.identity,
            scratch_package_identity("let value = 2\n")
                .unwrap()
                .identity
        );
        assert_ne!(
            plain.identity,
            scratch_package_identity("let other = 1\n")
                .unwrap()
                .identity
        );
        assert_eq!(plain.identity_schema, 1);
        assert_eq!(plain.qualified_name, "org.nessa.scratch/source");
        assert_eq!(plain.version, "0.0.0");
    }

    #[test]
    fn scratch_identity_preserves_layout_and_interpolation_contents() {
        assert_ne!(
            scratch_package_identity("if true:\n    1\n")
                .unwrap()
                .identity,
            scratch_package_identity("if true:\n1\n").unwrap().identity,
        );
        assert_ne!(
            scratch_package_identity("let value = \"prefix {1}\"\n")
                .unwrap()
                .identity,
            scratch_package_identity("let value = \"prefix {2}\"\n")
                .unwrap()
                .identity,
        );
        assert_ne!(
            scratch_package_identity("let value = \"one\"\n")
                .unwrap()
                .identity,
            scratch_package_identity("let value = \"two\"\n")
                .unwrap()
                .identity,
        );
    }

    #[test]
    fn scratch_roots_use_framed_typed_paths() {
        let named = |name: &str| IdentityPathSegment::Named(name.into());
        let source = "let value = 1\n";
        assert_ne!(
            scratch_for_path(source, &[named("a.b")]).unwrap().identity,
            scratch_for_path(source, &[named("a"), named("b")])
                .unwrap()
                .identity,
        );
        assert_ne!(
            scratch_for_path(source, &[named("0")]).unwrap().identity,
            scratch_for_path(
                source,
                &[IdentityPathSegment::Lexical {
                    kind: 1,
                    ordinal: 0
                }]
            )
            .unwrap()
            .identity,
        );
    }

    #[test]
    fn scratch_identity_rejects_invalid_tokens_and_source_budget() {
        assert!(scratch_package_identity("\"unterminated").is_err());
        assert!(scratch_package_identity(&" ".repeat(MAX_SOURCE_BYTES + 1)).is_err());
    }
}
