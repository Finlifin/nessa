//! Integer constants retain their validated magnitude and signedness.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;
use type_pool::Intrinsic;

use crate::NirValue;

/// Lower a literal or its immediate unary minus without first materializing
/// a positive signed value; that value cannot represent a signed minimum.
pub(crate) fn lower_integer(resolved: &ResolvedAst, node: NodeIndex) -> NirValue {
    lower_integer_as(resolved, node, None)
}

pub(crate) fn lower_integer_as(
    resolved: &ResolvedAst,
    node: NodeIndex,
    ty: Option<type_pool::TypeIndex>,
) -> NirValue {
    let ast = &resolved.ast;
    let negative = ast.node(node).kind == NodeKind::Negative;
    let literal = if negative {
        ast.fixed_children(node)[0]
    } else {
        node
    };
    let text = str_interner::get(ast.node(literal).str_id);
    // Lowering follows successful type resolution, which validates the token's
    // magnitude and signed range. Never silently replace invalid data with zero.
    let magnitude = ast::literal::integer_magnitude(&text)
        .expect("integer tokens are validated before NIR lowering");
    let kind = ty
        .or_else(|| {
            resolved
                .node_types
                .get(&node)
                .or_else(|| resolved.node_types.get(&literal))
                .copied()
        })
        .and_then(|ty| resolved.type_pool.as_intrinsic(ty));
    let unsigned = kind.is_some_and(|kind| {
        matches!(
            kind,
            Intrinsic::U8
                | Intrinsic::U16
                | Intrinsic::U32
                | Intrinsic::U64
                | Intrinsic::U128
                | Intrinsic::Usize
        )
    });
    if unsigned || (!negative && magnitude > i128::MAX as u128) {
        debug_assert!(!negative, "negative literals cannot have unsigned types");
        if kind == Some(Intrinsic::U128) {
            return NirValue::ConstU128(magnitude);
        }
        return match u64::try_from(magnitude) {
            Ok(value) => NirValue::ConstUInt(value),
            Err(_) => NirValue::ConstU128(magnitude),
        };
    }
    let value = if negative && magnitude == 1_u128 << 127 {
        i128::MIN
    } else {
        let positive =
            i128::try_from(magnitude).expect("validated signed literal magnitude fits i128");
        if negative { -positive } else { positive }
    };
    if kind == Some(Intrinsic::I128) {
        return NirValue::ConstI128(value);
    }
    match i64::try_from(value) {
        Ok(value) => NirValue::ConstInt(value),
        Err(_) => NirValue::ConstI128(value),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use ast::{Ast, NodeIndex, NodeKind};
    use diagnostic::DiagnosticContext;
    use resolution::ResolvedAst;
    use rustc_span::{
        DUMMY_SP,
        source_map::{FilePathMapping, SourceMap},
    };

    use super::lower_integer;
    use crate::{NirValue, Terminator};

    pub(crate) fn resolved_integer(
        text: &str,
        negative: bool,
        ty: &str,
    ) -> (ResolvedAst, NodeIndex) {
        let mut ast = Ast::new();
        let sign = if negative { "-" } else { "" };
        ast.source = Some(format!("fn number()->{ty}={sign}{text}"));
        let token = ast
            .builder(NodeKind::Int, DUMMY_SP)
            .set_str_id(str_interner::intern(text))
            .build();
        let value = if negative {
            ast.builder(NodeKind::Negative, DUMMY_SP)
                .add_child(token)
                .build()
        } else {
            token
        };
        let annotation = ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern(ty))
            .build();
        let name = ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("number"))
            .build();
        let function = ast
            .builder(NodeKind::FunctionDef, DUMMY_SP)
            .add_child(name)
            .add_child(annotation)
            .add_child(value)
            .add_child(NodeIndex::NULL)
            .build();
        ast.root = ast
            .builder(NodeKind::FileScope, DUMMY_SP)
            .add_multi_children(&[function])
            .build();
        let source_map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = DiagnosticContext::new(&source_map);
        let resolved = resolution::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        (resolved, value)
    }

    #[test]
    fn integer_lowering_keeps_radix_signedness_and_full_width() {
        for (text, negative, ty, expected) in [
            ("42", false, "i64", NirValue::ConstInt(42)),
            ("42", false, "i128", NirValue::ConstI128(42)),
            ("42", false, "u128", NirValue::ConstU128(42)),
            ("0x2A", false, "i64", NirValue::ConstInt(42)),
            ("0B101010", false, "u64", NirValue::ConstUInt(42)),
            ("0o52", true, "i64", NirValue::ConstInt(-42)),
            (
                "9223372036854775808",
                true,
                "i64",
                NirValue::ConstInt(i64::MIN),
            ),
            (
                "18446744073709551615",
                false,
                "u64",
                NirValue::ConstUInt(u64::MAX),
            ),
            (
                "18446744073709551616",
                false,
                "i128",
                NirValue::ConstI128(1_i128 << 64),
            ),
            (
                "170141183460469231731687303715884105728",
                true,
                "i128",
                NirValue::ConstI128(i128::MIN),
            ),
            (
                "0xffffffffffffffffffffffffffffffff",
                false,
                "u128",
                NirValue::ConstU128(u128::MAX),
            ),
        ] {
            let (resolved, literal) = resolved_integer(text, negative, ty);
            assert_eq!(lower_integer(&resolved, literal), expected, "{ty}: {text}");
            let module = crate::lower(&resolved);
            assert!(
                module.functions[0].blocks.iter().any(|block| {
                    matches!(block.terminator, Terminator::Return(value) if value == expected)
                }),
                "function lowering must preserve {expected:?}"
            );
        }
    }
}
