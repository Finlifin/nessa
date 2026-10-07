//! Nominal enum metadata, checked constructors and typed structural patterns.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{FieldInfo, TypeIndex, TypeKind, VariantInfo};

use crate::resolver::Resolver;
use crate::{SymbolKind, name, typing};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumVariantRef {
    pub type_index: TypeIndex,
    pub variant_tag: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumConstructionPlan {
    pub type_index: TypeIndex,
    pub variant_tag: u32,
    pub argument_type: TypeIndex,
    /// Metadata-only constructors have no source parameter declaration.
    /// Source variants use the shared CallArgumentPlan instead.
    pub source_fields: Option<Vec<usize>>,
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

pub(crate) fn prepare(r: &mut Resolver<'_>, ast: &Ast) {
    let declarations: Vec<_> = r
        .symbols
        .iter()
        .filter_map(|symbol| {
            (!symbol.def_node.is_null() && ast.node(symbol.def_node).kind == NodeKind::EnumDef)
                .then_some((symbol.type_index, symbol.def_node))
        })
        .collect();
    for (ty, declaration) in declarations {
        let mut variants = Vec::new();
        for &wrapped in ast.multi_children(declaration) {
            let node = crate::structs::unwrap_member(ast, wrapped);
            if ast.node(node).kind != NodeKind::EnumVariant {
                continue;
            }
            let name_node = ast.fixed_children(node)[0];
            let Some(&symbol) = r.node_symbols.get(&name_node) else {
                continue;
            };
            r.symbols[symbol.0 as usize].type_index = ty;
            let tag = r.enum_variant_indices[&symbol];
            let mut fields = Vec::new();
            let mut names = HashSet::new();
            for &parameter in ast.multi_children(node) {
                let children = ast.fixed_children(parameter);
                if !matches!(
                    ast.node(parameter).kind,
                    NodeKind::ParamTyped | NodeKind::ParamOptional | NodeKind::ParamVarargs
                ) || ast.node(children[0]).kind != NodeKind::Id
                {
                    report(
                        r,
                        ast,
                        parameter,
                        "enum fields require named type annotations; destructured fields are not implemented",
                    );
                    continue;
                }
                let name = ast.node(children[0]).str_id;
                if !names.insert(name) {
                    report(r, ast, parameter, "duplicate enum field name");
                }
                let Some(field_type) = typing::resolve_type_expr(r, ast, children[1]) else {
                    report(
                        r,
                        ast,
                        parameter,
                        "enum fields require a valid type annotation",
                    );
                    continue;
                };
                if let Some(&symbol) = r.node_symbols.get(&children[0]) {
                    r.symbol_mut(symbol).type_index = field_type;
                }
                if ast.node(parameter).kind == NodeKind::ParamVarargs
                    && !crate::lists::is_list(r, field_type)
                {
                    report(
                        r,
                        ast,
                        parameter,
                        "a variadic parameter requires a List type annotation",
                    );
                }
                fields.push(FieldInfo {
                    name,
                    ty: field_type,
                    has_default: ast.node(parameter).kind == NodeKind::ParamOptional,
                    offset: (fields.len() * 8) as u32,
                });
            }
            crate::arguments::validate_variadic_parameters(r, ast, node, ast.multi_children(node));
            variants.push(VariantInfo {
                name: ast.node(name_node).str_id,
                tag,
                fields,
            });
        }
        if let TypeKind::Enum {
            variants: destination,
            ..
        } = &mut r.type_pool.get_mut(ty).kind
        {
            *destination = variants;
        }
    }
}

pub(crate) fn descriptor(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
) -> Option<(EnumVariantRef, VariantInfo)> {
    if matches!(
        ast.node(node).kind,
        NodeKind::Projection | NodeKind::PropertyPattern
    ) {
        let children = ast.fixed_children(node);
        if let Some(ty) = typing::resolve_type_expr_inner(r, ast, children[0])
            .and_then(|ty| r.type_pool.canonical_type(ty))
            && r.type_pool
                .checked_iteration_step_item(ty)
                .ok()
                .flatten()
                .is_some()
            && let TypeKind::Enum { variants, .. } = &r.type_pool.get(ty).kind
        {
            let variant = variants
                .iter()
                .find(|variant| variant.name == ast.node(children[1]).str_id)?
                .clone();
            return Some((
                EnumVariantRef {
                    type_index: ty,
                    variant_tag: variant.tag,
                },
                variant,
            ));
        }
    }
    let symbol = r.node_symbols.get(&node).copied().or_else(|| {
        matches!(
            ast.node(node).kind,
            NodeKind::Projection | NodeKind::PropertyPattern
        )
        .then(|| r.node_symbols.get(&ast.fixed_children(node)[1]).copied())
        .flatten()
    })?;
    let symbol_info = &r.symbols[symbol.0 as usize];
    if symbol_info.kind != SymbolKind::EnumVariant {
        return None;
    }
    let ty = r.type_pool.canonical_type(symbol_info.type_index)?;
    let tag = *r.enum_variant_indices.get(&symbol)?;
    let TypeKind::Enum { variants, .. } = &r.type_pool.get(ty).kind else {
        return None;
    };
    let variant = variants.iter().find(|variant| variant.tag == tag)?.clone();
    Some((
        EnumVariantRef {
            type_index: ty,
            variant_tag: tag,
        },
        variant,
    ))
}

pub(crate) fn record_reference(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    if let Some((reference, _)) = descriptor(r, ast, node) {
        r.enum_variants.insert(node, reference);
        r.node_types.insert(node, reference.type_index);
    }
}

pub(crate) fn value(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    let (reference, variant) = descriptor(r, ast, node)?;
    r.enum_variants.insert(node, reference);
    if !variant.fields.is_empty() && r.current_call_callee != Some(node) {
        report(
            r,
            ast,
            node,
            "a payload enum variant requires a constructor call",
        );
    }
    Some(reference.type_index)
}

pub(crate) fn construction(r: &mut Resolver<'_>, ast: &Ast, call: NodeIndex) -> Option<TypeIndex> {
    let callee = ast.fixed_children(call)[0];
    let (reference, variant) = descriptor(r, ast, callee)?;
    if variant.fields.is_empty()
        && r.scopes.iter().any(|scope| {
            scope.assoc_type == Some(reference.type_index)
                && scope.bindings.contains_key(&str_interner::intern("apply"))
        })
    {
        // A nullary variant projection is already an enum value. Its apply
        // method takes precedence over the legacy empty constructor call.
        return None;
    }
    let arguments = ast.multi_children(call);
    let symbol = r.node_symbols.get(&callee).copied().or_else(|| {
        (ast.node(callee).kind == NodeKind::Projection)
            .then(|| r.node_symbols.get(&ast.fixed_children(callee)[1]).copied())
            .flatten()
    });
    if let Some(declaration) = symbol.and_then(|symbol| {
        let symbol = &r.symbols[symbol.0 as usize];
        (symbol.kind == SymbolKind::EnumVariant && !symbol.def_node.is_null())
            .then_some(symbol.def_node)
    }) {
        if ast.multi_children(declaration).len() != variant.fields.len() {
            for &argument in arguments {
                typing::resolve_types(r, ast, crate::arguments::argument_value_node(ast, argument));
            }
            return Some(reference.type_index);
        }
        let Some(plan) = crate::arguments::plan_parameters(
            r,
            ast,
            call,
            declaration,
            ast.multi_children(declaration),
            arguments,
            None,
        ) else {
            for &argument in arguments {
                typing::resolve_types(r, ast, crate::arguments::argument_value_node(ast, argument));
            }
            return Some(reference.type_index);
        };
        let mut expected = vec![None; arguments.len()];
        for (index, binding) in plan.parameters.iter().enumerate() {
            match &binding.value {
                crate::CallArgumentValue::Explicit { source_index } => {
                    expected[*source_index] = Some(variant.fields[index].ty)
                }
                crate::CallArgumentValue::Variadic { source_indices } => {
                    for &index in source_indices {
                        expected[index] = Some(type_pool::Intrinsic::Any.type_index());
                    }
                }
                crate::CallArgumentValue::Default { .. }
                | crate::CallArgumentValue::MapVariadic { .. } => {}
            }
        }
        for (&argument, target) in arguments.iter().zip(expected) {
            let value = crate::arguments::argument_value_node(ast, argument);
            typing::resolve_types_expected(r, ast, value, target);
            if let Some(target) = target {
                typing::check_expected_type(r, ast, value, target, "enum constructor field");
            }
        }
        let argument_type = argument_type(r, &variant);
        r.call_arguments.insert(call, plan);
        r.enum_constructions.insert(
            call,
            EnumConstructionPlan {
                type_index: reference.type_index,
                variant_tag: reference.variant_tag,
                argument_type,
                source_fields: None,
            },
        );
        return Some(reference.type_index);
    }
    let mut fields = vec![None; variant.fields.len()];
    let mut positional = 0;
    let mut valid = true;
    for (source_index, &argument) in arguments.iter().enumerate() {
        let value = crate::arguments::argument_value_node(ast, argument);
        let field = if ast.node(argument).kind == NodeKind::NamedArg {
            let key = ast.node(ast.fixed_children(argument)[0]).str_id;
            variant.fields.iter().position(|field| field.name == key)
        } else {
            let field = positional;
            positional += 1;
            (field < fields.len()).then_some(field)
        };
        let target = field.map(|index| variant.fields[index].ty);
        typing::resolve_types_expected(r, ast, value, target);
        if let Some(target) = target {
            typing::check_expected_type(r, ast, value, target, "enum constructor field");
        }
        let Some(field) = field else {
            report(
                r,
                ast,
                argument,
                "unknown or excess enum constructor argument",
            );
            valid = false;
            continue;
        };
        if fields[field].replace(source_index).is_some() {
            report(
                r,
                ast,
                argument,
                "enum constructor field supplied more than once",
            );
            valid = false;
        }
    }
    if fields.iter().any(Option::is_none) {
        report(r, ast, call, "missing required enum constructor fields");
        valid = false;
    }
    if valid {
        let argument_type = argument_type(r, &variant);
        r.enum_constructions.insert(
            call,
            EnumConstructionPlan {
                type_index: reference.type_index,
                variant_tag: reference.variant_tag,
                argument_type,
                source_fields: Some(fields.into_iter().flatten().collect()),
            },
        );
    }
    Some(reference.type_index)
}

fn argument_type(r: &mut Resolver<'_>, variant: &VariantInfo) -> TypeIndex {
    if variant.fields.is_empty() {
        type_pool::Intrinsic::Unit.type_index()
    } else {
        r.register_type(TypeKind::Tuple {
            elements: variant.fields.iter().map(|field| field.ty).collect(),
        })
    }
}

pub(crate) fn resolve_match_names(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    name::resolve_names(r, ast, ast.fixed_children(node)[0]);
    for &arm in ast.multi_children(node) {
        let previous = r.current_scope;
        r.push_scope(arm, None);
        r.node_scopes.insert(arm, r.current_scope);
        let children = ast.fixed_children(arm);
        name::resolve_pattern(r, ast, children[0], NodeKind::CaseArm);
        name::resolve_names(r, ast, children[1]);
        r.current_scope = previous;
    }
}

/// Patterns may contain literal tests and nested tuple/enum extraction.
pub(crate) fn pattern(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
) {
    if node.is_null() {
        return;
    }
    let Some(expected) = expected.and_then(|ty| r.type_pool.canonical_type(ty)) else {
        report(r, ast, node, "pattern matching requires a known input type");
        return;
    };
    r.node_types.insert(node, expected);
    match ast.node(node).kind {
        NodeKind::Id => {
            if let Some((reference, variant)) = descriptor(r, ast, node) {
                check_variant(r, ast, node, expected, reference);
                r.enum_variants.insert(node, reference);
                if !variant.fields.is_empty() {
                    report(
                        r,
                        ast,
                        node,
                        "payload enum patterns must specify field patterns",
                    );
                }
            } else if let Some(&symbol) = r.node_symbols.get(&node) {
                r.symbols[symbol.0 as usize].type_index = expected;
            }
        }
        NodeKind::Underscore => {}
        NodeKind::PatternError | NodeKind::PatternErrorOk => {
            crate::error_patterns::qualified_pattern(r, ast, node, expected)
        }
        NodeKind::PatternTypeFamily => {
            crate::error_patterns::family_pattern(r, ast, node, expected)
        }
        NodeKind::PatternOptionSome => crate::optional::pattern(r, ast, node, expected),
        NodeKind::PatternList => crate::list_patterns::resolve(r, ast, node, expected),
        NodeKind::PatternRestBind => report(
            r,
            ast,
            node,
            "rest binding must be a direct element of a List pattern",
        ),
        NodeKind::PatternTuple => {
            let TypeKind::Tuple { elements } = r.type_pool.get(expected).kind.clone() else {
                report(
                    r,
                    ast,
                    node,
                    "tuple pattern requires a statically known tuple input",
                );
                return;
            };
            let children = ast.multi_children(node);
            if children.len() != elements.len() {
                report(r, ast, node, "tuple pattern arity does not match the input");
            }
            for (&child, ty) in children.iter().zip(elements) {
                pattern(r, ast, child, Some(ty));
            }
        }
        NodeKind::PatternCall | NodeKind::PropertyPattern | NodeKind::Projection => {
            let callee = if ast.node(node).kind == NodeKind::PatternCall {
                ast.fixed_children(node)[0]
            } else {
                node
            };
            let Some((reference, variant)) = descriptor(r, ast, callee) else {
                report(
                    r,
                    ast,
                    node,
                    "constructor patterns require a statically resolved enum variant",
                );
                return;
            };
            check_variant(r, ast, node, expected, reference);
            r.enum_variants.insert(node, reference);
            r.enum_variants.insert(callee, reference);
            let children = if ast.node(node).kind == NodeKind::PatternCall {
                ast.multi_children(node)
            } else {
                &[]
            };
            if children.len() != variant.fields.len() {
                report(
                    r,
                    ast,
                    node,
                    "enum pattern arity does not match the variant fields",
                );
            }
            for (&child, field) in children.iter().zip(variant.fields) {
                pattern(r, ast, child, Some(field.ty));
            }
        }
        NodeKind::Char
        | NodeKind::Int
        | NodeKind::Negative
        | NodeKind::Real
        | NodeKind::Bool
        | NodeKind::Str
        | NodeKind::Null
        | NodeKind::Unit => {
            typing::resolve_types_expected(r, ast, node, Some(expected));
            typing::check_expected_type(r, ast, node, expected, "pattern literal");
        }
        NodeKind::PatternAndIs => {
            let children = ast.fixed_children(node);
            pattern(r, ast, children[0], Some(expected));
            typing::resolve_types(r, ast, children[1]);
            let computed_type = r.node_types.get(&children[1]).copied();
            pattern(r, ast, children[2], computed_type);
        }
        NodeKind::PatternNot => {
            pattern(r, ast, ast.fixed_children(node)[0], Some(expected));
        }
        NodeKind::PatternOr => {
            let children = ast.fixed_children(node);
            pattern(r, ast, children[0], Some(expected));
            pattern(r, ast, children[1], Some(expected));
            crate::pattern_bindings::check_alternative_types(r, ast, node);
        }
        NodeKind::PatternAsBind => {
            let children = ast.fixed_children(node);
            pattern(r, ast, children[0], Some(expected));
            let alias = children[1];
            r.node_types.insert(alias, expected);
            if let Some(&symbol) = r.node_symbols.get(&alias) {
                r.symbols[symbol.0 as usize].type_index = expected;
            }
        }
        NodeKind::PatternIfGuard => {
            let children = ast.fixed_children(node);
            pattern(r, ast, children[0], Some(expected));
            let boolean = type_pool::Intrinsic::Bool.type_index();
            typing::resolve_types_expected(r, ast, children[1], Some(boolean));
            typing::check_expected_type(r, ast, children[1], boolean, "pattern guard");
        }
        _ => report(r, ast, node, "this pattern operation is not implemented"),
    }
}

fn check_variant(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: TypeIndex,
    reference: EnumVariantRef,
) {
    if expected != reference.type_index
        && r.type_pool.as_intrinsic(expected) != Some(type_pool::Intrinsic::Any)
    {
        report(
            r,
            ast,
            node,
            "enum pattern belongs to a different enum type",
        );
    }
}

pub(crate) fn method_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    ty: TypeIndex,
) -> Option<TypeIndex> {
    let name = ast.node(ast.fixed_children(node)[1]).str_id;
    if r.current_call_callee != Some(node) {
        report(
            r,
            ast,
            node,
            "bound enum method values are not implemented; call the method directly",
        );
        return None;
    }
    let symbol = match crate::associated::member(r, ty, name) {
        Ok(symbol) => symbol,
        Err(error) => {
            if error.starts_with("unknown member")
                && let Some(signature) =
                    crate::comparison_derivation::declared_method_signature(r, ast, ty, name)
                        .or_else(|| crate::display_derivation::declared_signature(r, ast, ty, name))
            {
                return Some(signature);
            }
            report(r, ast, node, error);
            return None;
        }
    };
    let symbol = &r.symbols[symbol.0 as usize];
    if symbol.kind != SymbolKind::Function || symbol.def_node.is_null() {
        report(r, ast, node, "enum instance members must be methods");
        return None;
    }
    if ast
        .multi_children(symbol.def_node)
        .first()
        .is_none_or(|&parameter| ast.node(parameter).kind != NodeKind::ParamSelf)
    {
        report(
            r,
            ast,
            node,
            "enum instance methods require a self parameter",
        );
        return None;
    }
    let signature = r.type_pool.canonical_type(symbol.type_index)?;
    let TypeKind::Function { params, ret } = r.type_pool.get(signature).kind.clone() else {
        return None;
    };
    Some(r.register_type(TypeKind::Function {
        params: params.into_iter().skip(1).collect(),
        ret,
    }))
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
    use type_pool::Intrinsic;

    use super::*;

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("enum.ns".into()), source.into());
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
    fn recursive_fields_aliases_and_reordered_arguments_keep_nominal_identity() {
        let (resolved, errors) = resolve(
            "typealias Tree = E\nenum E { none, node(value: i8, next: Tree), }\nfn main() { E.node(next = E.none, value = 42) }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.enum_constructions.values().next().unwrap();
        assert_eq!(plan.variant_tag, 1);
        assert!(plan.source_fields.is_none());
        let arguments = resolved
            .call_arguments
            .values()
            .find(|arguments| {
                resolved.ast.node(arguments.declaration).kind == NodeKind::EnumVariant
            })
            .unwrap();
        assert_eq!(
            arguments.parameters[0].value,
            crate::CallArgumentValue::Explicit { source_index: 1 }
        );
        assert_eq!(
            arguments.parameters[1].value,
            crate::CallArgumentValue::Explicit { source_index: 0 }
        );
        let TypeKind::Enum { variants, .. } = &resolved.type_pool.get(plan.type_index).kind else {
            panic!("enum");
        };
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1].fields[0].ty, Intrinsic::I8.type_index());
        assert_eq!(
            resolved.type_pool.canonical_type(variants[1].fields[1].ty),
            Some(plan.type_index)
        );
        let TypeKind::Tuple { elements } = &resolved.type_pool.get(plan.argument_type).kind else {
            panic!("argument tuple");
        };
        assert_eq!(elements.len(), 2);
        let next = resolved
            .symbols
            .iter()
            .find(|symbol| symbol.name == str_interner::intern("next"))
            .unwrap();
        assert_eq!(
            resolved
                .ast
                .node(resolved.scopes[next.scope.0 as usize].node)
                .kind,
            NodeKind::EnumVariant
        );
    }

    #[test]
    fn enum_and_tuple_patterns_bind_real_field_types_in_arm_scopes() {
        let (resolved, errors) = resolve(
            "enum E { none, value(pair: (i8, bool)), }\nfn main() { E.value((42, true)) match {\nE.value((x, true)) => x\nE.none => 0\n} }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| symbol.name == str_interner::intern("x"))
            .unwrap();
        assert_eq!(symbol.type_index, Intrinsic::I8.type_index());
        assert_eq!(
            resolved
                .ast
                .node(resolved.scopes[symbol.scope.0 as usize].node)
                .kind,
            NodeKind::CaseArm
        );
        assert!(
            resolved
                .enum_variants
                .values()
                .any(|reference| reference.variant_tag == 1)
        );
        let (_, errors) =
            resolve("enum E { some(x:i64), none, }\nfn main() { E.some(42) matches E.some(42) }");
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn constructor_and_pattern_errors_reject_incomplete_plans() {
        for (body, message) in [
            ("E.some()", "missing required"),
            ("E.some(true)", "enum constructor field"),
            ("E.some(x = 1, x = 2)", "more than once"),
            ("E.some(y = 1)", "unknown parameter"),
            ("E.some", "requires a constructor call"),
            ("E.none matches E.some()", "pattern arity"),
            ("E.none matches Other.none", "different enum"),
        ] {
            let (_, errors) = resolve(&format!(
                "enum E {{ none, some(x:i64), }}\nenum Other {{ none, }}\nfn main() {{ {body} }}"
            ));
            assert!(
                errors.iter().any(|error| error.contains(message)),
                "{body}: {errors:?}"
            );
        }
        for source in [
            "enum E { some(...x:Any), }",
            "enum E { some(x:i64,x:bool), }",
        ] {
            let (_, errors) = resolve(source);
            assert!(!errors.is_empty(), "{source}");
        }
    }

    #[test]
    fn matches_bindings_never_escape_failed_partial_or_skipped_tests() {
        for body in [
            "E.none matches E.some(n); n",
            "false and (E.some(42) matches E.some(n)); n",
            "E.pair(42, 3) matches E.pair(n, 0); n",
        ] {
            let (_, errors) = resolve(&format!(
                "enum E {{ none, some(x:i64), pair(x:i64,y:i64), }}\nfn main() {{ {body} }}"
            ));
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("undefined name `n`")),
                "{body}: {errors:?}"
            );
        }
        let (_, errors) = resolve(
            "enum E { some(x:i64), none, }\nfn main() { E.some(42) matches E.some(n) if n > 40 }",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn match_arm_types_and_enum_method_signatures_are_checked() {
        let (_, errors) = resolve(
            "enum E { some(x:i64), none, pub fn value(self) -> i64 { self match { E.some(n) => n, E.none => 0 } } }\nfn main() { E.some(42).value() }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        for source in [
            "fn f()->i64 { false match { true=>42, _=>false } }",
            "enum E { some(x:i64), none, }\nfn f()->i64 { E.none match { E.some(n)=>n, E.none=>false } }",
            "enum E { none, pub fn value(self, n:i64)->i64 { n } }\nfn main() { E.none.value(true) }",
            "fn main() { \"x\" matches 'x' }",
        ] {
            let (_, errors) = resolve(source);
            assert!(!errors.is_empty(), "{source}");
        }
        let (resolved, errors) =
            resolve("fn main() { let n:i64=40; let f:f64=2.0; true match { true=>n, _=>f } }");
        assert!(errors.is_empty(), "{errors:?}");
        let matcher = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| {
                (node.kind == NodeKind::PostMatch).then_some(NodeIndex(index as u32))
            })
            .unwrap();
        assert_eq!(resolved.node_types[&matcher], Intrinsic::F64.type_index());
    }

    #[test]
    fn any_enum_patterns_retain_checked_nominal_tests() {
        let (resolved, errors) = resolve(
            "enum A { one, }\nenum B { one, }\nfn main() { let x: Any = B.one; x match { A.one => 0, B.one => 42, _ => 0 } }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let types: HashSet<_> = resolved
            .enum_variants
            .values()
            .map(|variant| variant.type_index)
            .collect();
        assert_eq!(types.len(), 2);
    }

    #[test]
    fn enum_hash_derivation_and_unimplemented_pattern_operations_are_rejected() {
        let (_, errors) = resolve("enum E { none, }\nderive Hash for E\nfn main() {} ");
        assert!(
            errors.iter().any(|error| error.contains("Hash derivation")),
            "{errors:?}"
        );
        let (_, errors) = resolve("fn main() { 1 matches [1] }");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("List pattern requires List or Any input")),
            "{errors:?}"
        );
    }
}
