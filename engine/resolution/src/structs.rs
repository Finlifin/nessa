//! Checked struct construction plans preserve source evaluation and declaration layout.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{FieldInfo, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{ScopeId, SymbolKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructConstructionPlan {
    pub type_index: TypeIndex,
    pub declaration: NodeIndex,
    pub fields: Vec<StructFieldBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructFieldBinding {
    pub field: NodeIndex,
    pub value: StructFieldValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructFieldValue {
    Explicit { source_index: usize },
    Default { expression: NodeIndex },
}

pub(crate) fn unwrap_member(ast: &Ast, mut node: NodeIndex) -> NodeIndex {
    while matches!(ast.node(node).kind, NodeKind::PubDef | NodeKind::PrivateDef) {
        node = ast.fixed_children(node)[0];
    }
    node
}

fn fields(ast: &Ast, declaration: NodeIndex) -> Vec<NodeIndex> {
    ast.multi_children(declaration)
        .iter()
        .map(|&node| unwrap_member(ast, node))
        .filter(|&node| ast.node(node).kind == NodeKind::StructField)
        .collect()
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

pub(crate) fn validate_default_names(r: &Resolver<'_>, ast: &Ast, expression: NodeIndex) {
    crate::defaults::validate_default_control(r, ast, expression);
    let mut pending = vec![expression];
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        if r.node_symbols
            .get(&node)
            .is_some_and(|symbol| r.symbols[symbol.0 as usize].kind == SymbolKind::Field)
        {
            report(
                r,
                ast,
                node,
                "references to instance fields in struct defaults are not supported",
            );
        }
        if matches!(ast.node(node).kind, NodeKind::Property | NodeKind::NamedArg) {
            pending.push(ast.fixed_children(node)[1]);
        } else {
            pending.extend(ast.fixed_children(node));
            pending.extend(ast.multi_children(node));
        }
    }
}

pub(crate) fn prepare_fields(r: &mut Resolver<'_>, ast: &Ast) {
    let declarations: Vec<_> = r
        .symbols
        .iter()
        .filter_map(|symbol| {
            (!symbol.def_node.is_null() && ast.node(symbol.def_node).kind == NodeKind::StructDef)
                .then_some((symbol.type_index, symbol.def_node))
        })
        .collect();
    for (ty, declaration) in declarations {
        let mut metadata = Vec::new();
        for field in fields(ast, declaration) {
            let children = ast.fixed_children(field);
            let field_ty = crate::typing::resolve_type_expr(r, ast, children[1]);
            let Some(field_ty) = field_ty else {
                report(r, ast, children[1], "invalid struct field type");
                continue;
            };
            if let Some(&symbol) = r.node_symbols.get(&children[0]) {
                r.symbol_mut(symbol).type_index = field_ty;
            }
            metadata.push(FieldInfo {
                name: ast.node(children[0]).str_id,
                ty: field_ty,
                has_default: !children[2].is_null(),
                offset: (metadata.len() * 8) as u32,
            });
        }
        let Some(size) = metadata
            .len()
            .checked_mul(8)
            .and_then(|size| u32::try_from(size).ok())
        else {
            report(r, ast, declaration, "struct payload is too large");
            continue;
        };
        let info = r.type_pool.get_mut(ty);
        if let TypeKind::Struct { fields, .. } = &mut info.kind {
            *fields = metadata;
            // Source fields are TaggedValue slots, independently of field type.
            info.size = size;
            info.align = 8;
        }
    }
}

pub(crate) fn resolve_defaults(r: &mut Resolver<'_>, ast: &Ast, declaration: NodeIndex) {
    for field in fields(ast, declaration) {
        let children = ast.fixed_children(field);
        if children[2].is_null() {
            continue;
        }
        if let Some(&symbol) = r.node_symbols.get(&children[0]) {
            let ty = r.symbols[symbol.0 as usize].type_index;
            if ty != TypeIndex::INVALID {
                crate::typing::resolve_types_expected(r, ast, children[2], Some(ty));
                crate::typing::check_expected_type(r, ast, children[2], ty, "struct field default");
            }
        }
    }
}

pub(crate) fn resolve_construction(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
) -> Option<TypeIndex> {
    let callee = ast.fixed_children(node)[0];
    crate::typing::resolve_types(r, ast, callee);
    let ty = r
        .node_symbols
        .get(&callee)
        .and_then(|symbol| {
            let symbol = &r.symbols[symbol.0 as usize];
            (symbol.kind == SymbolKind::Type).then_some(symbol.type_index)
        })
        .or_else(|| r.node_type_values.get(&callee).copied())
        .and_then(|ty| r.type_pool.canonical_type(ty));
    if ty.is_some_and(|ty| r.type_pool.is_reserved_collection_role(ty)) {
        report(
            r,
            ast,
            node,
            "collection storage cannot be constructed as a struct; use the collection constructor",
        );
        return ty;
    }
    let declaration = ty.and_then(|ty| {
        r.symbols
            .iter()
            .find(|symbol| {
                symbol.type_index == ty
                    && !symbol.def_node.is_null()
                    && ast.node(symbol.def_node).kind == NodeKind::StructDef
            })
            .map(|symbol| symbol.def_node)
    });
    let (Some(ty), Some(declaration)) = (ty, declaration) else {
        report(
            r,
            ast,
            callee,
            "field construction requires a statically known struct type",
        );
        for &arg in ast.multi_children(node) {
            let value = if ast.node(arg).kind == NodeKind::Property {
                ast.fixed_children(arg)[1]
            } else {
                arg
            };
            crate::typing::resolve_types(r, ast, value);
        }
        return None;
    };
    let field_nodes = fields(ast, declaration);
    let mut bindings = vec![None; field_nodes.len()];
    let mut valid = true;
    let saved_scope = r.current_scope;
    r.current_scope = r.node_scopes.get(&node).copied().unwrap_or(ScopeId::ROOT);
    for (source_index, &arg) in ast.multi_children(node).iter().enumerate() {
        let (name, value) = match ast.node(arg).kind {
            NodeKind::Property => {
                let children = ast.fixed_children(arg);
                (ast.node(children[0]).str_id, children[1])
            }
            NodeKind::Id => (ast.node(arg).str_id, arg),
            _ => {
                report(
                    r,
                    ast,
                    arg,
                    "struct arguments require named properties or identifier shorthand",
                );
                valid = false;
                crate::typing::resolve_types(r, ast, arg);
                continue;
            }
        };
        let Some(slot) = field_nodes
            .iter()
            .position(|&field| ast.node(ast.fixed_children(field)[0]).str_id == name)
        else {
            report(
                r,
                ast,
                arg,
                format!("unknown struct field `{}`", str_interner::get(name)),
            );
            valid = false;
            crate::typing::resolve_types(r, ast, value);
            continue;
        };
        if bindings[slot].is_some() {
            report(r, ast, arg, "duplicate struct field argument");
            valid = false;
        }
        let member = crate::associated::member(r, ty, name);
        match member {
            Ok(symbol) => {
                let expected = r.symbols[symbol.0 as usize].type_index;
                crate::typing::resolve_types_expected(r, ast, value, Some(expected));
                crate::typing::check_expected_type(r, ast, value, expected, "struct field");
            }
            Err(error) => {
                report(r, ast, arg, error);
                valid = false;
                crate::typing::resolve_types(r, ast, value);
            }
        }
        bindings[slot] = Some(StructFieldValue::Explicit { source_index });
    }
    r.current_scope = saved_scope;
    let mut planned = Vec::new();
    for (slot, &field) in field_nodes.iter().enumerate() {
        let children = ast.fixed_children(field);
        let value = match bindings[slot].take() {
            Some(value) => value,
            None if !children[2].is_null() => StructFieldValue::Default {
                expression: children[2],
            },
            None => {
                report(
                    r,
                    ast,
                    node,
                    format!(
                        "missing required struct field `{}`",
                        str_interner::get(ast.node(children[0]).str_id)
                    ),
                );
                valid = false;
                continue;
            }
        };
        planned.push(StructFieldBinding { field, value });
    }
    if valid {
        r.struct_constructions.insert(
            node,
            StructConstructionPlan {
                type_index: ty,
                declaration,
                fields: planned,
            },
        );
    }
    Some(ty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostic::{DiagnosticContext, Level};
    use rustc_span::{
        FileName,
        source_map::{FilePathMapping, SourceMap},
    };

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("structs.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.level == Level::Error)
            .map(|diagnostic| format!("{diagnostic:?}"))
            .collect();
        (resolved, errors)
    }

    #[test]
    fn source_struct_layout_and_native_display_plans_use_tagged_value_slots() {
        let (resolved, errors) =
            resolve("struct P{small:i8,text:String};typealias Alias=P;derive Display for Alias");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(resolved.display_derivations.len(), 1);
        let plan = &resolved.display_derivations[0];
        let info = resolved.type_pool.get(plan.implementor);
        assert_eq!(info.size, 16);
        assert_eq!(info.align, 8);
        let TypeKind::Struct { fields, .. } = &info.kind else {
            panic!("struct");
        };
        assert_eq!(
            fields.iter().map(|field| field.offset).collect::<Vec<_>>(),
            [0, 8]
        );
        let TypeKind::Function { params, ret } = &resolved.type_pool.get(plan.signature).kind
        else {
            panic!("signature");
        };
        assert_eq!(params, &[plan.implementor]);
        assert_eq!(*ret, type_pool::Intrinsic::Str.type_index());
        let schema = resolved.type_pool.trait_schema(plan.trait_type).unwrap();
        resolved
            .type_pool
            .check_native_derived_method(&schema.slots[0], plan.implementor)
            .unwrap();
    }

    #[test]
    fn native_display_derivation_does_not_overwrite_existing_implementations() {
        for source in [
            "struct P{};derive Display for P;derive Display for P",
            "struct P{};impl Display for P{pub fn to_string(self)->String{\"custom\"}};derive Display for P",
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("already implemented")),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn forward_alias_construction_binds_layout_and_typed_defaults() {
        let (resolved, errors) = resolve(
            "fn main() -> P { A { y: 40, x: 2 } }\ntypealias A = P\nstruct P { x: i64, y: i64, z: i8 = 7 }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.struct_constructions.values().next().unwrap();
        assert_eq!(plan.fields.len(), 3);
        assert_eq!(
            plan.fields[0].value,
            StructFieldValue::Explicit { source_index: 1 }
        );
        assert_eq!(
            plan.fields[1].value,
            StructFieldValue::Explicit { source_index: 0 }
        );
        let StructFieldValue::Default { expression } = plan.fields[2].value else {
            panic!("missing default")
        };
        assert_eq!(
            resolved.node_types[&expression],
            type_pool::Intrinsic::I8.type_index()
        );
        let TypeKind::Struct { fields, .. } = &resolved.type_pool.get(plan.type_index).kind else {
            panic!("not struct")
        };
        assert!(fields[2].has_default);
    }

    #[test]
    fn shorthand_and_private_same_type_impl_are_checked() {
        let (_, errors) = resolve(
            "struct P { private x: i64, pub y: i64 = 2 }\nimpl P { pub fn make(x: i64) -> P { P { x } }\npub fn read(p: P) -> i64 { p.x } }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) =
            resolve("struct P { private x: i64 = 2 }\nfn main() -> i64 { let p = P {}; p.x }");
        assert!(
            errors.iter().any(|error| error.contains("not visible")),
            "{errors:?}"
        );
    }

    #[test]
    fn malformed_constructions_and_unused_defaults_are_rejected() {
        for (source, expected) in [
            ("struct P { x:i64 }\nfn main(){ P {} }", "missing required"),
            (
                "struct P { x:i64 }\nfn main(){ P { z:1 } }",
                "unknown struct field",
            ),
            (
                "struct P { x:i64 }\nfn main(){ P { x:1, x:2 } }",
                "duplicate struct field",
            ),
            (
                "struct P { x:i64 }\nfn main(){ P { 1 } }",
                "named properties",
            ),
            (
                "struct P { x:i64 }\nfn main(){ let p=P{x:1}; p{x:2} }",
                "known struct",
            ),
            ("struct P { x:bool = 1 }", "type mismatch"),
            ("struct P { x:i64=1, y:i64=x }", "instance fields"),
            (
                "struct P { x:i64=if true {return 42}else 0 }",
                "default boundary",
            ),
            (
                "struct P { private x:i64 }\nfn main(){ P{x:1} }",
                "not visible",
            ),
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains(expected)),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn struct_and_parameter_default_cycles_share_expansion_guard() {
        let (_, errors) =
            resolve("struct P { x:i64=f() }\nfn f(.p:P=P{}) -> i64 { 42 }\nfn main(){ P{} }");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("recursive default")),
            "{errors:?}"
        );
    }
}
