//! Declaration-aware argument binding. Evaluation order remains source order;
//! lowering applies this parameter-order plan only after explicit values exist.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};

use crate::resolver::Resolver;
use crate::{SymbolId, SymbolKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArgumentPlan {
    pub declaration: NodeIndex,
    pub parameters: Vec<CallParameterBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallParameterBinding {
    pub parameter: NodeIndex,
    pub symbols: Vec<SymbolId>,
    pub value: CallArgumentValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallArgumentValue {
    Explicit {
        source_index: usize,
    },
    /// Additional positional values packed into one List parameter.
    Variadic {
        source_indices: Vec<usize>,
    },
    /// Source-ordered properties packed into one Map; repeated keys replace earlier values.
    MapVariadic {
        source_properties: Vec<(str_interner::StrId, usize)>,
    },
    Default {
        expression: NodeIndex,
        parameter_references: Vec<SymbolId>,
    },
}

/// Strip a named argument or property wrapper; its key is not an evaluated value.
pub fn argument_value_node(ast: &Ast, argument: NodeIndex) -> NodeIndex {
    if matches!(
        ast.node(argument).kind,
        NodeKind::NamedArg | NodeKind::Property
    ) {
        ast.fixed_children(argument)[1]
    } else {
        argument
    }
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

fn parameter_pattern(ast: &Ast, parameter: NodeIndex) -> NodeIndex {
    if ast.node(parameter).kind == NodeKind::ParamSelf {
        parameter
    } else {
        ast.fixed_children(parameter)[0]
    }
}

pub(crate) fn parameter_symbols(
    r: &Resolver<'_>,
    ast: &Ast,
    parameter: NodeIndex,
) -> Vec<SymbolId> {
    let pattern = parameter_pattern(ast, parameter);
    let mut symbols = Vec::new();
    let mut pending = vec![pattern];
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        if let Some(&symbol) = r.node_symbols.get(&node)
            && r.symbols[symbol.0 as usize].def_node == node
            && !symbols.contains(&symbol)
        {
            symbols.push(symbol);
        }
        pending.extend(ast.fixed_children(node));
        pending.extend(ast.multi_children(node));
    }
    symbols
}

fn references(r: &Resolver<'_>, ast: &Ast, expression: NodeIndex) -> HashSet<SymbolId> {
    let mut symbols = HashSet::new();
    let mut pending = vec![expression];
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        if let Some(&symbol) = r.node_symbols.get(&node) {
            symbols.insert(symbol);
        }
        if ast.node(node).kind == NodeKind::NamedArg {
            pending.push(argument_value_node(ast, node));
        } else {
            pending.extend(ast.fixed_children(node));
        }
        pending.extend(ast.multi_children(node));
    }
    symbols
}

/// Resolve defaults in their callable's declaration scope after names are bound.
/// Only preceding parameter bindings are available to a default expression.
pub(crate) fn resolve_default_names(r: &mut Resolver<'_>, ast: &Ast, declaration: NodeIndex) {
    let parameters = ast.multi_children(declaration);
    let mut names = HashSet::new();
    for &parameter in parameters {
        let pattern = parameter_pattern(ast, parameter);
        // ParamSelf carries no identifier payload; its default StrId is not a name.
        let name = match ast.node(pattern).kind {
            NodeKind::ParamSelf => Some(str_interner::intern("self")),
            NodeKind::Id => Some(ast.node(pattern).str_id),
            _ => None,
        };
        if name.is_some_and(|name| !names.insert(name)) {
            report(r, ast, parameter, "duplicate parameter name");
        }
    }
    for (index, &parameter) in parameters.iter().enumerate() {
        if ast.node(parameter).kind != NodeKind::ParamOptional {
            continue;
        }
        let default = ast.fixed_children(parameter)[2];
        crate::name::resolve_names(r, ast, default);
        crate::defaults::validate_default_control(r, ast, default);
        let referenced = references(r, ast, default);
        if parameters
            .iter()
            .filter(|&&parameter| ast.node(parameter).kind == NodeKind::ParamCatch)
            .flat_map(|&parameter| parameter_symbols(r, ast, parameter))
            .any(|symbol| referenced.contains(&symbol))
        {
            report(
                r,
                ast,
                default,
                "a parameter default cannot reference a runtime-provided catch binding",
            );
        }
        let forbidden = parameters[index..]
            .iter()
            .flat_map(|&parameter| parameter_symbols(r, ast, parameter));
        if forbidden
            .into_iter()
            .any(|symbol| referenced.contains(&symbol))
        {
            report(
                r,
                ast,
                default,
                "a parameter default cannot reference itself or a later parameter",
            );
        }
    }
}

/// An immutable named declaration or a literal lambda can provide argument keys.
/// General function values carry only FnType, which does not encode defaults.
pub(crate) fn known_declaration(
    r: &Resolver<'_>,
    ast: &Ast,
    callee: NodeIndex,
) -> Option<NodeIndex> {
    if ast.node(callee).kind == NodeKind::Lambda {
        return Some(callee);
    }
    let symbol = &r.symbols[r.node_symbols.get(&callee)?.0 as usize];
    if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Effect)
        || symbol.def_node.is_null()
    {
        return None;
    }
    matches!(
        (symbol.kind, ast.node(symbol.def_node).kind),
        (
            SymbolKind::Function,
            NodeKind::FunctionDef | NodeKind::TraitDefFn | NodeKind::TraitDeriveFn
        ) | (
            SymbolKind::Effect,
            NodeKind::EffectDef | NodeKind::AsyncEffectDef
        )
    )
    .then_some(symbol.def_node)
}

pub(crate) fn plan_call(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call: NodeIndex,
) -> Option<CallArgumentPlan> {
    let callee = ast.fixed_children(call)[0];
    let arguments = ast.multi_children(call);
    if let Some(&method) = r.instance_methods.get(&callee) {
        let declaration = r.symbols[method.0 as usize].def_node;
        return plan_method(r, ast, call, declaration, arguments, None);
    }
    let Some(declaration) = known_declaration(r, ast, callee) else {
        for &argument in arguments {
            if ast.node(argument).kind == NodeKind::NamedArg {
                report(
                    r,
                    ast,
                    argument,
                    "named arguments require a statically known function declaration",
                );
            }
        }
        return None;
    };
    let parameters = ast.multi_children(declaration);
    if parameters
        .iter()
        .any(|&parameter| ast.node(parameter).kind == NodeKind::ParamSelf)
    {
        return None;
    }
    let caller_parameters = caller_parameters(ast, declaration);
    plan_parameters(
        r,
        ast,
        call,
        declaration,
        &caller_parameters,
        arguments,
        None,
    )
}

/// A catch slot is injected by dispatch, never bound by the effect's caller.
pub(crate) fn caller_parameters(ast: &Ast, declaration: NodeIndex) -> Vec<NodeIndex> {
    ast.multi_children(declaration)
        .iter()
        .copied()
        .filter(|&parameter| {
            !matches!(
                ast.node(parameter).kind,
                NodeKind::ParamCatch | NodeKind::ParamSelf
            )
        })
        .collect()
}

pub(crate) fn plan_method(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    declaration: NodeIndex,
    arguments: &[NodeIndex],
    assignment_rhs: Option<usize>,
) -> Option<CallArgumentPlan> {
    plan_parameters(
        r,
        ast,
        node,
        declaration,
        &ast.multi_children(declaration)[1..],
        arguments,
        assignment_rhs,
    )
}

pub(crate) fn validate_variadic_parameters(
    r: &Resolver<'_>,
    ast: &Ast,
    declaration: NodeIndex,
    parameters: &[NodeIndex],
) -> Option<crate::variadics::Layout> {
    let source = ast.multi_children(declaration);
    if source
        .iter()
        .filter(|&&parameter| ast.node(parameter).kind == NodeKind::ParamVarargs)
        .count()
        > 1
    {
        if source.iter().enumerate().any(|(index, &parameter)| {
            index != 0 && ast.node(parameter).kind == NodeKind::ParamSelf
        }) {
            report(
                r,
                ast,
                declaration,
                "self must be the first parameter of a dual variadic method",
            );
            return None;
        }
        if !matches!(
            ast.node(declaration).kind,
            NodeKind::EffectDef | NodeKind::AsyncEffectDef
        ) && source
            .iter()
            .any(|&parameter| ast.node(parameter).kind == NodeKind::ParamCatch)
        {
            report(
                r,
                ast,
                declaration,
                "catch parameters are only supported in effect declarations",
            );
            return None;
        }
    }
    match crate::variadics::layout(r, ast, parameters) {
        Ok(crate::variadics::Layout::ListMap)
            if ast.node(declaration).kind == NodeKind::EnumVariant =>
        {
            report(
                r,
                ast,
                declaration,
                "dual variadic enum variants are not supported",
            );
            None
        }
        Ok(layout) => Some(layout),
        Err(message) => {
            report(r, ast, declaration, message);
            None
        }
    }
}

pub(crate) fn plan_parameters(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call: NodeIndex,
    declaration: NodeIndex,
    parameters: &[NodeIndex],
    arguments: &[NodeIndex],
    assignment_rhs: Option<usize>,
) -> Option<CallArgumentPlan> {
    let variadic = match validate_variadic_parameters(r, ast, declaration, parameters)? {
        crate::variadics::Layout::Fixed => None,
        crate::variadics::Layout::List(index) => Some(index),
        crate::variadics::Layout::ListMap => {
            report(
                r,
                ast,
                call,
                "dual variadic functions require extended application syntax",
            );
            return None;
        }
    };
    let mut by_name = HashMap::new();
    let mut positional = Vec::new();
    for (index, &parameter) in parameters.iter().enumerate() {
        let pattern = parameter_pattern(ast, parameter);
        if ast.node(pattern).kind == NodeKind::Id {
            by_name.insert(ast.node(pattern).str_id, index);
        }
        if !matches!(
            ast.node(parameter).kind,
            NodeKind::ParamOptional | NodeKind::ParamVarargs
        ) {
            positional.push(index);
        }
    }
    let mut values = vec![None; parameters.len()];
    if let Some(index) = variadic {
        values[index] = Some(CallArgumentValue::Variadic {
            source_indices: Vec::new(),
        });
    }
    let mut next_position = 0;
    let mut valid = true;
    for (source_index, &argument) in arguments.iter().enumerate() {
        let index = if assignment_rhs == Some(source_index) && variadic.is_none() {
            let Some(&index) = positional.last() else {
                report(
                    r,
                    ast,
                    argument,
                    "update requires a positional parameter for the assigned value",
                );
                valid = false;
                continue;
            };
            index
        } else if ast.node(argument).kind == NodeKind::NamedArg {
            let name = ast.node(ast.fixed_children(argument)[0]).str_id;
            let Some(&index) = by_name.get(&name) else {
                report(
                    r,
                    ast,
                    argument,
                    format!("unknown parameter `{}`", str_interner::get(name)),
                );
                valid = false;
                continue;
            };
            if Some(index) == variadic {
                report(
                    r,
                    ast,
                    argument,
                    "variadic parameters collect positional arguments and cannot be supplied by name",
                );
                valid = false;
                continue;
            }
            index
        } else {
            let Some(&index) = positional.get(next_position) else {
                if let Some(index) = variadic
                    && let Some(CallArgumentValue::Variadic { source_indices }) = &mut values[index]
                {
                    source_indices.push(source_index);
                    continue;
                }
                report(
                    r,
                    ast,
                    argument,
                    "too many positional arguments; optional parameters must be supplied by name",
                );
                valid = false;
                continue;
            };
            next_position += 1;
            index
        };
        if values[index].is_some() {
            report(r, ast, argument, "parameter supplied more than once");
            valid = false;
        } else {
            values[index] = Some(CallArgumentValue::Explicit { source_index });
            if ast.node(argument).kind == NodeKind::NamedArg {
                let pattern = parameter_pattern(ast, parameters[index]);
                if let Some(&symbol) = r.node_symbols.get(&pattern) {
                    r.node_symbols
                        .insert(ast.fixed_children(argument)[0], symbol);
                }
            }
        }
    }
    let all_parameter_symbols: Vec<_> = parameters
        .iter()
        .flat_map(|&parameter| parameter_symbols(r, ast, parameter))
        .collect();
    let mut bindings = Vec::new();
    for (index, &parameter) in parameters.iter().enumerate() {
        let value = values[index].take().or_else(|| {
            if ast.node(parameter).kind == NodeKind::ParamOptional {
                let expression = ast.fixed_children(parameter)[2];
                let referenced = references(r, ast, expression);
                Some(CallArgumentValue::Default {
                    expression,
                    parameter_references: all_parameter_symbols
                        .iter()
                        .copied()
                        .filter(|symbol| referenced.contains(symbol))
                        .collect(),
                })
            } else {
                report(
                    r,
                    ast,
                    call,
                    format!("missing required parameter at position {}", index + 1),
                );
                valid = false;
                None
            }
        });
        if let Some(value) = value {
            bindings.push(CallParameterBinding {
                parameter,
                symbols: parameter_symbols(r, ast, parameter),
                value,
            });
        }
    }
    valid.then_some(CallArgumentPlan {
        declaration,
        parameters: bindings,
    })
}

/// Lowering expands selected defaults, never the ordinary callee body.
#[cfg(test)]
mod tests {
    use diagnostic::{DiagnosticContext, Level};
    use rustc_span::{
        FileName,
        source_map::{FilePathMapping, SourceMap},
    };
    use type_pool::Intrinsic;

    use super::*;
    use crate::ResolvedAst;

    fn resolve(source: &str) -> (ResolvedAst, Vec<diagnostic::Diagnostic>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("arguments.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let parser = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos);
        let ast = parser.parse();
        let resolved = crate::resolve(ast, &diagnostics);
        (resolved, diagnostics.diagnostics().to_vec())
    }

    fn checked(source: &str) -> ResolvedAst {
        let (resolved, diagnostics) = resolve(source);
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level == Level::Error),
            "{source}: {diagnostics:?}"
        );
        resolved
    }

    #[test]
    fn receiver_parameter_identity_does_not_use_its_unused_string_payload() {
        let source = "struct Box {}\nimpl Box { fn push(self, value: Any) { value } }";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("receiver.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let mut ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        // Reproduce the collision without depending on which name was interned first.
        for node in &mut ast.nodes {
            if node.kind == NodeKind::ParamSelf {
                node.str_id = str_interner::intern("value");
            }
        }
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        let receiver = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| {
                (node.kind == NodeKind::ParamSelf).then_some(NodeIndex(index as u32))
            })
            .unwrap();
        let symbol = resolved.node_symbols[&receiver];
        assert_eq!(
            resolved.symbols[symbol.0 as usize].name,
            str_interner::intern("self")
        );
    }

    #[test]
    fn named_arguments_keep_source_order_but_bind_parameter_order() {
        let resolved = checked(
            "fn f(a: i64, .b: i8 = 7, .c: i64 = a + b) -> i64 { c }\nfn main() -> i64 { f(c = 40, a = 2) }",
        );
        let (&call, plan) = resolved.call_arguments.iter().next().unwrap();
        assert_eq!(plan.parameters.len(), 3);
        assert_eq!(
            plan.parameters[0].value,
            CallArgumentValue::Explicit { source_index: 1 }
        );
        assert!(matches!(
            plan.parameters[1].value,
            CallArgumentValue::Default { .. }
        ));
        assert_eq!(
            plan.parameters[2].value,
            CallArgumentValue::Explicit { source_index: 0 }
        );
        for &argument in resolved.ast.multi_children(call) {
            assert_eq!(resolved.ast.node(argument).kind, NodeKind::NamedArg);
            let key = resolved.ast.fixed_children(argument)[0];
            let symbol = resolved.node_symbols[&key];
            assert!(
                plan.parameters
                    .iter()
                    .any(|binding| binding.symbols.contains(&symbol))
            );
        }
        let default = match plan.parameters[1].value {
            CallArgumentValue::Default { expression, .. } => expression,
            _ => unreachable!(),
        };
        assert_eq!(resolved.node_types[&default], Intrinsic::I8.type_index());
    }

    #[test]
    fn defaults_reference_previous_parameter_symbols_and_declaration_scope() {
        let resolved = checked(
            "const outer: i64 = 40\nfn f(a: i64, .b: i64 = a + outer, .c: i64 = b + a) -> i64 { c }\nfn main() -> i64 { let outer = true; f(2) }",
        );
        let plan = resolved.call_arguments.values().next().unwrap();
        let a = plan.parameters[0].symbols[0];
        let b = plan.parameters[1].symbols[0];
        let CallArgumentValue::Default {
            expression,
            parameter_references,
        } = &plan.parameters[1].value
        else {
            panic!("default")
        };
        assert_eq!(parameter_references, &[a]);
        let outer_reference = resolved.ast.fixed_children(*expression)[1];
        let symbol = &resolved.symbols[resolved.node_symbols[&outer_reference].0 as usize];
        assert_eq!(symbol.kind, SymbolKind::Constant);
        let CallArgumentValue::Default {
            parameter_references,
            ..
        } = &plan.parameters[2].value
        else {
            panic!("default")
        };
        assert_eq!(parameter_references, &[a, b]);
    }

    #[test]
    fn optional_and_named_argument_errors_are_explicit() {
        for (source, expected) in [
            (
                "fn f(.a: i64 = 1) {}\nfn main() { f(42) }",
                "optional parameters must",
            ),
            (
                "fn f(a: i64) {}\nfn main() { f(a = 1, a = 2) }",
                "more than once",
            ),
            (
                "fn f(a: i64) {}\nfn main() { f(1, a = 2) }",
                "more than once",
            ),
            (
                "fn f(a: i64) {}\nfn main() { f(other = 2) }",
                "unknown parameter",
            ),
            (
                "fn f(a: i64, .b: i64 = 1) {}\nfn main() { f(b = 2) }",
                "missing required",
            ),
            ("fn f(.a: i64 = true) {}", "parameter default"),
            ("fn f(.a: i8 = 128) {}", "out of range"),
            ("fn f(.a: i64 = a) {}", "itself or a later"),
            (
                "const later: i64 = 42\nfn f(.a: i64 = later, later: i64) {}",
                "itself or a later",
            ),
            ("fn f(a: i64, .a: i64 = 1) {}", "duplicate parameter"),
            (
                "fn f(...values: Any) {}\nfn main() { f(1, 2) }",
                "variadic parameter requires a List type annotation",
            ),
            (
                "fn f(.a: i64 = 1) -> i64 { a }\nfn main(g: fn(i64) -> i64) { g(a = 2) }",
                "statically known",
            ),
            (
                "fn f(.a: i64 = 1) -> i64 { a }\nfn main(g: fn(i64) -> i64) { g() }",
                "expects 1 argument",
            ),
        ] {
            let (_, diagnostics) = resolve(source);
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.level == Level::Error
                        && diagnostic.message.contains(expected)),
                "{source}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn literal_lambda_and_legacy_named_syntax_share_binding_plans() {
        for source in [
            "fn main() -> i64 { (|a: i64, .b: i64 = a + 1| b)(2) }",
            "fn f(.a: i64 = 1) -> i64 { a }\nfn main() -> i64 { f(.a = 42) }",
        ] {
            let resolved = checked(source);
            assert_eq!(resolved.call_arguments.len(), 1);
        }
    }

    #[test]
    fn recursive_selected_defaults_are_rejected_but_ordinary_recursion_is_valid() {
        for source in [
            "fn f(.x: i64 = f()) -> i64 { x }",
            "fn f(.x: i64 = g()) -> i64 { x }\nfn g(.x: i64 = f()) -> i64 { x }",
        ] {
            let (_, diagnostics) = resolve(source);
            assert!(
                diagnostics.iter().any(|diagnostic| diagnostic
                    .message
                    .contains("recursive default argument expansion")),
                "{diagnostics:?}"
            );
        }
        checked("fn f(.x: i64 = f(x = 42)) -> i64 { x }\nfn main() -> i64 { f() }");
        checked(
            "fn f(x: i64) -> i64 { if x > 0 { f(x - 1) } else { 42 } }\nfn main() -> i64 { f(2) }",
        );
    }

    #[test]
    fn repeated_default_edges_are_counted_before_exponential_lowering() {
        let mut source = "fn f0(.a: i64 = 1, .b: i64 = 1) -> i64 { a + b }\n".to_owned();
        for index in 1..20 {
            let previous = index - 1;
            source.push_str(&format!(
                "fn f{index}(.a: i64 = f{previous}(), .b: i64 = f{previous}()) -> i64 {{ a + b }}\n"
            ));
        }
        source.push_str("fn main() -> i64 { f19() }");
        let (_, diagnostics) = resolve(&source);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("compiler size limit")),
            "{diagnostics:?}"
        );
    }
    #[test]
    fn defaults_cannot_escape_the_caller_control_context() {
        for source in [
            "fn f(.x: i64 = if true { return 42 } else { 0 }) -> i64 { x }",
            "fn f(.x: i64 = if true { resume 42 } else { 0 }) -> i64 { x }",
            "fn f(.x: i64 = if true { break } else { 0 }) -> i64 { x }",
            "fn f(.x: i64 = if true { continue } else { 0 }) -> i64 { x }",
        ] {
            let (_, diagnostics) = resolve(source);
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains("default boundary")
                        || diagnostic.message.contains("loop inside that default")),
                "{source}: {diagnostics:?}"
            );
        }
        checked("fn f(.x: fn() -> i64 = || { return 42 }) -> i64 { x() }");
        checked("fn f(.x: i64 = if true { while true { break }; 42 } else { 0 }) -> i64 { x }");
    }
    #[test]
    fn effect_plans_compress_catch_slots_without_losing_named_source_indices() {
        let resolved = checked(
            "typealias Small=i8;effect ask(a:i64,catch k,.b:Small=2)->i64;fn main(){ask(b=2,a=40)#{ask(a,k,b)=>k(a+b)}}",
        );
        let (&call, plan) = resolved
            .call_arguments
            .iter()
            .find(|(_, plan)| resolved.ast.node(plan.declaration).kind == NodeKind::EffectDef)
            .unwrap();
        assert_eq!(plan.parameters.len(), 2);
        assert_eq!(
            plan.parameters[0].value,
            CallArgumentValue::Explicit { source_index: 1 }
        );
        assert_eq!(
            plan.parameters[1].value,
            CallArgumentValue::Explicit { source_index: 0 }
        );
        assert!(
            plan.parameters
                .iter()
                .all(|binding| resolved.ast.node(binding.parameter).kind != NodeKind::ParamCatch)
        );
        let operation = &resolved.effects[0].operations[0];
        assert_eq!(operation.continuation_param, Some(1));
        assert_eq!(operation.param_types.len(), plan.parameters.len());
        assert_eq!(
            resolved.type_pool.as_intrinsic(operation.param_types[1]),
            Some(Intrinsic::I8)
        );
        let explicit = argument_value_node(&resolved.ast, resolved.ast.multi_children(call)[0]);
        assert_eq!(
            resolved
                .type_pool
                .as_intrinsic(resolved.node_types[&explicit]),
            Some(Intrinsic::I8)
        );
        let default = resolved.ast.fixed_children(plan.parameters[1].parameter)[2];
        assert_eq!(
            resolved
                .type_pool
                .as_intrinsic(resolved.node_types[&default]),
            Some(Intrinsic::I8)
        );
    }

    #[test]
    fn effect_defaults_check_completed_later_headers_and_unavailable_catch_bindings() {
        checked(
            "effect first(.x:i64=second(40))->i64;effect second(x:i64)->i64;fn main(){first(x=42)#{first(x)=>x}}",
        );
        for (source, message) in [
            (
                "effect first(.x:i64=second(true))->i64;effect second(x:i64)->i64",
                "type mismatch",
            ),
            (
                "effect first(.x:i64=later())->i64;fn later()->bool{true}",
                "parameter default",
            ),
            (
                "effect ask(catch k,.x:Any=k)->i64",
                "runtime-provided catch",
            ),
            (
                "effect ask(.x:Any=k,catch k)->i64",
                "runtime-provided catch",
            ),
            (
                "effect ask(catch k,.x:fn()->Any=||k)->i64",
                "runtime-provided catch",
            ),
        ] {
            let (_, diagnostics) = resolve(source);
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.level == Level::Error
                        && diagnostic.message.contains(message)),
                "{source}: {diagnostics:?}"
            );
        }
    }
}
