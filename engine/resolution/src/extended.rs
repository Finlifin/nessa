//! Checked extended application shares ordinary call plans and replay authority.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{CallArgumentPlan, CallArgumentValue, CallParameterBinding, SymbolKind};

pub(crate) fn resolve(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    let callee = ast.fixed_children(node)[0];
    let previous = r.current_call_callee.replace(callee);
    crate::typing::resolve_types(r, ast, callee);
    r.current_call_callee = previous;
    let constructor = r
        .node_type_values
        .get(&callee)
        .copied()
        .or_else(|| {
            r.node_symbols.get(&callee).and_then(|symbol| {
                let symbol = &r.symbols[symbol.0 as usize];
                (symbol.kind == SymbolKind::Type).then_some(symbol.type_index)
            })
        })
        .and_then(|ty| r.type_pool.canonical_type(ty));
    let struct_instance = r
        .node_types
        .get(&callee)
        .and_then(|&ty| r.type_pool.canonical_type(ty))
        .is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::Struct { .. }));
    if constructor.is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::Struct { .. }))
        || struct_instance
    {
        return crate::structs::resolve_construction(r, ast, node);
    }
    let declaration = r
        .instance_methods
        .get(&callee)
        .map(|symbol| r.symbols[symbol.0 as usize].def_node)
        .or_else(|| crate::arguments::known_declaration(r, ast, callee));
    let mut result = None;
    if let Some(declaration) = declaration {
        let parameters = crate::arguments::caller_parameters(ast, declaration);
        if crate::arguments::validate_variadic_parameters(r, ast, declaration, &parameters)
            == Some(crate::variadics::Layout::ListMap)
        {
            let mut children = Vec::new();
            let mut properties = Vec::new();
            for (index, &argument) in ast.multi_children(node).iter().enumerate() {
                if ast.node(argument).kind == NodeKind::Property {
                    properties.push((ast.node(ast.fixed_children(argument)[0]).str_id, index));
                } else {
                    children.push(index);
                }
            }
            r.call_arguments.insert(
                node,
                CallArgumentPlan {
                    declaration,
                    parameters: parameters
                        .iter()
                        .zip([
                            CallArgumentValue::Variadic {
                                source_indices: children,
                            },
                            CallArgumentValue::MapVariadic {
                                source_properties: properties,
                            },
                        ])
                        .map(|(&parameter, value)| CallParameterBinding {
                            parameter,
                            symbols: crate::arguments::parameter_symbols(r, ast, parameter),
                            value,
                        })
                        .collect(),
                },
            );
            let signature = r
                .node_types
                .get(&callee)
                .copied()
                .filter(|&ty| {
                    r.type_pool.canonical_type(ty).is_some_and(|ty| {
                        matches!(
                            r.type_pool.get(ty).kind,
                            TypeKind::Function { .. } | TypeKind::Effect { .. }
                        )
                    })
                })
                .or_else(|| {
                    r.node_symbols
                        .get(&callee)
                        .map(|symbol| r.symbols[symbol.0 as usize].type_index)
                });
            result = signature
                .and_then(|ty| r.type_pool.canonical_type(ty))
                .and_then(|ty| match r.type_pool.get(ty).kind {
                    TypeKind::Function { ret, .. } | TypeKind::Effect { ret, .. } => Some(ret),
                    _ => None,
                });
        } else {
            report(
                r,
                ast,
                node,
                "extended application requires a dual variadic List/Map declaration or a known struct constructor",
            );
        }
    } else {
        report(
            r,
            ast,
            node,
            "extended application requires a statically known dual variadic declaration or struct constructor; function values lack packing metadata",
        );
    }
    // Keys are literal source names. Every value is checked even for invalid calls.
    for &argument in ast.multi_children(node) {
        let value = crate::argument_value_node(ast, argument);
        let expected = Intrinsic::Any.type_index();
        crate::typing::resolve_types_expected(r, ast, value, Some(expected));
        crate::typing::check_expected_type(r, ast, value, expected, "extended argument");
    }
    result
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: &str) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

