//! Compiler plans for legacy and compositional Display implementations.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayDerivationMode {
    LegacyStruct,
    FieldCalls,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedDisplayPlan {
    pub mode: DisplayDerivationMode,
    pub node: NodeIndex,
    pub implementor: TypeIndex,
    pub trait_type: TypeIndex,
    pub method_name: str_interner::StrId,
    pub signature: TypeIndex,
}

pub(crate) fn register_display(r: &mut Resolver<'_>, node: NodeIndex, implementor: TypeIndex) {
    let signature = r.type_pool.intern_structural(TypeKind::Function {
        params: vec![implementor],
        ret: Intrinsic::Str.type_index(),
    });
    r.display_derivations.push(DerivedDisplayPlan {
        mode: if matches!(r.type_pool.get(implementor).kind, TypeKind::Struct { .. }) {
            DisplayDerivationMode::LegacyStruct
        } else {
            DisplayDerivationMode::FieldCalls
        },
        node,
        implementor,
        trait_type: r.type_pool.well_known.display,
        method_name: str_interner::intern("to_string"),
        signature,
    });
}

pub(crate) fn declared_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    ty: TypeIndex,
    name: str_interner::StrId,
) -> Option<TypeIndex> {
    if name != str_interner::intern("to_string") {
        return None;
    }
    let ty = r.type_pool.canonical_type(ty)?;
    let denotes = |node: NodeIndex| {
        r.node_type_values
            .get(&node)
            .copied()
            .or_else(|| {
                r.node_symbols
                    .get(&node)
                    .map(|id| r.symbols[id.0 as usize].type_index)
            })
            .and_then(|ty| r.type_pool.canonical_type(ty))
    };
    let declared = (1..ast.nodes.len()).any(|index| {
        let node = NodeIndex(index as u32);
        ast.node(node).kind == NodeKind::DeriveDef
            && denotes(ast.fixed_children(node)[0]) == Some(ty)
            && ast
                .multi_children(node)
                .iter()
                .any(|&node| denotes(node) == Some(r.type_pool.well_known.display))
    });
    declared.then(|| {
        r.register_type(TypeKind::Function {
            params: vec![],
            ret: Intrinsic::Str.type_index(),
        })
    })
}

fn components(r: &Resolver<'_>, ty: TypeIndex) -> Vec<TypeIndex> {
    match &r.type_pool.get(ty).kind {
        TypeKind::Struct { fields, .. } => fields.iter().map(|field| field.ty).collect(),
        TypeKind::Enum { variants, .. } => variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty))
            .collect(),
        TypeKind::Tuple { elements } => elements.clone(),
        _ => vec![],
    }
}

fn displayable(r: &Resolver<'_>, ty: TypeIndex, seen: &mut HashSet<TypeIndex>, scope: u32) -> bool {
    let Some(ty) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    if r.type_pool.is_reserved_collection_role(ty) {
        return false;
    }
    if !seen.insert(ty) {
        return true;
    }
    let method = r.type_pool.find_trait_method(
        ty,
        r.type_pool.well_known.display,
        str_interner::intern("to_string"),
    );
    if let Some(method) = method {
        if !matches!(r.type_pool.method_accessible(method, scope), Ok(true)) {
            return false;
        }
        if method.func_id == type_pool::DERIVE_FUNC_ID {
            return r
                .display_derivations
                .iter()
                .any(|plan| plan.implementor == ty);
        }
        let Some(symbol) = r.symbols.get(method.func_id as usize) else {
            return false;
        };
        let Some(signature) = r.type_pool.canonical_type(symbol.type_index) else {
            return false;
        };
        return matches!(&r.type_pool.get(signature).kind,TypeKind::Function{params,ret}
            if params.len()==1 && r.type_pool.canonical_type(params[0])==Some(ty)
                && r.type_pool.as_intrinsic(*ret)==Some(Intrinsic::Str));
    }
    match &r.type_pool.get(ty).kind {
        TypeKind::Optional { inner } => {
            r.type_pool.as_intrinsic(*inner) == Some(Intrinsic::NoReturn)
                || displayable(r, *inner, seen, scope)
        }
        TypeKind::Tuple { elements } => elements
            .iter()
            .all(|&element| displayable(r, element, seen, scope)),
        _ => false,
    }
}

pub(crate) fn validate(r: &Resolver<'_>, ast: &Ast) {
    for plan in &r.display_derivations {
        if plan.mode == DisplayDerivationMode::LegacyStruct {
            continue;
        }
        let Some(scope) = r.node_scopes.get(&plan.node).map(|scope| scope.0) else {
            r.diag_ctx
                .error("Display derivation is missing its lexical scope".into())
                .with_primary_span(ast.node(plan.node).span)
                .emit(r.diag_ctx);
            continue;
        };
        for component in components(r, plan.implementor) {
            if !displayable(r, component, &mut HashSet::new(), scope) {
                let name = r
                    .type_pool
                    .display_name(component)
                    .unwrap_or_else(|| "<invalid>".into());
                r.diag_ctx.error(format!("Display derivation requires a checked global Display implementation for field type `{name}`"))
                    .with_primary_span(ast.node(plan.node).span).emit(r.diag_ctx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("display.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        (resolved, errors)
    }

    #[test]
    fn source_structs_keep_legacy_formatting_without_new_field_obligations() {
        let (resolved, errors) = resolve("struct Legacy { value:Any }; derive Display for Legacy");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(resolved.display_derivations.len(), 1);
        assert_eq!(
            resolved.display_derivations[0].mode,
            DisplayDerivationMode::LegacyStruct
        );
    }

    #[test]
    fn enum_and_tuple_display_plans_check_every_component() {
        let (resolved, errors) = resolve(
            "enum Empty { only }; derive Display for Empty; typealias EmptyTuple=(Empty,); derive Display for EmptyTuple",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(resolved.display_derivations.len(), 2);
        assert!(
            resolved
                .display_derivations
                .iter()
                .all(|plan| plan.mode == DisplayDerivationMode::FieldCalls)
        );
        for source in [
            "enum Bad { value(x:Any) }; derive Display for Bad",
            "typealias Bad=(Any,); derive Display for Bad",
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("checked global Display implementation")),
                "{errors:?}"
            );
        }
    }
}