#[cfg(test)]
mod tests {
    use crate::CallArgumentValue;
    use ast::NodeKind;
    use type_pool::{Intrinsic, TraitParameterKind, TypeKind};

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let source = format!("typealias List=.List'builtin;typealias Map=.Map'builtin;{source}");
        let (tokens, errors) = lexer::tokenize(&source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let file = map.new_source_file(
            rustc_span::FileName::Custom("extended.ns".into()),
            source.clone(),
        );
        let diagnostics = diagnostic::DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, &source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve_with_options(
            ast,
            &diagnostics,
            crate::ResolveOptions::for_builtin_package(),
        );
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|error| error.message.clone())
            .collect();
        (resolved, errors)
    }

    #[test]
    fn checked_packing_retains_interleaved_source_indices_and_literal_keys() {
        let (resolved, errors) = resolve(
            "fn gather(...children:List,...properties:Map)->i64{42};fn main(){gather{40,answer:1,2,answer:2}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (&call, plan) = resolved
            .call_arguments
            .iter()
            .find(|(call, _)| resolved.ast.node(**call).kind == NodeKind::ExtendedCall)
            .unwrap();
        assert_eq!(resolved.node_types[&call], Intrinsic::I64.type_index());
        assert_eq!(plan.parameters.len(), 2);
        assert_eq!(
            plan.parameters[0].value,
            CallArgumentValue::Variadic {
                source_indices: vec![0, 2]
            }
        );
        assert_eq!(
            plan.parameters[1].value,
            CallArgumentValue::MapVariadic {
                source_properties: vec![
                    (str_interner::intern("answer"), 1),
                    (str_interner::intern("answer"), 3)
                ]
            }
        );
        for &argument in resolved.ast.multi_children(call) {
            if resolved.ast.node(argument).kind == NodeKind::Property {
                assert!(
                    !resolved
                        .node_symbols
                        .contains_key(&resolved.ast.fixed_children(argument)[0])
                );
            }
        }
    }

    #[test]
    fn hidden_receiver_and_catch_are_not_packed_caller_slots() {
        for source in [
            "struct P{};impl P{pub fn gather(self,...xs:List,...props:Map)->i64{42}};fn main(){P{}.gather{}}",
            "effect gather(catch k,...xs:List,...props:Map)->i64;fn main(){gather{}}",
            "effect gather(...xs:List,catch k,...props:Map)->i64;fn main(){gather{}}",
            "effect gather(...xs:List,...props:Map,catch k)->i64;fn main(){gather{}}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            let plan = resolved.call_arguments.values().next().unwrap();
            assert_eq!(plan.parameters.len(), 2);
            assert!(
                plan.parameters
                    .iter()
                    .all(|binding| resolved.ast.node(binding.parameter).kind
                        == NodeKind::ParamVarargs)
            );
        }
    }

    #[test]
    fn invalid_unused_layouts_and_unknown_extended_values_are_rejected() {
        for (source, fragment) in [
            (
                "fn f(...a:List,...b:Map,x:i64){42}",
                "exactly List then Map",
            ),
            ("fn f(...a:Map,...b:List){42}", "exactly List then Map"),
            ("fn f(...a:List,...b:List){42}", "exactly List then Map"),
            ("fn f(...a:Map){42}", "requires a List type annotation"),
            (
                "fn f(...a:List,...b:Map){42};fn main(){f([],null)}",
                "require extended application",
            ),
            (
                "fn f(...a:List,...b:Map){42};fn main(){let g=f;g{}}",
                "lack packing metadata",
            ),
            (
                "enum E{value(...a:List,...b:Map)}",
                "dual variadic enum variants are not supported",
            ),
            (
                "fn main(){let f=|...a:List,...b:Map,x:i64|42;42}",
                "exactly List then Map",
            ),
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains(fragment)),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn function_results_and_trait_modes_remain_precise() {
        let (resolved, errors) = resolve(
            "fn wrapper(){leaf{}};fn leaf(...a:List,...b:Map){true};struct P{};trait Gather{fn gather(self,...a:List,...b:Map)->i64};impl Gather for P{pub fn gather(self,...a:List,...b:Map)->i64{42}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "wrapper")
            .unwrap();
        let TypeKind::Function { ret, .. } = resolved.type_pool.get(symbol.type_index).kind else {
            panic!("function type missing")
        };
        assert_eq!(ret, Intrinsic::Bool.type_index());
        let snapshot = resolved.type_pool.snapshot();
        assert!(
            snapshot
                .trait_schemas
                .iter()
                .flat_map(|schema| &schema.slots)
                .filter_map(|method| method.signature.as_ref())
                .any(|signature| signature.parameter_kinds
                    == [
                        TraitParameterKind::Receiver,
                        TraitParameterKind::ListVariadic,
                        TraitParameterKind::MapVariadic
                    ])
        );
    }
}
