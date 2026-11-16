//! Phase 3b: Type Resolution
//!
//! Walks the AST (post name-resolution) and infers types for expression nodes.
//! Literal types are assigned directly; operators propagate from their operands;
//! identifier types are read from the symbol table populated in Phase 3a.
//! Type annotations on function signatures and variable declarations are
//! resolved and checked against inferred types.

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::Level;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::SymbolKind;
use crate::resolver::Resolver;

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Walk `node_idx` and descendants, inferring types for expression nodes.
pub(crate) fn resolve_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    // Forward pass: resolve all function signatures so that call-site type
    // checking works regardless of definition order.
    if node_idx == ast.root {
        crate::structs::prepare_fields(r, ast);
        crate::enums::prepare(r, ast);
        prepare_module_annotations(r, ast);
        crate::effect::resolve_handler_pattern_types(r, ast, ast.root);
    }
    if !r.function_inference.headers_prepared {
        forward_resolve_signatures(r, ast, ast.root);
        r.function_inference.headers_prepared = true;
    }
    if node_idx == ast.root {
        crate::inference::resolve_static_dependencies(r, ast);
    }

    resolve_types_expected(r, ast, node_idx, None);
    if node_idx == ast.root {
        // Effect defaults consume finalized static function and module facts.
        crate::effect::resolve_defaults(r, ast);
    }
}

pub(crate) fn module_value_declarations(r: &Resolver, ast: &Ast) -> Vec<NodeIndex> {
    let mut declarations: Vec<_> = r
        .symbols
        .iter()
        .filter_map(|symbol| {
            let scope = &r.scopes[symbol.scope.0 as usize];
            if scope.node.is_null()
                || symbol.def_node.is_null()
                || !matches!(
                    ast.node(scope.node).kind,
                    NodeKind::FileScope
                        | NodeKind::ModuleDef
                        | NodeKind::StructDef
                        | NodeKind::EnumDef
                        | NodeKind::ImplDef
                        | NodeKind::ImplTraitDef
                        | NodeKind::ExtendDef
                        | NodeKind::ExtendTraitDef
                )
                || !matches!(
                    ast.node(symbol.def_node).kind,
                    NodeKind::ConstDecl
                        | NodeKind::LetDecl
                        | NodeKind::VarDecl
                        | NodeKind::LetHandler
                )
            {
                return None;
            }
            Some(symbol.def_node)
        })
        .collect();
    declarations.sort_unstable_by_key(|node| node.0);
    declarations.dedup();
    declarations
}

fn prepare_module_annotations(r: &mut Resolver, ast: &Ast) {
    for declaration in module_value_declarations(r, ast) {
        prepare_value_annotation(r, ast, declaration);
    }
}

pub(crate) fn prepare_value_annotation(r: &mut Resolver, ast: &Ast, declaration: NodeIndex) {
    let children = ast.fixed_children(declaration);
    let (pattern, ann_node) = if ast.node(declaration).kind == NodeKind::LetHandler {
        (children[1], children[2])
    } else {
        (children[0], children[1])
    };
    if let Some(annotation) = resolve_type_expr(r, ast, ann_node)
        && let Some(annotation) =
            crate::default_methods::parameter_body_type(r, ast, ann_node, annotation)
    {
        resolve_pattern_literals(r, ast, pattern, Some(annotation));
    }
}

/// Complete structural and forward aliases before checking function signatures.
/// Name resolution has already bound their targets; unresolved cycles and invalid
/// targets remain errors rather than being assigned a dynamic fallback.
pub(crate) fn prepare_type_aliases(r: &mut Resolver, ast: &Ast) {
    prepare_type_aliases_staged(r, ast, true);
}

pub(crate) fn prepare_type_aliases_staged(r: &mut Resolver, ast: &Ast, report_unresolved: bool) {
    let mut pending: Vec<_> = ast
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            if node.kind != NodeKind::Typealias {
                return None;
            }
            let node = NodeIndex(index as u32);
            let children = ast.fixed_children(node);
            let symbol = *r.node_symbols.get(&children[0])?;
            (r.symbols[symbol.0 as usize].type_index == TypeIndex::INVALID
                && r.symbols[symbol.0 as usize].kind == SymbolKind::Type)
                .then_some((symbol, children[1]))
        })
        .collect();
    loop {
        let before = pending.len();
        pending.retain(|&(symbol, target)| {
            if let Some(factory) = crate::type_factories::identity(r, target) {
                r.symbols[symbol.0 as usize].kind = SymbolKind::TypeFactory(factory);
                return false;
            }
            let Some(target) = resolve_type_expr_inner(r, ast, target) else {
                return true;
            };
            let name = r.symbols[symbol.0 as usize].name;
            let alias = r.register_type(TypeKind::Typealias { name, target });
            r.symbols[symbol.0 as usize].type_index = alias;
            false
        });
        if pending.len() == before {
            break;
        }
    }
    if report_unresolved {
        for (_, target) in pending {
            resolve_type_expr(r, ast, target);
        }
    }
}

// ---------------------------------------------------------------------------
// Forward signature resolution
// ---------------------------------------------------------------------------

/// Pre-scan the AST for all function definitions and resolve their parameter +
/// return types so that `Function` TypeKinds are registered on symbols before
/// any function body is type-checked. This eliminates definition-order
/// sensitivity for call-site type checking.
fn forward_resolve_signatures(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let mut pending = vec![node_idx];
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        if matches!(
            ast.node(node).kind,
            NodeKind::FunctionDef | NodeKind::TraitDefFn | NodeKind::TraitDeriveFn
        ) {
            if r.function_inference.headers.insert(node) {
                resolve_function_signature(r, ast, node);
            }
            // Nested declarations belong to their lexical body context. In
            // particular, trait replay supplies their concrete associated types.
            continue;
        }
        pending.extend(ast.fixed_children(node));
        pending.extend(ast.multi_children(node));
    }
}

/// Prepare direct local function headers after the enclosing parameters and
/// pattern bindings are known, before any local body or initializer is checked.
pub(crate) fn prepare_block_signatures(r: &mut Resolver, ast: &Ast, block: NodeIndex) {
    for &node in ast.multi_children(block) {
        if matches!(
            ast.node(node).kind,
            NodeKind::FunctionDef
                | NodeKind::TraitDeriveFn
                | NodeKind::ExtendDef
                | NodeKind::ExtendTraitDef
        ) {
            forward_resolve_signatures(r, ast, node);
        }
    }
}

/// Rebuild declaration headers owned by a freshly specialized body. The replay
/// boundary invalidates only these headers and restores the previous cache
/// afterwards; unrelated finalized functions are never prepared again.
pub(crate) fn prepare_replay_signatures(r: &mut Resolver, ast: &Ast, body: NodeIndex) {
    forward_resolve_signatures(r, ast, body);
}

/// Resolve only the signature (params + return type) of a function definition,
/// registering the Function type on the function's symbol. Does NOT recurse
/// into the function body.
fn resolve_function_signature(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] ret_type  [2] body  [3] capability

    // Resolve parameter types.
    for &param in ast.multi_children(node_idx) {
        resolve_param_annotation(r, ast, param);
        if param_type_index(r, ast, param).is_none()
            && let Some(pattern) = param_pattern_node(ast, param)
        {
            if ast.node(pattern).kind == NodeKind::PatternTuple {
                crate::tuples::pattern(r, ast, pattern, None, resolve_pattern_literals);
            } else if ast.node(pattern).kind == NodeKind::PatternCall {
                let _ = crate::newtype::resolve_pattern(
                    r,
                    ast,
                    pattern,
                    None,
                    true,
                    resolve_pattern_literals,
                );
            }
        }
        if param_type_index(r, ast, param).is_none()
            && let Some(pattern) = param_pattern_node(ast, param)
            && let Some(&symbol) = r.node_symbols.get(&pattern)
        {
            r.symbols[symbol.0 as usize].type_index = Intrinsic::Any.type_index();
        }
    }

    crate::arguments::validate_variadic_parameters(r, ast, node_idx, ast.multi_children(node_idx));

    // Resolve return type annotation.
    let ret_type = resolve_type_expr(r, ast, children[1]).unwrap_or_else(|| {
        if str_interner::get(ast.node(children[0]).str_id) == "__init__"
            || ast.node(node_idx).kind == NodeKind::TraitDefFn
        {
            Intrinsic::Unit.type_index()
        } else {
            Intrinsic::Any.type_index()
        }
    });

    // Build a Function type and assign to the symbol.
    let name_node = children[0];
    if let Some(&sym_id) = r.node_symbols.get(&name_node) {
        let param_types: Vec<TypeIndex> = ast
            .multi_children(node_idx)
            .iter()
            .map(|&p| param_type_index(r, ast, p).unwrap_or(Intrinsic::Any.type_index()))
            .collect();
        let func_ti = r.register_type(TypeKind::Function {
            params: param_types,
            ret: ret_type,
        });
        r.symbols[sym_id.0 as usize].type_index = func_ti;
    }
    crate::capability::resolve_signature_capability(r, ast, node_idx);
}

/// Walk `node_idx` with an optional expected type for bidirectional inference.
/// When `expected` is `Some`, numeric literals adopt the expected type instead
/// of defaulting to i64/f64.
pub(crate) fn resolve_types_expected(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) {
    if node_idx.is_null() {
        return;
    }
    let kind = ast.node(node_idx).kind;
    if let Some(ty) = crate::enums::value(r, ast, node_idx) {
        r.node_types.insert(node_idx, ty);
        record_coercion(r, node_idx, kind, ty, expected);
        return;
    }

    let inferred = match kind {
        // Alias and newtype targets were resolved before signatures; factories are compile-time bindings.
        NodeKind::Typealias | NodeKind::Newtype => None,
        // ── Literal types ──────────────────────────────────────────
        NodeKind::Int => Some(resolve_integer_literal(r, ast, node_idx, expected, false)),
        NodeKind::Real => Some(contextual_float_type(&r.type_pool, expected)),
        NodeKind::Str => Some(Intrinsic::Str.type_index()),
        NodeKind::FStringConcat => {
            // Interpolated values are converted with Display; Any is checked
            // when the value is converted.
            let display = r.type_pool.well_known.display;
            for &child in ast.multi_children(node_idx) {
                resolve_types(r, ast, child);
                if ast.node(child).kind != NodeKind::Str
                    && let Some(&ty) = r.node_types.get(&child)
                    && ty != TypeIndex::INVALID
                {
                    crate::trait_typing::is_subtype(r, ast, child, ty, display);
                }
            }
            Some(Intrinsic::Str.type_index())
        }
        NodeKind::Concat => crate::methods::concat(r, ast, node_idx),
        NodeKind::Char => Some(Intrinsic::Char.type_index()),
        NodeKind::Bool => Some(Intrinsic::Bool.type_index()),
        NodeKind::Null => Some(r.type_pool.null_type()),
        NodeKind::Unit => Some(Intrinsic::Unit.type_index()),

        // ── Arithmetic — infer a common numeric type ───────────────
        NodeKind::Add | NodeKind::Sub | NodeKind::Mul | NodeKind::Div | NodeKind::Mod => {
            let children = ast.fixed_children(node_idx);
            // The operator consumes raw payloads. Qualification is applied once
            // to its result at the enclosing value boundary.
            let operand_expected = payload_inner_expected(&r.type_pool, expected);
            resolve_types_expected(r, ast, children[0], operand_expected);
            resolve_types_expected(r, ast, children[1], operand_expected);
            match (
                r.node_types.get(&children[0]).copied(),
                r.node_types.get(&children[1]).copied(),
            ) {
                (Some(left), Some(right)) => {
                    if r.type_pool.as_intrinsic(left) == Some(Intrinsic::Any)
                        || r.type_pool.as_intrinsic(right) == Some(Intrinsic::Any)
                    {
                        Some(Intrinsic::Any.type_index())
                    } else if let Some(common) = r.type_pool.numeric_lift(left, right) {
                        Some(common)
                    } else {
                        r.diag_ctx.error(format!(
                            "arithmetic requires compatible numeric operands, found `{}` and `{}`",
                            type_display_name(&r.type_pool, left), type_display_name(&r.type_pool, right)
                        )).with_primary_span(ast.node(node_idx).span).emit(r.diag_ctx);
                        None
                    }
                }
                _ => None,
            }
        }

        // ── Boolean operators require boolean operands ─────────────
        NodeKind::BoolAnd | NodeKind::BoolOr | NodeKind::BoolNot => {
            for &child in ast.fixed_children(node_idx) {
                if child.is_null() {
                    continue;
                }
                resolve_types_expected(r, ast, child, Some(Intrinsic::Bool.type_index()));
                if let Some(&ti) = r.node_types.get(&child)
                    && !matches!(
                        r.type_pool.as_intrinsic(ti),
                        Some(Intrinsic::Bool | Intrinsic::Any)
                    )
                {
                    r.diag_ctx
                        .error(format!(
                            "boolean operator requires `bool`, found `{}`",
                            type_display_name(&r.type_pool, ti)
                        ))
                        .with_primary_span(ast.node(child).span)
                        .emit(r.diag_ctx);
                }
            }
            Some(Intrinsic::Bool.type_index())
        }
        // ── Comparison — always Bool ───────────────────────────────
        NodeKind::BoolMatches => {
            let children = ast.fixed_children(node_idx);
            resolve_types(r, ast, children[0]);
            let expected = r.node_types.get(&children[0]).copied();
            crate::enums::pattern(r, ast, children[1], expected);
            Some(Intrinsic::Bool.type_index())
        }
        NodeKind::BoolEq
        | NodeKind::BoolNotEq
        | NodeKind::BoolGt
        | NodeKind::BoolGtEq
        | NodeKind::BoolLt
        | NodeKind::BoolLtEq => {
            for &child in ast.fixed_children(node_idx) {
                resolve_types(r, ast, child);
            }
            if matches!(kind, NodeKind::BoolEq | NodeKind::BoolNotEq) {
                crate::equality::plan(r, ast, node_idx);
            }
            Some(Intrinsic::Bool.type_index())
        }

        // ── Unary negation — propagate operand type ────────────────
        NodeKind::Negative => {
            let children = ast.fixed_children(node_idx);
            if ast.node(children[0]).kind == NodeKind::Int {
                let ti = resolve_integer_literal(r, ast, children[0], expected, true);
                r.node_types.insert(children[0], ti);
                Some(ti)
            } else {
                let operand_expected = payload_inner_expected(&r.type_pool, expected);
                resolve_types_expected(r, ast, children[0], operand_expected);
                let ty = r.node_types.get(&children[0]).copied();
                match ty.and_then(|ty| r.type_pool.as_intrinsic(ty)) {
                    Some(Intrinsic::Any) => ty,
                    Some(kind) if kind.is_numeric() => {
                        if matches!(
                            kind,
                            Intrinsic::U8
                                | Intrinsic::U16
                                | Intrinsic::U32
                                | Intrinsic::U64
                                | Intrinsic::U128
                                | Intrinsic::Usize
                        ) {
                            let signed = r
                                .type_pool
                                .numeric_lift(kind.type_index(), Intrinsic::I8.type_index());
                            if signed.is_none() {
                                r.diag_ctx.error("negation requires a signed type capable of representing the operand range".into())
                                    .with_primary_span(ast.node(node_idx).span).emit(r.diag_ctx);
                            }
                            signed
                        } else {
                            ty
                        }
                    }
                    Some(_) => {
                        r.diag_ctx
                            .error("negation requires a numeric operand".into())
                            .with_primary_span(ast.node(children[0]).span)
                            .emit(r.diag_ctx);
                        None
                    }
                    None => None,
                }
            }
        }

        // Explicit casts preserve the source type; the VM checks whether the
        // source value can be represented by the requested destination type.
        NodeKind::TypeCast => {
            let children = ast.fixed_children(node_idx);
            resolve_types(r, ast, children[0]);
            let target = resolve_type_expr(r, ast, children[1]);
            if let Some(target) = target {
                r.node_types.insert(children[1], target);
                crate::newtype::validate_cast(r, ast, node_idx, children[0], target);
            } else {
                let span = if children[1].is_null() {
                    ast.node(node_idx).span
                } else {
                    ast.node(children[1]).span
                };
                r.diag_ctx
                    .error("cast target must be a valid type".into())
                    .with_primary_span(span)
                    .emit(r.diag_ctx);
            }
            target
        }

        // ── Identifier — read type from symbol table ───────────────
        NodeKind::Id | NodeKind::SelfLower | NodeKind::SelfUpper => {
            if crate::type_factories::bare_value(r, ast, node_idx) {
                return;
            }
            if let Some(ty) = crate::type_factories::bare_type(r, node_idx) {
                r.node_type_values.insert(node_idx, ty);
                let ti = Intrinsic::Type.type_index();
                r.node_types.insert(node_idx, ti);
                record_coercion(r, node_idx, kind, ti, expected);
                return;
            }
            if let Some(&sym_id) = r.node_symbols.get(&node_idx) {
                let symbol = &r.symbols[sym_id.0 as usize];
                let ti = symbol.type_index;
                if ti != TypeIndex::INVALID {
                    if matches!(
                        symbol.kind,
                        SymbolKind::Type | SymbolKind::Trait | SymbolKind::Effect
                    ) {
                        r.node_type_values.insert(node_idx, ti);
                        Some(Intrinsic::Type.type_index())
                    } else {
                        Some(ti)
                    }
                } else if matches!(symbol.kind, SymbolKind::BuiltinFunction(_)) {
                    expected.filter(|&ty| {
                        r.type_pool.canonical_type(ty).is_some_and(|ty| {
                            matches!(r.type_pool.get(ty).kind, TypeKind::Function { .. })
                        })
                    })
                } else {
                    None
                }
            } else {
                None
            }
        }

        NodeKind::OptionalType
        | NodeKind::FnType
        | NodeKind::Arrow
        | NodeKind::EffectType
        | NodeKind::AsyncEffectType
        | NodeKind::EffectQualifiedType
        | NodeKind::ErrorQualifiedType => {
            let represented = resolve_type_expr(r, ast, node_idx);
            if let Some(ty) = represented {
                r.node_type_values.insert(node_idx, ty);
            }
            represented.map(|_| Intrinsic::Type.type_index())
        }
        NodeKind::View => {
            if crate::type_factories::bare_value(r, ast, node_idx) {
                return;
            }
            if let Some(ty) = crate::type_factories::bare_type(r, node_idx) {
                r.node_type_values.insert(node_idx, ty);
                let ti = Intrinsic::Type.type_index();
                r.node_types.insert(node_idx, ti);
                record_coercion(r, node_idx, kind, ti, expected);
                return;
            }
            let children = ast.fixed_children(node_idx);
            if str_interner::get(ast.node(children[1]).str_id) == "type" {
                resolve_types(r, ast, children[0]);
                Some(Intrinsic::Type.type_index())
            } else if let Some(ty) = resolve_type_expr_inner(r, ast, node_idx) {
                r.node_type_values.insert(node_idx, ty);
                Some(Intrinsic::Type.type_index())
            } else if r
                .node_symbols
                .get(&node_idx)
                .is_some_and(|symbol| r.builtin_fns.contains_key(symbol))
            {
                expected.filter(|&ty| {
                    r.type_pool.canonical_type(ty).is_some_and(|ty| {
                        matches!(r.type_pool.get(ty).kind, TypeKind::Function { .. })
                    })
                })
            } else {
                None
            }
        }

        // ── Let / Const / Var — propagate and check types ──────────
        NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl => {
            resolve_decl(r, ast, node_idx);
            None
        }

        NodeKind::Assign => {
            let children = ast.fixed_children(node_idx);
            if ast.node(children[0]).kind == NodeKind::Call {
                crate::applications::update(r, ast, node_idx);
                r.node_types.insert(node_idx, Intrinsic::Unit.type_index());
                return;
            }
            resolve_types(r, ast, children[0]);
            crate::assignments::check_place(r, ast, children[0], "assignment");
            let target = r.node_types.get(&children[0]).copied();
            resolve_types_expected(r, ast, children[1], target);
            if let Some(target) = target {
                check_expected_type(r, ast, children[1], target, "assignment");
            }
            Some(Intrinsic::Unit.type_index())
        }

        NodeKind::AddAssign
        | NodeKind::SubAssign
        | NodeKind::MulAssign
        | NodeKind::DivAssign
        | NodeKind::ModAssign => {
            let children = ast.fixed_children(node_idx);
            let target = if ast.node(children[0]).kind == NodeKind::Call {
                crate::assignments::compound_index(r, ast, children[0])
            } else {
                resolve_types(r, ast, children[0]);
                crate::assignments::check_place(r, ast, children[0], "compound assignment");
                r.node_types.get(&children[0]).copied()
            };
            resolve_types_expected(r, ast, children[1], target);
            if let Some(target) = target {
                check_expected_type(r, ast, children[1], target, "compound assignment");
                if r.type_pool.as_intrinsic(target) != Some(Intrinsic::Any)
                    && !r
                        .type_pool
                        .as_intrinsic(target)
                        .is_some_and(type_pool::Intrinsic::is_numeric)
                {
                    r.diag_ctx
                        .error("compound assignment requires numeric operands".into())
                        .with_primary_span(ast.node(node_idx).span)
                        .emit(r.diag_ctx);
                }
            }
            Some(Intrinsic::Unit.type_index())
        }

        NodeKind::BreakStatement | NodeKind::ContinueStatement => {
            let guard = ast.fixed_children(node_idx)[1];
            if !guard.is_null() {
                let boolean = Intrinsic::Bool.type_index();
                resolve_types_expected(r, ast, guard, Some(boolean));
                check_expected_type(r, ast, guard, boolean, "loop control guard");
            }
            Some(Intrinsic::Unit.type_index())
        }

        NodeKind::ForLoop => {
            crate::trait_loops::type_loop(r, ast, node_idx);
            Some(Intrinsic::Unit.type_index())
        }

        NodeKind::WhenStatement => resolve_when_types(r, ast, node_idx, expected),

        NodeKind::IfStatement | NodeKind::WhileLoop => {
            let children = ast.fixed_children(node_idx);
            let condition = if kind == NodeKind::IfStatement {
                children[0]
            } else {
                children[1]
            };
            let bool_type = Intrinsic::Bool.type_index();
            resolve_types_expected(r, ast, condition, Some(bool_type));
            check_expected_type(r, ast, condition, bool_type, "condition");
            if kind == NodeKind::IfStatement {
                resolve_types_expected(r, ast, children[1], expected);
                resolve_types_expected(r, ast, children[2], expected);
                if let Some(expected) = expected {
                    for &branch in &children[1..3] {
                        if !branch.is_null() {
                            check_expected_type(r, ast, branch, expected, "conditional branch");
                        }
                    }
                    if children[2].is_null() {
                        check_expected_type(
                            r,
                            ast,
                            children[2],
                            expected,
                            "conditional branch without else",
                        );
                    }
                }
                let types = [
                    r.node_types
                        .get(&children[1])
                        .copied()
                        .unwrap_or(Intrinsic::Unit.type_index()),
                    r.node_types
                        .get(&children[2])
                        .copied()
                        .unwrap_or(Intrinsic::Unit.type_index()),
                ];
                Some(join_result_types(r, types))
            } else {
                resolve_types(r, ast, children[2]);
                Some(Intrinsic::Unit.type_index())
            }
        }

        // ── Block — type is the type of the last expression ────────
        NodeKind::Block => {
            crate::inference::resolve_block_dependencies(r, ast, node_idx);
            let mc = ast.multi_children(node_idx);
            let last_idx = mc.len().saturating_sub(1);
            let mut block_handled_effects: Vec<TypeIndex> = Vec::new();
            let mut capturing_answers: Vec<TypeIndex> = Vec::new();
            for (i, &child) in mc.iter().enumerate() {
                if i == last_idx {
                    resolve_types_expected(r, ast, child, expected);
                } else {
                    let child_expected =
                        if !child.is_null() && ast.node(child).kind == NodeKind::HandlesStatement {
                            expected
                        } else {
                            None
                        };
                    resolve_types_expected(r, ast, child, child_expected);
                }
                if !child.is_null() {
                    if let Some(plan) = r.handles_statements.get(&child) {
                        block_handled_effects.push(plan.effect_type);
                        if plan.continuation_param.is_some()
                            && r.type_pool.as_intrinsic(plan.answer_type)
                                != Some(Intrinsic::NoReturn)
                        {
                            capturing_answers.push(plan.answer_type);
                        }
                    } else if let Some(plan) = r.let_handlers.get(&child) {
                        for h in &plan.handlers {
                            let (eff_ty, cont_p, ret_ty) = match h {
                                crate::HandlerBindingKind::Callable {
                                    effect_type,
                                    continuation_param,
                                } => (*effect_type, *continuation_param, None),
                                crate::HandlerBindingKind::CapabilityMethod {
                                    effect_type,
                                    continuation_param,
                                    return_type,
                                    ..
                                } => (*effect_type, *continuation_param, Some(*return_type)),
                            };
                            block_handled_effects.push(eff_ty);
                            if cont_p.is_some()
                                && let Some(rt) = ret_ty
                                && !matches!(
                                    r.type_pool.as_intrinsic(rt),
                                    Some(Intrinsic::Any | Intrinsic::NoReturn)
                                )
                            {
                                capturing_answers.push(crate::capability::strip_effect_qualified(
                                    &r.type_pool,
                                    rt,
                                ));
                            }
                        }
                    }
                }
            }
            let base_ty = mc
                .last()
                .and_then(|&last| r.node_types.get(&last).copied())
                .map(|ty| {
                    if block_handled_effects.is_empty() {
                        ty
                    } else {
                        crate::capability::subtract_effects(r, ty, &block_handled_effects)
                    }
                });
            if capturing_answers.is_empty() {
                base_ty
            } else {
                let mut combined = base_ty.unwrap_or(Intrinsic::Unit.type_index());
                for ans in capturing_answers {
                    if ans != combined {
                        combined = crate::errors::join(r, combined, ans);
                    }
                }
                Some(combined)
            }
        }

        NodeKind::HandlerApplication => {
            crate::capability::resolve_handler_application(r, ast, node_idx, expected)
        }
        NodeKind::HandlesStatement => {
            crate::capability::resolve_handles_statement(r, ast, node_idx, expected)
        }
        NodeKind::LetHandler => crate::capability::resolve_let_handler(r, ast, node_idx),

        // ── Function definition — resolve signatures and check body ─
        NodeKind::FunctionDef | NodeKind::TraitDeriveFn => {
            resolve_function_def_types(r, ast, node_idx);
            None
        }

        // ── Call — infer return type from callee ───────────────────
        NodeKind::Call => resolve_call_types(r, ast, node_idx, expected),
        NodeKind::InfixFnCall => resolve_infix_call_types(r, ast, node_idx),

        // ── PostMatch — type is the type of the first arm body ─────
        NodeKind::PostMatch => resolve_match_types(r, ast, node_idx, expected),

        // ── Projection (a.b) — infer from enum variant or field ────
        NodeKind::Projection => resolve_projection_types(r, ast, node_idx),

        // ── Tuple — build a Tuple type from element types ──────────
        NodeKind::Tuple => crate::tuples::literal(
            r,
            ast,
            node_idx,
            payload_inner_expected(&r.type_pool, expected),
        ),
        NodeKind::ListOf => crate::lists::literal(r, ast, node_idx),
        NodeKind::RangeFull
        | NodeKind::RangeFrom
        | NodeKind::RangeTo
        | NodeKind::RangeToInclusive
        | NodeKind::RangeFromTo
        | NodeKind::RangeFromToInclusive => crate::ranges::literal(r, ast, node_idx),
        NodeKind::Object => {
            for &item in ast.multi_children(node_idx) {
                if ast.node(item).kind == NodeKind::Property {
                    resolve_types(r, ast, ast.fixed_children(item)[1]);
                } else {
                    resolve_types(r, ast, item);
                }
            }
            Some(Intrinsic::Any.type_index())
        }
        NodeKind::Symbol => Some(Intrinsic::Any.type_index()),
        NodeKind::Await => {
            let inner = ast.fixed_children(node_idx)[0];
            let expected_future = expected.map(|exp| r.type_pool.intern_future(exp));
            resolve_types_expected(r, ast, inner, expected_future);
            let Some(&inner_ty) = r.node_types.get(&inner) else {
                return;
            };
            let Some(canon) = r.type_pool.canonical_type(inner_ty) else {
                return;
            };
            match r.type_pool.get(canon).kind {
                TypeKind::Future { inner: elem } => Some(elem),
                TypeKind::Intrinsic(Intrinsic::Any) => Some(Intrinsic::Any.type_index()),
                _ => {
                    let found = type_display_name(&r.type_pool, canon);
                    r.diag_ctx
                        .error(format!("`.await` requires a `Future`, found `{found}`"))
                        .with_primary_span(ast.node(node_idx).span)
                        .emit(r.diag_ctx);
                    None
                }
            }
        }
        NodeKind::ResetExpr => {
            let children = ast.fixed_children(node_idx);
            resolve_types(r, ast, children[0]);
            resolve_types(r, ast, children[1]);
            Some(Intrinsic::Any.type_index())
        }
        NodeKind::ShiftExpr => {
            let children = ast.fixed_children(node_idx);
            resolve_types(r, ast, children[0]);
            if !children[1].is_null()
                && let Some(&sym_id) = r.node_symbols.get(&children[1])
            {
                r.symbols[sym_id.0 as usize].type_index = Intrinsic::Any.type_index();
            }
            resolve_types(r, ast, children[2]);
            Some(Intrinsic::Any.type_index())
        }

        // ── Error construction — produces ErrorQualified type ──────
        NodeKind::ErrorConstruction => crate::errors::construction(r, ast, node_idx, expected),

        // ── Error/effect/option propagation — passthrough ──────────
        NodeKind::ExprStatement => {
            let child = ast.fixed_children(node_idx)[0];
            resolve_types_expected(r, ast, child, expected);
            r.node_types.get(&child).copied()
        }
        NodeKind::OptionPropagation => crate::optional::propagate(r, ast, node_idx),
        NodeKind::ErrorElimination => {
            crate::error_patterns::elimination(r, ast, node_idx, expected)
        }
        NodeKind::ErrorPropagation => crate::errors::propagate(r, ast, node_idx),
        NodeKind::ReturnStatement | NodeKind::ResumeStatement => {
            let children = ast.fixed_children(node_idx);
            if !children[1].is_null() {
                resolve_types_expected(r, ast, children[1], Some(Intrinsic::Bool.type_index()));
                if let Some(&guard_type) = r.node_types.get(&children[1])
                    && !matches!(
                        r.type_pool.as_intrinsic(guard_type),
                        Some(Intrinsic::Bool | Intrinsic::Any)
                    )
                {
                    r.diag_ctx
                        .error("return/resume guard must have type `bool`".into())
                        .with_primary_span(ast.node(children[1]).span)
                        .emit(r.diag_ctx);
                }
            }
            let return_type = r.return_types.last().copied().flatten().or(expected);
            resolve_types_expected(r, ast, children[0], return_type);
            if let Some(return_type) = return_type {
                check_expected_type(r, ast, children[0], return_type, "return value");
            }
            if matches!(kind, NodeKind::ReturnStatement | NodeKind::ResumeStatement) {
                let actual = r
                    .node_types
                    .get(&children[0])
                    .copied()
                    .unwrap_or(Intrinsic::Unit.type_index());
                if let Some(returns) = r.inferred_returns.last_mut() {
                    returns.push(actual);
                }
            }
            Some(
                r.node_types
                    .get(&children[0])
                    .copied()
                    .unwrap_or(Intrinsic::Unit.type_index()),
            )
        }
        NodeKind::EffectPropagation | NodeKind::EffectElimination => {
            let inner = ast.fixed_children(node_idx)[0];
            resolve_types(r, ast, inner);
            let mut answer_type = None;
            let mut handled_effects = Vec::new();
            for &arm in ast.multi_children(node_idx) {
                let children = ast.fixed_children(arm);
                let (pattern, _) = crate::effect::unwrap_handler_pattern(ast, children[0]);
                if pattern.is_null() || ast.node(pattern).kind != NodeKind::PatternCall {
                    continue;
                }
                let callee = ast.fixed_children(pattern)[0];
                if let Some(&symbol) = r.node_symbols.get(&callee) {
                    let eff_ty = r.symbols[symbol.0 as usize].type_index;
                    if eff_ty != TypeIndex::INVALID {
                        handled_effects.push(eff_ty);
                    }
                }
                let operation = r.node_symbols.get(&callee).and_then(|&symbol| {
                    let effect_type = r.symbols[symbol.0 as usize].type_index;
                    r.effects
                        .iter()
                        .find(|effect| effect.type_index == effect_type)
                        .and_then(|effect| effect.operations.first())
                        .cloned()
                });
                let captures = operation
                    .as_ref()
                    .is_some_and(|operation| operation.continuation_param.is_some());
                let expected_return = if captures {
                    expected
                } else {
                    operation.as_ref().map(|operation| operation.return_type)
                };
                // Each handler is lowered to its own closure. Its exits must
                // neither inherit nor contribute to the enclosing function.
                r.return_types.push(expected_return);
                r.inferred_returns.push(Vec::new());
                resolve_types_expected(r, ast, children[1], expected_return);
                r.return_types.pop();
                let returns = r
                    .inferred_returns
                    .pop()
                    .expect("handler return context was pushed");
                let inferred = infer_return_type(r, ast, children[1], returns);
                if expected_return.is_none() {
                    coerce_inferred_returns(r, ast, children[1], inferred);
                }
                if let (Some(expected), Some(&actual)) =
                    (expected_return, r.node_types.get(&children[1]))
                    && !definitely_returns(ast, children[1])
                    && !crate::trait_typing::is_subtype(r, ast, children[1], actual, expected)
                    && !r.type_pool.is_gradually_consistent(actual, expected)
                {
                    r.diag_ctx
                        .error(format!(
                            "handler result type mismatch: expected `{}`, found `{}`",
                            type_display_name(&r.type_pool, expected),
                            type_display_name(&r.type_pool, actual)
                        ))
                        .with_primary_span(ast.node(children[1]).span)
                        .emit(r.diag_ctx);
                }
                let handler_result = expected_return.unwrap_or(inferred);
                crate::errors::finish_callable(r, arm, handler_result);
                r.node_types.insert(children[1], handler_result);
                if captures && let Some(&ty) = r.node_types.get(&children[1]) {
                    // Capturing handlers can replace the computation's result
                    // (including returning the continuation for later use).
                    answer_type = Some(match answer_type {
                        Some(previous) if previous != ty => crate::errors::join(r, previous, ty),
                        _ => ty,
                    });
                }
            }
            let residual_effects: Vec<TypeIndex> = r
                .node_types
                .get(&inner)
                .and_then(|&ty| r.type_pool.canonical_type(ty))
                .and_then(|canon| match &r.type_pool.get(canon).kind {
                    TypeKind::EffectQualified { effects, .. } => Some(
                        effects
                            .iter()
                            .copied()
                            .filter(|e| {
                                let ec = r.type_pool.canonical_type(*e).unwrap_or(*e);
                                !handled_effects
                                    .iter()
                                    .any(|h| r.type_pool.canonical_type(*h).unwrap_or(*h) == ec)
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            let raw_result = answer_type.or_else(|| {
                r.node_types
                    .get(&inner)
                    .copied()
                    .map(|ty| match r.type_pool.get(ty).kind {
                        TypeKind::EffectQualified { inner, .. } => inner,
                        _ => ty,
                    })
            });
            if kind == NodeKind::EffectElimination && !residual_effects.is_empty() {
                raw_result.map(|res| crate::effect::normalize(r, residual_effects, res))
            } else {
                raw_result
            }
        }

        // ── Lambda & case closure — resolve params, body, build Function type
        NodeKind::Lambda => resolve_lambda_types(
            r,
            ast,
            node_idx,
            payload_inner_expected(&r.type_pool, expected),
        ),
        NodeKind::CaseMap | NodeKind::CaseAlternative => resolve_case_closure_types(
            r,
            ast,
            node_idx,
            payload_inner_expected(&r.type_pool, expected),
        ),
        NodeKind::DeferStatement => {
            resolve_types(r, ast, ast.fixed_children(node_idx)[0]);
            Some(Intrinsic::Unit.type_index())
        }

        // ── Named argument — resolve value only ────────────────────
        NodeKind::NamedArg | NodeKind::PropertyAssignment => {
            let children = ast.fixed_children(node_idx);
            // children[0] = parameter name (Id) — skip
            // children[1] = value expression — resolve with expected type
            resolve_types_expected(r, ast, children[1], expected);
            r.node_types.get(&children[1]).copied()
        }
        NodeKind::SpreadArg => {
            let children = ast.fixed_children(node_idx);
            resolve_types(r, ast, children[0]);
            r.diag_ctx
                .error("spread `...expr` is only valid in call argument position".into())
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
            None
        }

        // ── Struct construction: TypeName { field: val, ... } ──────
        NodeKind::ExtendedCall => crate::extended::resolve(r, ast, node_idx),
        // Effect headers are prepared before bodies; defaults wait until the
        // root traversal has inferred later function results.
        NodeKind::EffectDef | NodeKind::AsyncEffectDef => None,
        NodeKind::EnumDef => {
            for &member in ast.multi_children(node_idx) {
                let member = crate::structs::unwrap_member(ast, member);
                if ast.node(member).kind == NodeKind::EnumVariant {
                    for &parameter in ast.multi_children(member) {
                        resolve_param_types(r, ast, parameter);
                    }
                } else {
                    resolve_types(r, ast, member);
                }
            }
            None
        }
        NodeKind::StructDef => {
            crate::structs::resolve_defaults(r, ast, node_idx);
            for &member in ast.multi_children(node_idx) {
                if ast.node(crate::structs::unwrap_member(ast, member)).kind
                    != NodeKind::StructField
                {
                    resolve_types(r, ast, member);
                }
            }
            None
        }

        // ── Default: recurse ───────────────────────────────────────
        _ => {
            for &child in ast.fixed_children(node_idx) {
                resolve_types(r, ast, child);
            }
            for &child in ast.multi_children(node_idx) {
                resolve_types(r, ast, child);
            }
            None
        }
    };

    if let Some(ti) = inferred {
        r.node_types.insert(node_idx, ti);
        record_coercion(r, node_idx, kind, ti, expected);
    }
}

pub(crate) fn check_expected_type(
    r: &mut Resolver,
    ast: &Ast,
    value: NodeIndex,
    expected: TypeIndex,
    context: &str,
) {
    let actual = if value.is_null() {
        Some(Intrinsic::Unit.type_index())
    } else {
        r.node_types.get(&value).copied()
    };
    if let Some(actual) = actual
        && !crate::trait_typing::is_subtype(r, ast, value, actual, expected)
        && !r.type_pool.is_gradually_consistent(actual, expected)
    {
        let mut diagnostic = r.diag_ctx.error(format!(
            "type mismatch in {context}: expected `{}`, found `{}`",
            type_display_name(&r.type_pool, expected),
            type_display_name(&r.type_pool, actual)
        ));
        if !value.is_null() {
            diagnostic = diagnostic.with_primary_span(ast.node(value).span);
        }
        diagnostic.emit(r.diag_ctx);
    }
}

fn record_coercion(
    r: &mut Resolver,
    node: NodeIndex,
    kind: NodeKind,
    actual: TypeIndex,
    expected: Option<TypeIndex>,
) {
    // These AST wrappers are bypassed by statement lowering. Record boundaries
    // on their actual value children, once, rather than on each wrapper layer.
    if matches!(
        kind,
        NodeKind::Block
            | NodeKind::ExprStatement
            | NodeKind::ReturnStatement
            | NodeKind::ResumeStatement
            | NodeKind::NamedArg
            | NodeKind::PropertyAssignment
    ) {
        return;
    }
    let target = expected.or_else(|| {
        matches!(
            kind,
            NodeKind::Int
                | NodeKind::Real
                | NodeKind::Negative
                | NodeKind::Add
                | NodeKind::Sub
                | NodeKind::Mul
                | NodeKind::Div
                | NodeKind::Mod
        )
        .then_some(actual)
    });
    let Some(target) = target else { return };
    if crate::errors::record_conversion(r, node, actual, target) {
        return;
    }
    let target = if r
        .type_pool
        .as_intrinsic(actual)
        .is_some_and(Intrinsic::is_numeric)
        && let Some(optional) = r.type_pool.canonical_type(target)
        && let TypeKind::Optional { inner } = r.type_pool.get(optional).kind
        && r.type_pool
            .as_intrinsic(inner)
            .is_some_and(Intrinsic::is_numeric)
    {
        inner
    } else {
        target
    };
    if kind == NodeKind::TypeCast && actual == target {
        // The explicit cast already establishes this exact representation.
        return;
    }
    let target_kind = r.type_pool.as_intrinsic(target);
    let coercion = if r.type_pool.as_intrinsic(actual) == Some(Intrinsic::Any)
        && target_kind != Some(Intrinsic::Any)
    {
        Some(crate::CoercionKind::Assert)
    } else if optional_numeric_widening(&r.type_pool, actual, target)
        || (r
            .type_pool
            .as_intrinsic(actual)
            .is_some_and(Intrinsic::is_numeric)
            && target_kind.is_some_and(Intrinsic::is_numeric)
            && r.type_pool.is_subtype(actual, target)
            && (actual != target
                || matches!(
                    target_kind,
                    Some(
                        Intrinsic::I8
                            | Intrinsic::I16
                            | Intrinsic::I32
                            | Intrinsic::Isize
                            | Intrinsic::U8
                            | Intrinsic::U16
                            | Intrinsic::U32
                            | Intrinsic::Usize
                            | Intrinsic::F32
                    )
                )))
    {
        Some(crate::CoercionKind::Convert)
    } else {
        None
    };
    if let Some(kind) = coercion {
        r.node_coercions
            .insert(node, crate::Coercion { target, kind });
    }
}

fn optional_numeric_widening(
    pool: &type_pool::TypePool,
    actual: TypeIndex,
    target: TypeIndex,
) -> bool {
    let (Some(actual), Some(target)) = (pool.canonical_type(actual), pool.canonical_type(target))
    else {
        return false;
    };
    let (TypeKind::Optional { inner: actual }, TypeKind::Optional { inner: target }) =
        (&pool.get(actual).kind, &pool.get(target).kind)
    else {
        return false;
    };
    pool.as_intrinsic(*actual)
        .is_some_and(Intrinsic::is_numeric)
        && pool
            .as_intrinsic(*target)
            .is_some_and(Intrinsic::is_numeric)
        && pool.canonical_type(*actual) != pool.canonical_type(*target)
        && pool.is_subtype(*actual, *target)
}

// ---------------------------------------------------------------------------
// Type annotation resolution
// ---------------------------------------------------------------------------

/// Resolve a present type expression, reporting invalid annotations instead of
/// silently treating them as inferred or dynamically typed declarations.
pub(crate) fn resolve_type_expr(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
) -> Option<TypeIndex> {
    if node_idx.is_null() {
        return None;
    }
    let resolved = resolve_type_expr_inner(r, ast, node_idx);
    if resolved.is_none() {
        r.diag_ctx
            .error("expected a valid type expression".into())
            .with_primary_span(ast.node(node_idx).span)
            .emit(r.diag_ctx);
    }
    resolved
}

/// Resolve a type-expression AST node to a TypeIndex.
/// Handles: Id (named types), OptionalType, FnType, etc.
pub(crate) fn resolve_type_expr_inner(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
) -> Option<TypeIndex> {
    let ty = resolve_type_expr_raw(r, ast, node_idx)?;
    // Signature provenance needs the actual composite shape before replay specializes it.
    r.node_type_values.insert(node_idx, ty);
    let ty = crate::default_methods::specialize_body_annotation(r, ast, node_idx, ty)?;
    r.node_type_values.insert(node_idx, ty);
    Some(ty)
}
fn resolve_type_expr_raw(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) -> Option<TypeIndex> {
    if node_idx.is_null() {
        return None;
    }
    let kind = ast.node(node_idx).kind;
    match kind {
        // Named type: look up the identifier's symbol, which should be a Type or Trait.
        NodeKind::Id | NodeKind::SelfUpper | NodeKind::Projection => {
            if let Some(ty) = crate::type_factories::bare_type(r, node_idx) {
                return Some(ty);
            }
            if let Some(&sym_id) = r.node_symbols.get(&node_idx) {
                let sym = &r.symbols[sym_id.0 as usize];
                if matches!(
                    sym.kind,
                    SymbolKind::Type | SymbolKind::Trait | SymbolKind::Effect
                ) {
                    let ti = sym.type_index;
                    if ti != TypeIndex::INVALID {
                        return Some(ti);
                    }
                }
            }
            None
        }
        // `.Name'builtin` type view
        NodeKind::View => {
            if let Some(ty) = crate::type_factories::bare_type(r, node_idx) {
                return Some(ty);
            }
            if let Some(&sym_id) = r.node_symbols.get(&node_idx) {
                let sym = &r.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Type && sym.type_index != TypeIndex::INVALID {
                    return Some(sym.type_index);
                }
            }
            None
        }
        NodeKind::OptionalType | NodeKind::PatternOptionSome => {
            let children = ast.fixed_children(node_idx);
            resolve_type_expr_inner(r, ast, children[0])
                .map(|inner| r.register_type(TypeKind::Optional { inner }))
        }
        // Function and effect signatures, optionally followed by a return type.
        NodeKind::FnType | NodeKind::EffectType | NodeKind::AsyncEffectType | NodeKind::Arrow => {
            let (signature, ret) = if kind == NodeKind::Arrow {
                let children = ast.fixed_children(node_idx);
                (children[0], resolve_type_expr_inner(r, ast, children[1]))
            } else {
                (node_idx, Some(Intrinsic::Unit.type_index()))
            };
            let signature_kind = ast.node(signature).kind;
            if !matches!(
                signature_kind,
                NodeKind::FnType | NodeKind::EffectType | NodeKind::AsyncEffectType
            ) {
                return None;
            }
            let params = ast
                .multi_children(signature)
                .iter()
                .map(|&node| resolve_type_expr_inner(r, ast, node))
                .collect::<Option<Vec<_>>>()?;
            let ret = ret?;
            Some(
                r.type_pool
                    .intern_structural(if signature_kind == NodeKind::FnType {
                        TypeKind::Function { params, ret }
                    } else {
                        TypeKind::Effect {
                            params,
                            ret,
                            is_async: signature_kind == NodeKind::AsyncEffectType,
                        }
                    }),
            )
        }
        NodeKind::Tuple | NodeKind::PatternTuple => {
            let elems: Vec<TypeIndex> = ast
                .multi_children(node_idx)
                .iter()
                .map(|&child| resolve_type_expr_inner(r, ast, child))
                .collect::<Option<Vec<_>>>()?;
            if elems.is_empty() {
                None
            } else {
                Some(r.register_type(TypeKind::Tuple { elements: elems }))
            }
        }
        // Error qualified type: !ErrType InnerType  — children: [0] err_set [1] inner
        NodeKind::ErrorQualifiedType => {
            let children = ast.fixed_children(node_idx);
            let errors = crate::errors::set(r, ast, children[0])?;
            let inner = resolve_type_expr_inner(r, ast, children[1])?;
            crate::errors::normalize(r, ast, node_idx, errors, inner)
        }
        // Effect qualified type: #EffSet InnerType — children: [0] eff_set [1] inner
        NodeKind::EffectQualifiedType => {
            let children = ast.fixed_children(node_idx);
            if children.len() >= 2 {
                let effects = crate::effect::set(r, ast, children[0])?;
                let inner = resolve_type_expr_inner(r, ast, children[1])?;
                Some(crate::effect::normalize(r, effects, inner))
            } else {
                resolve_type_expr_inner(r, ast, children[0])
            }
        }
        // Unit type literal
        NodeKind::Unit => Some(Intrinsic::Unit.type_index()),
        NodeKind::Call | NodeKind::PatternCall => {
            crate::type_factories::application(r, ast, node_idx)?.ok()
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Function definition type resolution
// ---------------------------------------------------------------------------

fn join_result_types(r: &mut Resolver, types: impl IntoIterator<Item = TypeIndex>) -> TypeIndex {
    types
        .into_iter()
        .reduce(|left, right| crate::errors::join(r, left, right))
        .unwrap_or(Intrinsic::Unit.type_index())
}

fn definitely_returns(ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() {
        return false;
    }
    match ast.node(node).kind {
        NodeKind::ReturnStatement | NodeKind::ResumeStatement => {
            ast.fixed_children(node)[1].is_null()
        }
        NodeKind::Block => ast
            .multi_children(node)
            .iter()
            .any(|&child| definitely_returns(ast, child)),
        NodeKind::IfStatement => {
            let children = ast.fixed_children(node);
            definitely_returns(ast, children[1]) && definitely_returns(ast, children[2])
        }
        NodeKind::WhenStatement => {
            let arms = ast.multi_children(node);
            let has_else = arms
                .iter()
                .any(|&arm| !arm.is_null() && ast.node(arm).kind == NodeKind::ElseConditionArm);
            has_else
                && arms.iter().all(|&arm| {
                    if arm.is_null() {
                        return false;
                    }
                    let body = match ast.node(arm).kind {
                        NodeKind::ConditionArm => ast.fixed_children(arm)[1],
                        NodeKind::ElseConditionArm => ast.fixed_children(arm)[0],
                        _ => return false,
                    };
                    definitely_returns(ast, body)
                })
        }
        _ => false,
    }
}

pub(crate) fn body_definitely_returns(ast: &Ast, node: NodeIndex) -> bool {
    definitely_returns(ast, node)
}

pub(crate) fn infer_return_type(
    r: &mut Resolver,
    ast: &Ast,
    body: NodeIndex,
    mut returns: Vec<TypeIndex>,
) -> TypeIndex {
    if !definitely_returns(ast, body) {
        returns.push(
            r.node_types
                .get(&body)
                .copied()
                .unwrap_or(Intrinsic::Unit.type_index()),
        );
    }
    join_result_types(r, returns)
}

pub(crate) fn coerce_result_value(r: &mut Resolver, ast: &Ast, node: NodeIndex, target: TypeIndex) {
    if node.is_null() {
        return;
    }
    match ast.node(node).kind {
        NodeKind::Block => {
            if let Some(&last) = ast.multi_children(node).last() {
                coerce_result_value(r, ast, last, target);
            }
        }
        NodeKind::ExprStatement | NodeKind::ReturnStatement | NodeKind::ResumeStatement => {
            coerce_result_value(r, ast, ast.fixed_children(node)[0], target);
        }
        NodeKind::NamedArg | NodeKind::PropertyAssignment => {
            coerce_result_value(r, ast, ast.fixed_children(node)[1], target)
        }
        NodeKind::IfStatement => {
            for &branch in &ast.fixed_children(node)[1..3] {
                coerce_result_value(r, ast, branch, target);
            }
        }
        NodeKind::WhenStatement => {
            for &arm in ast.multi_children(node) {
                if arm.is_null() {
                    continue;
                }
                let body = match ast.node(arm).kind {
                    NodeKind::ConditionArm => ast.fixed_children(arm)[1],
                    NodeKind::ElseConditionArm => ast.fixed_children(arm)[0],
                    _ => continue,
                };
                coerce_result_value(r, ast, body, target);
            }
        }
        NodeKind::PostMatch => {
            for &arm in ast.multi_children(node) {
                coerce_result_value(r, ast, ast.fixed_children(arm)[1], target);
            }
        }
        _ => {
            if let Some(&actual) = r.node_types.get(&node) {
                record_coercion(r, node, ast.node(node).kind, actual, Some(target));
            }
        }
    }
}

pub(crate) fn coerce_inferred_returns(
    r: &mut Resolver,
    ast: &Ast,
    body: NodeIndex,
    target: TypeIndex,
) {
    fn visit(r: &mut Resolver, ast: &Ast, node: NodeIndex, target: TypeIndex) {
        if node.is_null() || r.handler_arms.contains(&node) {
            return;
        }
        match ast.node(node).kind {
            NodeKind::FunctionDef
            | NodeKind::Lambda
            | NodeKind::CaseMap
            | NodeKind::CaseAlternative => return,
            NodeKind::ReturnStatement | NodeKind::ResumeStatement => {
                coerce_result_value(r, ast, ast.fixed_children(node)[0], target)
            }
            _ => {}
        }
        for &child in ast
            .fixed_children(node)
            .iter()
            .chain(ast.multi_children(node))
        {
            visit(r, ast, child, target);
        }
    }
    visit(r, ast, body, target);
    if !definitely_returns(ast, body) {
        coerce_result_value(r, ast, body, target);
    }
}

pub(crate) fn resolve_function_def_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    if ast.node(node_idx).kind == NodeKind::TraitDeriveFn
        && r.default_body_context.is_none()
        && r.node_scopes
            .get(&node_idx)
            .and_then(|scope| crate::associated::enclosing_type(r, *scope))
            .is_some_and(|ty| {
                crate::associated_types::has_declarations(r, ty)
                    || crate::default_body::requires_replay(r, ast, node_idx)
            })
    {
        return;
    }
    if r.function_inference.bodies.contains_key(&node_idx) {
        return;
    }
    r.function_inference
        .bodies
        .insert(node_idx, crate::inference::BodyState::InProgress);
    for &parameter in ast.multi_children(node_idx) {
        resolve_param_types(r, ast, parameter);
    }
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] ret_type  [2] body  [3] capability
    // multi: params

    // A forward Any result is unknown, not a contextual source annotation.
    let name_node = children[0];
    let ret_type = resolve_type_expr(r, ast, children[1])
        .or_else(|| crate::capability::capability_expected_return(r, node_idx))
        .or_else(|| {
            (str_interner::get(ast.node(name_node).str_id) == "__init__")
                .then_some(Intrinsic::Unit.type_index())
        });

    // Resolve body with expected return type for bidirectional inference.
    let body_return_type = ret_type.map(|ty| match r.type_pool.get(ty).kind {
        TypeKind::EffectQualified { inner, .. } => inner,
        _ => ty,
    });
    r.return_types.push(body_return_type);
    r.inferred_returns.push(Vec::new());
    resolve_types_expected(r, ast, children[2], body_return_type);
    r.return_types.pop();
    let returns = r
        .inferred_returns
        .pop()
        .expect("function return inference context was pushed before resolving its body");
    if ret_type.is_none() && !r.function_inference.cyclic.contains(&node_idx) {
        let inferred = infer_return_type(r, ast, children[2], returns);
        coerce_inferred_returns(r, ast, children[2], inferred);
        crate::errors::finish_callable(r, node_idx, inferred);
        if let Some(&symbol) = r.node_symbols.get(&name_node) {
            let signature = r.symbols[symbol.0 as usize].type_index;
            if let TypeKind::Function { params, .. } = r.type_pool.get(signature).kind.clone() {
                let signature = r.register_type(TypeKind::Function {
                    params,
                    ret: inferred,
                });
                r.symbols[symbol.0 as usize].type_index = signature;
            }
        }
        if let Some(decl) = r.capabilities.iter_mut().find(|c| c.def_node == node_idx) {
            decl.return_type = inferred;
        }
    }

    if let Some(target) = body_return_type {
        crate::errors::finish_callable(r, node_idx, target);
    }

    // Check: body type vs declared return type.
    if let Some(ret_ti) = body_return_type
        && let Some(&body_ti) = r.node_types.get(&children[2])
        && !crate::trait_typing::is_subtype(r, ast, children[2], body_ti, ret_ti)
        && !r.type_pool.is_gradually_consistent(body_ti, ret_ti)
    {
        let fn_name = str_interner::get(ast.node(name_node).str_id);
        let expected = type_display_name(&r.type_pool, ret_ti);
        let found = type_display_name(&r.type_pool, body_ti);
        let body_span = ast.node(children[2]).span;
        let ret_type_span = ast.node(children[1]).span;
        r.diag_ctx
                    .error(format!(
                        "type mismatch in function `{fn_name}`: expected return type `{expected}`, found `{found}`"
                    ))
                    .with_primary_span(body_span)
                    .with_label(body_span, format!("this has type `{found}`"), Level::Error)
                    .with_label(ret_type_span, format!("expected `{expected}` because of return type"), Level::Note)
                    .emit(r.diag_ctx);
    }

    // Resolve capability.
    resolve_types(r, ast, children[3]);
    r.function_inference
        .bodies
        .insert(node_idx, crate::inference::BodyState::Ready);
}

// ---------------------------------------------------------------------------
// Lambda type resolution
// ---------------------------------------------------------------------------

fn resolve_lambda_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let children = ast.fixed_children(node_idx);
    // Lambda: fixed=[0] body, [1] ret_type   multi=params
    let body_node = children[0];
    let ret_type_node = children[1];
    let params = ast.multi_children(node_idx).to_vec();
    crate::capability::validate_lambda_params(r, ast, &params);

    // Extract expected parameter/return types from context (if expected is a Function type).
    let (expected_param_types, expected_ret) = expected
        .and_then(|exp| {
            if let TypeKind::Function { params, ret } = &r.type_pool.get(exp).kind {
                Some((Some(params.clone()), Some(*ret)))
            } else {
                None
            }
        })
        .unwrap_or((None, None));

    // Resolve parameter types: use annotations if present, fall back to expected types.
    let mut param_type_list = Vec::with_capacity(params.len());
    for (i, &param) in params.iter().enumerate() {
        resolve_param_types(r, ast, param);
        let from_annotation = param_type_index(r, ast, param);
        let from_expected = expected_param_types
            .as_ref()
            .and_then(|pts| pts.get(i).copied());
        let ti = from_annotation
            .or(from_expected)
            .unwrap_or(Intrinsic::Any.type_index());
        // Propagate resolved type to the parameter symbol.
        if ti != TypeIndex::INVALID
            && let Some(pat_node) = param_pattern_node(ast, param)
        {
            let body_ti = crate::default_methods::contextual_parameter_type(r, ast, param, ti)?;
            resolve_pattern_literals(r, ast, pat_node, Some(body_ti));
        }
        param_type_list.push(ti);
    }

    crate::arguments::validate_variadic_parameters(r, ast, node_idx, &params);

    // Resolve explicit return type annotation.
    let explicit_ret = resolve_type_expr(r, ast, ret_type_node);

    // Resolve body with expected return type for bidirectional inference.
    let body_expected = explicit_ret.or(expected_ret);
    r.return_types.push(body_expected);
    r.inferred_returns.push(Vec::new());
    resolve_types_expected(r, ast, body_node, body_expected);
    r.return_types.pop();
    let returns = r
        .inferred_returns
        .pop()
        .expect("lambda return inference context was pushed before resolving its body");
    let body_type = r.node_types.get(&body_node).copied();

    // Determine return type: explicit annotation > body type > expected > Unit.
    let ret_type = explicit_ret
        .or(expected_ret)
        .unwrap_or_else(|| infer_return_type(r, ast, body_node, returns));
    if body_expected.is_none() {
        coerce_inferred_returns(r, ast, body_node, ret_type);
    }

    crate::errors::finish_callable(r, node_idx, ret_type);

    // Both explicit and contextual return contracts must match the body.
    if let (Some(ann_ret), Some(body_ti)) = (body_expected, body_type)
        && !crate::trait_typing::is_subtype(r, ast, body_node, body_ti, ann_ret)
        && !r.type_pool.is_gradually_consistent(body_ti, ann_ret)
    {
        let expected_name = type_display_name(&r.type_pool, ann_ret);
        let found_name = type_display_name(&r.type_pool, body_ti);
        let body_span = ast.node(body_node).span;
        let ret_span = ast.node(ret_type_node).span;
        r.diag_ctx
                .error(format!(
                    "type mismatch in lambda: expected return type `{expected_name}`, found `{found_name}`"
                ))
                .with_primary_span(body_span)
                .with_label(body_span, format!("this has type `{found_name}`"), Level::Error)
                .with_label(ret_span, format!("expected `{expected_name}` because of return type"), Level::Note)
                .emit(r.diag_ctx);
    }

    // Build the Function type.
    let func_ti = r.register_type(TypeKind::Function {
        params: param_type_list,
        ret: ret_type,
    });
    Some(func_ti)
}

fn peel_case_top_pattern(ast: &Ast, mut pat: NodeIndex) -> NodeIndex {
    while !pat.is_null()
        && matches!(
            ast.node(pat).kind,
            NodeKind::PatternIfGuard | NodeKind::PatternAsBind
        )
    {
        pat = ast.fixed_children(pat)[0];
    }
    pat
}

fn resolve_case_closure_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let mut arms = Vec::new();
    crate::flatten_case_arms(ast, node_idx, &mut arms);

    let (expected_param, expected_ret) = expected
        .and_then(|exp| r.type_pool.canonical_type(exp))
        .and_then(|exp| {
            if let TypeKind::Function { params, ret } = &r.type_pool.get(exp).kind {
                let param = (params.len() == 1).then(|| params[0]);
                Some((param, Some(*ret)))
            } else {
                None
            }
        })
        .unwrap_or((None, None));

    let mut typed_annotations = Vec::new();
    let mut all_arms_typed = !arms.is_empty();
    for &arm in &arms {
        let core = peel_case_top_pattern(ast, ast.fixed_children(arm)[0]);
        if !core.is_null() && ast.node(core).kind == NodeKind::PatternTyped {
            let ty_expr = ast.fixed_children(core)[1];
            if let Some(ty) = resolve_type_expr(r, ast, ty_expr) {
                typed_annotations.push((core, ty));
            } else {
                all_arms_typed = false;
            }
        } else {
            all_arms_typed = false;
        }
    }

    let from_annotation = if all_arms_typed && !typed_annotations.is_empty() {
        let first = typed_annotations[0].1;
        let first_canon = r.type_pool.canonical_type(first);
        let same = typed_annotations
            .iter()
            .all(|&(_, ty)| r.type_pool.canonical_type(ty) == first_canon);
        if same {
            for &(core, _) in &typed_annotations {
                r.annotation_typed_patterns.insert(core);
            }
            Some(first)
        } else {
            None
        }
    } else {
        None
    };

    let param_ty = from_annotation
        .or(expected_param)
        .unwrap_or(Intrinsic::Any.type_index());

    r.return_types.push(expected_ret);
    r.inferred_returns.push(Vec::new());
    let mut bodies = Vec::new();
    for &arm in &arms {
        let arm_children = ast.fixed_children(arm);
        crate::enums::pattern(r, ast, arm_children[0], Some(param_ty));
        resolve_types_expected(r, ast, arm_children[1], expected_ret);
        if !definitely_returns(ast, arm_children[1])
            && let Some(&ty) = r.node_types.get(&arm_children[1])
            && r.type_pool.as_intrinsic(ty) != Some(Intrinsic::NoReturn)
        {
            bodies.push((arm_children[1], ty));
        }
    }
    r.return_types.pop();
    let returns = r
        .inferred_returns
        .pop()
        .expect("case closure return inference context was pushed before resolving arms");

    if r.type_pool.error_shape(param_ty).ok().flatten().is_some()
        && !crate::error_coverage::direct_exhaustive(r, ast, &arms, param_ty)
    {
        crate::errors::report(
            r,
            ast,
            node_idx,
            "Error match must exhaust both success and error branches; guards and refutable patterns do not prove coverage",
        );
    }

    let mut all_result_types = returns;
    all_result_types.extend(bodies.iter().map(|&(_, ty)| ty));
    let ret_type = expected_ret.unwrap_or_else(|| {
        if all_result_types.is_empty() {
            Intrinsic::NoReturn.type_index()
        } else {
            join_result_types(r, all_result_types)
        }
    });

    if expected_ret.is_none() {
        for &arm in &arms {
            coerce_inferred_returns(r, ast, ast.fixed_children(arm)[1], ret_type);
        }
    }
    for (body, _) in bodies {
        resolve_types_expected(r, ast, body, Some(ret_type));
        check_expected_type(r, ast, body, ret_type, "case arm");
    }

    crate::errors::finish_callable(r, node_idx, ret_type);

    let func_ti = r.register_type(TypeKind::Function {
        params: vec![param_ty],
        ret: ret_type,
    });
    Some(func_ti)
}

/// Get the pattern node of a parameter (the identifier binding).
fn param_pattern_node(ast: &Ast, param_idx: NodeIndex) -> Option<NodeIndex> {
    if param_idx.is_null() {
        return None;
    }
    match ast.node(param_idx).kind {
        NodeKind::ParamTyped | NodeKind::ParamLambda => {
            let children = ast.fixed_children(param_idx);
            Some(children[0])
        }
        NodeKind::ParamOptional | NodeKind::ParamVarargs => Some(ast.fixed_children(param_idx)[0]),
        NodeKind::ParamSelf => Some(param_idx),
        _ => None,
    }
}

/// Check defaults only after callable headers are available.
pub(crate) fn resolve_param_types(r: &mut Resolver, ast: &Ast, param_idx: NodeIndex) {
    resolve_param_annotation(r, ast, param_idx);
    if !param_idx.is_null() && ast.node(param_idx).kind == NodeKind::ParamOptional {
        let default = ast.fixed_children(param_idx)[2];
        if let Some(ti) = param_type_index(r, ast, param_idx) {
            resolve_types_expected(r, ast, default, Some(ti));
            check_expected_type(r, ast, default, ti, "parameter default");
        } else {
            resolve_types(r, ast, default);
        }
    }
}

/// Resolve a parameter annotation without checking its default expression.
pub(crate) fn resolve_param_annotation(r: &mut Resolver, ast: &Ast, param_idx: NodeIndex) {
    if param_idx.is_null() {
        return;
    }
    let kind = ast.node(param_idx).kind;
    match kind {
        NodeKind::ParamSelf => {
            if let Some(&symbol) = r.node_symbols.get(&param_idx) {
                if let Some(ty) =
                    crate::associated::enclosing_type(r, r.symbols[symbol.0 as usize].scope)
                {
                    r.symbols[symbol.0 as usize].type_index = ty;
                } else {
                    r.diag_ctx
                        .error("self parameters require an enclosing type scope".into())
                        .with_primary_span(ast.node(param_idx).span)
                        .emit(r.diag_ctx);
                }
            }
        }
        NodeKind::ParamTyped | NodeKind::ParamLambda => {
            let children = ast.fixed_children(param_idx);
            // [0] pattern  [1] type (or [0] = PatternTyped([inner, type]), [1] = NULL)
            let (pat_node, type_node) = if children[1].is_null()
                && !children[0].is_null()
                && ast.node(children[0]).kind == NodeKind::PatternTyped
            {
                let inner = ast.fixed_children(children[0]);
                r.annotation_typed_patterns.insert(children[0]);
                (inner[0], inner[1])
            } else {
                (children[0], children[1])
            };
            if let Some(ti) = resolve_type_expr(r, ast, type_node) {
                r.node_types.insert(children[0], ti);
                let Some(ti) = crate::default_methods::parameter_body_type(r, ast, type_node, ti)
                else {
                    return;
                };
                // Set the parameter symbol's type.
                resolve_pattern_literals(r, ast, pat_node, Some(ti));
                crate::error_patterns::validate_binding(r, ast, pat_node, 0);
                if let Some(&sym_id) = r.node_symbols.get(&pat_node) {
                    r.symbols[sym_id.0 as usize].type_index = ti;
                }
            }
        }
        NodeKind::ParamOptional => {
            let children = ast.fixed_children(param_idx);
            // [0] id  [1] type  [2] default
            let expected = resolve_type_expr(r, ast, children[1]);
            if let Some(ti) = expected {
                if let Some(&symbol) = r.node_symbols.get(&children[0]) {
                    r.symbols[symbol.0 as usize].type_index = ti;
                }
            } else {
                r.diag_ctx
                    .error("optional parameters require a valid type annotation".into())
                    .with_primary_span(ast.node(param_idx).span)
                    .emit(r.diag_ctx);
            }
        }
        NodeKind::ParamVarargs => {
            let children = ast.fixed_children(param_idx);
            // [0] id  [1] type
            let annotation = resolve_type_expr(r, ast, children[1]);
            if let Some(ti) = annotation.filter(|&ty| {
                matches!(
                    r.type_pool.collection_role(ty),
                    Some(type_pool::CollectionRole::List | type_pool::CollectionRole::Map)
                )
            }) {
                if let Some(&sym_id) = r.node_symbols.get(&children[0]) {
                    r.symbols[sym_id.0 as usize].type_index = ti;
                }
            } else {
                r.diag_ctx
                    .error("a variadic parameter requires a List type annotation".into())
                    .with_primary_span(ast.node(param_idx).span)
                    .emit(r.diag_ctx);
            }
        }
        _ => {}
    }
}

/// Extract the TypeIndex of a parameter (if resolved).
pub(crate) fn param_type_index(r: &Resolver, ast: &Ast, param_idx: NodeIndex) -> Option<TypeIndex> {
    if param_idx.is_null() {
        return None;
    }
    let kind = ast.node(param_idx).kind;
    let pat_node = match kind {
        NodeKind::ParamTyped | NodeKind::ParamLambda => ast.fixed_children(param_idx)[0],
        NodeKind::ParamOptional | NodeKind::ParamVarargs => ast.fixed_children(param_idx)[0],
        NodeKind::ParamSelf => param_idx,
        _ => return None,
    };
    if let Some(&ty) = r.node_types.get(&pat_node) {
        return Some(ty);
    }
    if let Some(&sym_id) = r.node_symbols.get(&pat_node) {
        let ti = r.symbols[sym_id.0 as usize].type_index;
        if ti != TypeIndex::INVALID {
            return Some(ti);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Variable declaration type resolution
// ---------------------------------------------------------------------------

pub(crate) fn resolve_decl(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    if r.function_inference.values.contains(&node_idx) {
        return;
    }
    let children = ast.fixed_children(node_idx);
    // [0] pattern  [1] type_annotation  [2] value  ([3] else for refutable)
    let has_else = children.len() > 3 && !children[3].is_null();

    // Resolve type annotation.
    let annotation_ti = if children.len() > 1 {
        resolve_type_expr(r, ast, children[1])
    } else {
        None
    };

    // Pass the declared type into bidirectional inference before checking compatibility.
    if children.len() > 2 {
        resolve_types_expected(r, ast, children[2], annotation_ti);
    }

    // Determine the effective type: annotation takes priority, fallback to value type.
    let val_ti = if children.len() > 2 {
        r.node_types.get(&children[2]).copied()
    } else {
        None
    };

    let binding_annotation = annotation_ti
        .and_then(|ty| crate::default_methods::parameter_body_type(r, ast, children[1], ty));
    let effective_ti = binding_annotation.or(val_ti);

    // Check: if both annotation and value type are known, verify compatibility.
    if let (Some(ann_ti), Some(v_ti)) = (annotation_ti, val_ti)
        && !crate::trait_typing::is_subtype(r, ast, children[2], v_ti, ann_ti)
        && !r.type_pool.is_gradually_consistent(v_ti, ann_ti)
    {
        let expected = type_display_name(&r.type_pool, ann_ti);
        let found = type_display_name(&r.type_pool, v_ti);
        let val_span = ast.node(children[2]).span;
        let ann_span = ast.node(children[1]).span;
        r.diag_ctx
            .error(format!(
                "type mismatch: expected `{expected}`, found `{found}`"
            ))
            .with_primary_span(val_span)
            .with_label(
                val_span,
                format!("expected `{expected}`, found `{found}`"),
                Level::Error,
            )
            .with_label(
                ann_span,
                "expected due to this type annotation".to_string(),
                Level::Note,
            )
            .emit(r.diag_ctx);
    }

    // Resolve and validate diverging `else` branch BEFORE binding pattern types.
    if has_else {
        let else_node = children[3];
        resolve_types(r, ast, else_node);
        if !definitely_diverges(r, ast, else_node) {
            r.diag_ctx
                .error(
                    "the `else` branch of a `let`/`const`/`var` binding must diverge (e.g. `return`, `break`, `continue`, or `panic`)"
                        .into(),
                )
                .with_primary_span(ast.node(else_node).span)
                .emit(r.diag_ctx);
        }
        crate::enums::pattern(r, ast, children[0], effective_ti);
    } else if let Some(ti) = effective_ti {
        // Propagate type to the irrefutable pattern symbol(s).
        resolve_pattern_literals(r, ast, children[0], Some(ti));
        crate::error_patterns::validate_binding(r, ast, children[0], 0);
    }
}

pub(crate) fn resolve_irrefutable_pattern(
    r: &mut Resolver,
    ast: &Ast,
    pat: NodeIndex,
    ty: TypeIndex,
) {
    resolve_pattern_literals(r, ast, pat, Some(ty));
    crate::error_patterns::validate_binding(r, ast, pat, 0);
}

fn definitely_diverges(r: &Resolver, ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() {
        return false;
    }
    if r.node_types
        .get(&node)
        .is_some_and(|&ty| r.type_pool.as_intrinsic(ty) == Some(Intrinsic::NoReturn))
    {
        return true;
    }
    match ast.node(node).kind {
        NodeKind::ExprStatement => definitely_diverges(r, ast, ast.fixed_children(node)[0]),
        NodeKind::ReturnStatement
        | NodeKind::ResumeStatement
        | NodeKind::BreakStatement
        | NodeKind::ContinueStatement => ast.fixed_children(node)[1].is_null(),
        NodeKind::Call => {
            let callee = ast.fixed_children(node)[0];
            if let Some(&sym) = r.node_symbols.get(&callee)
                && matches!(
                    r.builtin_fns.get(&sym),
                    Some(&runtime::ids::PANIC | &runtime::ids::EXIT)
                )
            {
                return true;
            }
            ast.multi_children(node)
                .iter()
                .any(|&arg| definitely_diverges(r, ast, arg))
        }
        NodeKind::InfixFnCall => {
            if let Some(&sym) = r.node_symbols.get(&node)
                && matches!(
                    r.builtin_fns.get(&sym),
                    Some(&runtime::ids::PANIC | &runtime::ids::EXIT)
                )
            {
                return true;
            }
            ast.fixed_children(node)
                .iter()
                .any(|&arg| definitely_diverges(r, ast, arg))
        }
        NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl => {
            let children = ast.fixed_children(node);
            children.len() > 2 && definitely_diverges(r, ast, children[2])
        }
        NodeKind::LetHandler => {
            let children = ast.fixed_children(node);
            children.len() > 3 && definitely_diverges(r, ast, children[3])
        }
        NodeKind::Block => ast
            .multi_children(node)
            .iter()
            .any(|&stmt| definitely_diverges(r, ast, stmt)),
        NodeKind::IfStatement => {
            let children = ast.fixed_children(node);
            !children[2].is_null()
                && definitely_diverges(r, ast, children[1])
                && definitely_diverges(r, ast, children[2])
        }
        NodeKind::WhenStatement => {
            let arms = ast.multi_children(node);
            arms.iter()
                .any(|&arm| !arm.is_null() && ast.node(arm).kind == NodeKind::ElseConditionArm)
                && arms.iter().all(|&arm| {
                    if arm.is_null() {
                        return false;
                    }
                    let body = match ast.node(arm).kind {
                        NodeKind::ConditionArm => ast.fixed_children(arm)[1],
                        NodeKind::ElseConditionArm => ast.fixed_children(arm)[0],
                        _ => return false,
                    };
                    definitely_diverges(r, ast, body)
                })
        }
        NodeKind::PostMatch => {
            let arms = ast.multi_children(node);
            !arms.is_empty()
                && arms
                    .iter()
                    .all(|&arm| definitely_diverges(r, ast, ast.fixed_children(arm)[1]))
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Call type inference
// ---------------------------------------------------------------------------

fn resolve_when_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let arms = ast.multi_children(node_idx);
    let has_else = arms
        .iter()
        .any(|&arm| !arm.is_null() && ast.node(arm).kind == NodeKind::ElseConditionArm);
    let bool_type = Intrinsic::Bool.type_index();
    let arm_expected = if has_else { expected } else { None };
    let mut bodies = Vec::new();
    for &arm in arms {
        if arm.is_null() {
            continue;
        }
        match ast.node(arm).kind {
            NodeKind::ConditionArm => {
                let children = ast.fixed_children(arm);
                resolve_types_expected(r, ast, children[0], Some(bool_type));
                check_expected_type(r, ast, children[0], bool_type, "when condition");
                resolve_types_expected(r, ast, children[1], arm_expected);
                if has_else
                    && !definitely_returns(ast, children[1])
                    && let Some(&ty) = r.node_types.get(&children[1])
                    && r.type_pool.as_intrinsic(ty) != Some(Intrinsic::NoReturn)
                {
                    bodies.push((children[1], ty));
                }
            }
            NodeKind::ElseConditionArm => {
                let body = ast.fixed_children(arm)[0];
                resolve_types_expected(r, ast, body, arm_expected);
                if !definitely_returns(ast, body)
                    && let Some(&ty) = r.node_types.get(&body)
                    && r.type_pool.as_intrinsic(ty) != Some(Intrinsic::NoReturn)
                {
                    bodies.push((body, ty));
                }
            }
            _ => resolve_types(r, ast, arm),
        }
    }
    if !has_else {
        return Some(Intrinsic::Unit.type_index());
    }
    let result = expected.unwrap_or_else(|| {
        if bodies.is_empty() {
            Intrinsic::NoReturn.type_index()
        } else {
            join_result_types(r, bodies.iter().map(|&(_, ty)| ty))
        }
    });
    for (body, _) in bodies {
        resolve_types_expected(r, ast, body, Some(result));
        check_expected_type(r, ast, body, result, "when arm");
    }
    Some(result)
}

fn resolve_infix_call_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) -> Option<TypeIndex> {
    let args = ast.fixed_children(node_idx);
    let Some(&callee_sym) = r.node_symbols.get(&node_idx) else {
        resolve_types(r, ast, args[0]);
        resolve_types(r, ast, args[1]);
        return None;
    };
    let sym_kind = r.symbols[callee_sym.0 as usize].kind;
    let sym_ti = r.symbols[callee_sym.0 as usize].type_index;
    if matches!(sym_kind, SymbolKind::Type | SymbolKind::Trait) {
        r.diag_ctx
            .error("Type values are not callable; types cannot be dynamically constructed".into())
            .with_primary_span(ast.node(node_idx).span)
            .emit(r.diag_ctx);
        resolve_types(r, ast, args[0]);
        resolve_types(r, ast, args[1]);
        return None;
    }
    if sym_ti != TypeIndex::INVALID
        && crate::applications::is_instance(r, sym_ti)
        && sym_kind != SymbolKind::Effect
    {
        return crate::applications::apply(r, ast, node_idx, sym_ti);
    }
    let canonical_ti = (sym_ti != TypeIndex::INVALID)
        .then(|| r.type_pool.canonical_type(sym_ti))
        .flatten();
    let param_types = canonical_ti
        .and_then(|ti| match &r.type_pool.get(ti).kind {
            TypeKind::Function { params, .. } | TypeKind::Effect { params, .. } => {
                Some(params.clone())
            }
            _ => None,
        })
        .or_else(|| get_callee_param_types(r, ast, node_idx));

    let plan = crate::arguments::plan_call(r, ast, node_idx);
    let declaration_bound = plan.is_some();
    let mut argument_parameters = vec![None; args.len()];
    let mut variadic_arguments = std::collections::HashSet::new();
    if let Some(plan) = plan {
        for (index, binding) in plan.parameters.iter().enumerate() {
            if let crate::CallArgumentValue::Explicit { source_index } = binding.value {
                argument_parameters[source_index] = Some(index);
            } else if let crate::CallArgumentValue::Variadic { ref source_indices } = binding.value
            {
                variadic_arguments.extend(source_indices.iter().copied());
            }
        }
        r.call_arguments.insert(node_idx, plan);
    }

    for (i, &arg) in args.iter().enumerate() {
        let parameter_index = argument_parameters[i].unwrap_or(i);
        let exp = if variadic_arguments.contains(&i) {
            Some(Intrinsic::Any.type_index())
        } else {
            param_types.as_ref().and_then(|pts| {
                let t = pts.get(parameter_index).copied()?;
                (t != TypeIndex::INVALID).then_some(t)
            })
        };
        resolve_types_expected(r, ast, arg, exp);
        if declaration_bound && let Some(exp) = exp {
            check_expected_type(r, ast, arg, exp, "argument");
        }
    }

    if !declaration_bound && let Some(ref pts) = param_types {
        let callee_name = str_interner::get(r.symbols[callee_sym.0 as usize].name);
        if pts.len() != args.len() {
            let arity = format!(
                "{} argument{}",
                pts.len(),
                if pts.len() == 1 { "" } else { "s" }
            );
            r.diag_ctx
                .error(format!(
                    "`{callee_name}` expects {arity}, but 2 were supplied"
                ))
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
        } else {
            for (i, &arg) in args.iter().enumerate() {
                if let Some(&arg_ti) = r.node_types.get(&arg) {
                    let param_ti = pts[i];
                    if param_ti != TypeIndex::INVALID
                        && !crate::trait_typing::is_subtype(r, ast, arg, arg_ti, param_ti)
                        && !r.type_pool.is_gradually_consistent(arg_ti, param_ti)
                    {
                        let expected_name = type_display_name(&r.type_pool, param_ti);
                        let found_name = type_display_name(&r.type_pool, arg_ti);
                        let arg_span = ast.node(arg).span;
                        r.diag_ctx
                            .error(format!(
                                "type mismatch in argument {} of `{callee_name}`: expected `{expected_name}`, found `{found_name}`",
                                i + 1
                            ))
                            .with_primary_span(arg_span)
                            .with_label(
                                arg_span,
                                format!("expected `{expected_name}`, found `{found_name}`"),
                                Level::Error,
                            )
                            .emit(r.diag_ctx);
                    }
                }
            }
        }
    }

    if let SymbolKind::BuiltinFunction(id) = sym_kind {
        return Some(builtin_return_type(&r.type_pool, id));
    }
    if let Some(ti) = canonical_ti {
        if r.type_pool.as_intrinsic(ti) == Some(Intrinsic::Any) {
            return Some(Intrinsic::Any.type_index());
        }
        if let TypeKind::Function { ret, .. } | TypeKind::Effect { ret, .. } =
            &r.type_pool.get(ti).kind
        {
            return Some(*ret);
        }
    }
    if sym_ti == TypeIndex::INVALID
        && matches!(
            sym_kind,
            SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant
        )
    {
        return Some(Intrinsic::Any.type_index());
    }
    None
}

fn resolve_call_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    if let Some(result) = crate::type_factories::application(r, ast, node_idx) {
        return match result {
            Ok(ty) => {
                r.node_type_values.insert(node_idx, ty);
                Some(Intrinsic::Type.type_index())
            }
            Err(message) => {
                r.diag_ctx
                    .error(message)
                    .with_primary_span(ast.node(node_idx).span)
                    .emit(r.diag_ctx);
                None
            }
        };
    }
    let children = ast.fixed_children(node_idx);
    // [0] callee  multi = args
    let callee = children[0];
    if let Some(ty) = crate::enums::construction(r, ast, node_idx) {
        crate::enums::record_reference(r, ast, callee);
        return Some(ty);
    }
    if let Some(ty) = crate::newtype::construction(r, ast, node_idx) {
        return Some(ty);
    }
    let previous_callee = r.current_call_callee.replace(callee);
    resolve_types(r, ast, callee);
    r.current_call_callee = previous_callee;
    if let Some(kind) = r
        .node_types
        .get(&callee)
        .and_then(|&ty| crate::collections::CollectionKind::from_type(&r.type_pool, ty))
    {
        return Some(crate::collections::index_call(r, ast, node_idx, kind));
    }
    let is_effect_operation = r
        .node_symbols
        .get(&callee)
        .is_some_and(|symbol| r.symbols[symbol.0 as usize].kind == SymbolKind::Effect);
    if r.node_types
        .get(&callee)
        .is_some_and(|&ty| r.type_pool.as_intrinsic(ty) == Some(Intrinsic::Type))
        && !is_effect_operation
    {
        r.diag_ctx
            .error("Type values are not callable; types cannot be dynamically constructed".into())
            .with_primary_span(ast.node(callee).span)
            .emit(r.diag_ctx);
        return None;
    }

    // Retrieve callee's parameter types to propagate expected types to arguments.
    if let Some(&ty) = r.node_types.get(&callee)
        && crate::applications::is_instance(r, ty)
        && !is_effect_operation
    {
        return crate::applications::apply(r, ast, node_idx, ty);
    }
    let param_types = get_callee_param_types(r, ast, callee);

    // Resolve argument types with expected parameter types for bidirectional inference.
    let args = ast.multi_children(node_idx);
    if r.node_types
        .get(&callee)
        .is_some_and(|&ty| r.type_pool.as_intrinsic(ty) == Some(Intrinsic::Continuation))
    {
        if args.len() != 1 {
            r.diag_ctx
                .error("continuation invocation requires exactly one value".into())
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
        }
        for &arg in args {
            let input = crate::effect_contracts::input(r, ast, callee);
            resolve_types_expected(r, ast, arg, input);
            if let Some(input) = input {
                check_expected_type(r, ast, arg, input, "continuation input");
            }
        }
        if let Some(target) = expected.filter(|&ty| ty != Intrinsic::Any.type_index()) {
            // A continuation's erased answer is checked at this boundary. In
            // particular an existing Err carrier must not be lifted into Ok.
            r.node_coercions.insert(
                node_idx,
                crate::Coercion {
                    target,
                    kind: crate::CoercionKind::Assert,
                },
            );
            return Some(target);
        }
        return Some(Intrinsic::Any.type_index());
    }
    let plan = crate::arguments::plan_call(r, ast, node_idx);
    let declaration_bound = plan.is_some();
    let has_spread = args
        .iter()
        .any(|&arg| !arg.is_null() && ast.node(arg).kind == NodeKind::SpreadArg);
    let mut argument_parameters = vec![None; args.len()];
    let mut variadic_arguments = std::collections::HashSet::new();
    let mut unpacked_call = false;
    if let Some(mut plan) = plan {
        unpacked_call = plan.spread_pack.is_some();
        for (index, binding) in plan.parameters.iter_mut().enumerate() {
            match &mut binding.value {
                crate::CallArgumentValue::Explicit { source_index } => {
                    argument_parameters[*source_index] = Some(index);
                }
                crate::CallArgumentValue::Variadic { source_indices } => {
                    variadic_arguments.extend(source_indices.iter().copied());
                }
                crate::CallArgumentValue::Unpacked { target_type, .. } => {
                    if let Some(pt) = param_types
                        .as_ref()
                        .and_then(|pts| pts.get(index).copied())
                        .filter(|&t| t != TypeIndex::INVALID)
                    {
                        validate_spread_target_type(r, ast, node_idx, pt);
                        *target_type = Some(pt);
                    }
                }
                crate::CallArgumentValue::UnpackedTail
                | crate::CallArgumentValue::Default { .. }
                | crate::CallArgumentValue::MapVariadic { .. } => {}
            }
        }
        r.call_arguments.insert(node_idx, plan);
    }
    for (i, &arg) in args.iter().enumerate() {
        let expected = if unpacked_call {
            argument_parameters[i].and_then(|param_idx| {
                param_types
                    .as_ref()
                    .and_then(|pts| pts.get(param_idx).copied())
                    .filter(|&t| t != TypeIndex::INVALID)
            })
        } else if variadic_arguments.contains(&i) {
            Some(Intrinsic::Any.type_index())
        } else {
            let parameter_index = argument_parameters[i].unwrap_or(i);
            param_types.as_ref().and_then(|pts| {
                let t = pts.get(parameter_index).copied()?;
                if t != TypeIndex::INVALID {
                    Some(t)
                } else {
                    None
                }
            })
        };
        let check_ctx = (declaration_bound && expected.is_some()).then_some("argument");
        resolve_call_argument_or_spread(r, ast, arg, expected, check_ctx);
    }

    // Back-propagate: if callee has INVALID param types, update them from argument types.
    if !has_spread && let Some(&callee_sym) = r.node_symbols.get(&callee) {
        let ti = r.symbols[callee_sym.0 as usize].type_index;
        if ti != TypeIndex::INVALID {
            let kind = r.type_pool.get(ti).kind.clone();
            if let TypeKind::Function { params, ret } = kind {
                let has_invalid = params.contains(&TypeIndex::INVALID);
                if has_invalid && args.len() == params.len() {
                    let updated_params: Vec<TypeIndex> = params
                        .iter()
                        .enumerate()
                        .map(|(i, &pt)| {
                            if pt == TypeIndex::INVALID {
                                args.get(i)
                                    .and_then(|&arg| r.node_types.get(&arg).copied())
                                    .unwrap_or(TypeIndex::INVALID)
                            } else {
                                pt
                            }
                        })
                        .collect();
                    let new_func_ti = r.register_type(TypeKind::Function {
                        params: updated_params,
                        ret,
                    });
                    r.symbols[callee_sym.0 as usize].type_index = new_func_ti;
                }
            }
        }
    }

    // Validate argument count and types against callee signature.
    if !declaration_bound {
        if has_spread {
            if let Some(ref pts) = param_types {
                let min_explicit = args
                    .iter()
                    .filter(|&&arg| ast.node(arg).kind != NodeKind::SpreadArg)
                    .count();
                if min_explicit > pts.len() {
                    let callee_name = callee_display_name(r, ast, callee);
                    r.diag_ctx
                        .error(format!(
                            "`{callee_name}` expects {} argument{}, but at least {} positional arguments were supplied",
                            pts.len(),
                            if pts.len() == 1 { "" } else { "s" },
                            min_explicit
                        ))
                        .with_primary_span(ast.node(node_idx).span)
                        .emit(r.diag_ctx);
                } else {
                    for &pt in pts {
                        if pt != TypeIndex::INVALID {
                            validate_spread_target_type(r, ast, node_idx, pt);
                        }
                    }
                    let parameters = pts
                        .iter()
                        .enumerate()
                        .map(|(unpack_index, &pt)| crate::CallParameterBinding {
                            parameter: NodeIndex::NULL,
                            symbols: Vec::new(),
                            value: crate::CallArgumentValue::Unpacked {
                                unpack_index,
                                target_type: (pt != TypeIndex::INVALID).then_some(pt),
                            },
                        })
                        .collect();
                    r.call_arguments.insert(
                        node_idx,
                        crate::CallArgumentPlan {
                            declaration: NodeIndex::NULL,
                            parameters,
                            spread_pack: Some(crate::SpreadPackPlan {
                                source_indices: (0..args.len()).collect(),
                                fixed_count: pts.len(),
                                has_variadic_tail: false,
                            }),
                        },
                    );
                }
            } else {
                r.diag_ctx
                    .error(
                        "spread arguments require a callee with a known parameter list or variadic signature"
                            .into(),
                    )
                    .with_primary_span(ast.node(node_idx).span)
                    .emit(r.diag_ctx);
            }
        } else if let Some(ref pts) = param_types {
            let call_span = ast.node(node_idx).span;
            let callee_span = ast.node(callee).span;
            let required = pts.len();

            // Arity check.
            if args.len() < required || args.len() > pts.len() {
                let callee_name = callee_display_name(r, ast, callee);
                let arity = if required == pts.len() {
                    format!(
                        "{required} argument{}",
                        if required == 1 { "" } else { "s" }
                    )
                } else {
                    format!("between {required} and {} arguments", pts.len())
                };
                r.diag_ctx
                    .error(format!(
                        "`{callee_name}` expects {arity}, but {} {} supplied",
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" },
                    ))
                    .with_primary_span(call_span)
                    .with_label(callee_span, format!("expects {arity}"), Level::Note)
                    .emit(r.diag_ctx);
            } else {
                // Type check each argument.
                for (i, &arg) in args.iter().enumerate() {
                    if let Some(&arg_ti) = r.node_types.get(&arg) {
                        let param_ti = pts[i];
                        if param_ti != TypeIndex::INVALID
                            && !crate::trait_typing::is_subtype(r, ast, arg, arg_ti, param_ti)
                            && !r.type_pool.is_gradually_consistent(arg_ti, param_ti)
                        {
                            let callee_name = callee_display_name(r, ast, callee);
                            let expected = type_display_name(&r.type_pool, param_ti);
                            let found = type_display_name(&r.type_pool, arg_ti);
                            let arg_span = ast.node(arg).span;
                            r.diag_ctx
                            .error(format!(
                                "type mismatch in argument {} of `{callee_name}`: expected `{expected}`, found `{found}`",
                                i + 1
                            ))
                            .with_primary_span(arg_span)
                            .with_label(arg_span, format!("expected `{expected}`, found `{found}`"), Level::Error)
                            .emit(r.diag_ctx);
                        }
                    }
                }
            }
        }
    }

    // Infer return type from callee.
    if let Some(&ti) = r.node_types.get(&callee) {
        if r.type_pool.as_intrinsic(ti) == Some(Intrinsic::Any) {
            return Some(Intrinsic::Any.type_index());
        }
        if ti != TypeIndex::INVALID {
            match &r.type_pool.get(ti).kind {
                TypeKind::Function { ret, .. } => return Some(*ret),
                TypeKind::Effect { ret, is_async, .. } => {
                    let ret = *ret;
                    return Some(if *is_async {
                        r.type_pool.intern_future(ret)
                    } else {
                        ret
                    });
                }
                _ => {}
            }
        }
    }
    if let Some(&callee_sym) = r.node_symbols.get(&callee) {
        let sym = &r.symbols[callee_sym.0 as usize];
        if let SymbolKind::BuiltinFunction(id) = sym.kind {
            return Some(builtin_return_type(&r.type_pool, id));
        }
        let ti = sym.type_index;
        if ti != TypeIndex::INVALID {
            match &r.type_pool.get(ti).kind {
                TypeKind::Function { ret, .. } => return Some(*ret),
                TypeKind::Effect { ret, is_async, .. } => {
                    let ret = *ret;
                    return Some(if *is_async {
                        r.type_pool.intern_future(ret)
                    } else {
                        ret
                    });
                }
                _ => {}
            }
        }
    }
    None
}

pub(crate) fn validate_spread_target_type(
    r: &Resolver,
    ast: &Ast,
    call_node: NodeIndex,
    param_ty: TypeIndex,
) {
    if let Some(canon) = r.type_pool.canonical_type(param_ty)
        && matches!(r.type_pool.get(canon).kind, TypeKind::Trait { .. })
    {
        r.diag_ctx
            .error("cannot unpack spread `List` into bare trait parameter".into())
            .with_primary_span(ast.node(call_node).span)
            .emit(r.diag_ctx);
    }
}

pub(crate) fn resolve_call_argument_or_spread(
    r: &mut Resolver,
    ast: &Ast,
    argument: NodeIndex,
    expected: Option<TypeIndex>,
    check_context: Option<&str>,
) {
    if !argument.is_null() && ast.node(argument).kind == NodeKind::SpreadArg {
        let inner = ast.fixed_children(argument)[0];
        resolve_types(r, ast, inner);
        if let Some(&inner_ty) = r.node_types.get(&inner) {
            r.node_types.insert(argument, inner_ty);
            let canon = r.type_pool.canonical_type(inner_ty).unwrap_or(inner_ty);
            let is_tuple = canon != TypeIndex::INVALID
                && matches!(r.type_pool.get(canon).kind, TypeKind::Tuple { .. });
            if inner_ty != TypeIndex::INVALID
                && r.type_pool.collection_role(canon) != Some(type_pool::CollectionRole::List)
                && !is_tuple
                && r.type_pool.as_intrinsic(canon) != Some(Intrinsic::Any)
            {
                let found = type_display_name(&r.type_pool, inner_ty);
                r.diag_ctx
                    .error(format!(
                        "spread argument requires a `List` or tuple value, found `{found}`"
                    ))
                    .with_primary_span(ast.node(argument).span)
                    .emit(r.diag_ctx);
            }
        }
        return;
    }
    resolve_types_expected(r, ast, argument, expected);
    if let (Some(exp), Some(ctx)) = (expected, check_context) {
        check_expected_type(r, ast, crate::argument_value_node(ast, argument), exp, ctx);
    }
}

/// Get the parameter types for a callee, if known.
fn get_callee_param_types(r: &Resolver, ast: &Ast, callee: NodeIndex) -> Option<Vec<TypeIndex>> {
    if let Some(&ti) = r.node_types.get(&callee)
        && ti != TypeIndex::INVALID
        && let TypeKind::Function { params, .. } | TypeKind::Effect { params, .. } =
            &r.type_pool.get(ti).kind
    {
        return Some(params.clone());
    }
    if let Some(&sym_id) = r.node_symbols.get(&callee) {
        let sym = &r.symbols[sym_id.0 as usize];
        let ti = sym.type_index;
        if ti != TypeIndex::INVALID
            && let TypeKind::Function { params, .. } | TypeKind::Effect { params, .. } =
                &r.type_pool.get(ti).kind
        {
            return Some(params.clone());
        }
        // Parameter annotations remain available even when an unannotated
        // return type has not yet produced a complete Function type.
        if sym.kind == SymbolKind::Function && ast.node(sym.def_node).kind == NodeKind::FunctionDef
        {
            let params = ast.multi_children(sym.def_node);
            // Optional/named/variadic argument binding needs its own signature
            // representation; keep its existing path until that is implemented.
            if params
                .iter()
                .all(|&param| ast.node(param).kind == NodeKind::ParamTyped)
            {
                return Some(
                    params
                        .iter()
                        .map(|&param| param_type_index(r, ast, param).unwrap_or(TypeIndex::INVALID))
                        .collect(),
                );
            }
        }
    }
    None
}

/// Get a display name for a callee expression (for diagnostics).
fn callee_display_name(_r: &Resolver, ast: &Ast, callee: NodeIndex) -> String {
    if !callee.is_null() && ast.node(callee).kind == NodeKind::Id {
        return str_interner::get(ast.node(callee).str_id);
    }
    "<callee>".to_string()
}

/// Approximate return type for known builtins (dynamic; std wrappers refine).
fn builtin_return_type(pool: &type_pool::TypePool, id: runtime::BuiltinFnId) -> TypeIndex {
    use runtime::ids;
    match id {
        ids::PRINT
        | ids::PRINTLN
        | ids::EXIT
        | ids::PANIC
        | ids::LIST_SET
        | ids::LIST_PUSH
        | ids::LIST_TRUNCATE
        | ids::LIST_FILL
        | ids::LIST_RESIZE
        | ids::MAP_SET
        | ids::MAP_CLEAR
        | ids::MAP_MERGE
        | ids::LIST_SWAP
        | ids::LIST_CLEAR
        | ids::LIST_INSERT
        | ids::LIST_APPEND
        | ids::IO_EPRINT
        | ids::IO_EPRINTLN
        | ids::TIME_SLEEP_MS
        | ids::TASK_YIELD
        | ids::BYTES_SET
        | ids::BYTES_PUSH
        | ids::BYTES_CLEAR
        | ids::BYTES_EXTEND => Intrinsic::Unit.type_index(),
        ids::TYPE_OF => Intrinsic::Type.type_index(),
        ids::LIST_INIT
        | ids::STR_SPLIT
        | ids::STR_SPLIT_WHITESPACE
        | ids::STR_SPLITN
        | ids::STR_RSPLIT
        | ids::STR_RSPLITN
        | ids::STR_SPLIT_INCLUSIVE
        | ids::STR_FIND_ALL
        | ids::STR_FIND_INDICES
        | ids::STR_RFIND_INDICES
        | ids::STR_BYTE_FIND_INDICES
        | ids::STR_BYTE_RFIND_INDICES
        | ids::STR_SPLIT_TERMINATOR
        | ids::STR_RSPLIT_TERMINATOR
        | ids::STR_SPLIT_ASCII_WHITESPACE
        | ids::CHAR_ENCODE_UTF8
        | ids::CHAR_ENCODE_UTF16
        | ids::STR_ENCODE_UTF8
        | ids::STR_ENCODE_UTF16
        | ids::STR_SPLIT_AT
        | ids::INT_TO_BE_BYTES
        | ids::INT_TO_LE_BYTES
        | ids::F64_TO_BE_BYTES
        | ids::F64_TO_LE_BYTES
        | ids::F64_TO_NE_BYTES
        | ids::STR_CHARS
        | ids::STR_BYTES
        | ids::STR_CHAR_INDICES
        | ids::LIST_WINDOWS
        | ids::LIST_CHUNKS_EXACT
        | ids::LIST_RCHUNKS
        | ids::LIST_RCHUNKS_EXACT
        | ids::LIST_CHUNKS_REMAINDER
        | ids::LIST_RCHUNKS_REMAINDER
        | ids::LIST_SPLIT_AT
        | ids::LIST_ROTATE_LEFT
        | ids::LIST_ROTATE_RIGHT
        | ids::LIST_REPEAT
        | ids::LIST_SPLIT_OFF
        | ids::MAP_VALUES
        | ids::LIST_COPY
        | ids::LIST_REVERSE
        | ids::LIST_CONCAT
        | ids::LIST_TAKE
        | ids::LIST_DROP
        | ids::STR_LINES
        | ids::MAP_ENTRIES
        | ids::LIST_CHUNKS
        | ids::LIST_ENUMERATE
        | ids::LIST_FLATTEN
        | ids::LIST_ZIP
        | ids::ENV_ARGS
        | ids::TASK_AWAIT_ALL
        | ids::BYTES_TO_LIST => pool.list_type().unwrap_or(TypeIndex::INVALID),
        ids::BYTES_NEW
        | ids::BYTES_SLICE
        | ids::BYTES_COPY
        | ids::BYTES_FROM_STR
        | ids::BYTES_FROM_LIST => pool.bytes_type().unwrap_or(TypeIndex::INVALID),
        ids::MAP_INIT | ids::MAP_COPY | ids::ENV_VARS => {
            pool.map_type().unwrap_or(TypeIndex::INVALID)
        }
        ids::LIST_LEN | ids::MAP_LEN => Intrinsic::I64.type_index(),
        ids::LIST_GET
        | ids::LIST_POP
        | ids::LIST_SWAP_REMOVE
        | ids::LIST_REMOVE
        | ids::LIST_FIRST
        | ids::LIST_LAST
        | ids::MAP_GET
        | ids::MAP_REMOVE => Intrinsic::Any.type_index(),
        ids::MAP_CONTAINS
        | ids::STR_CONTAINS
        | ids::STR_STARTS_WITH
        | ids::STR_ENDS_WITH
        | ids::CHAR_IS_WHITESPACE
        | ids::CHAR_IS_ALPHABETIC
        | ids::CHAR_IS_NUMERIC
        | ids::CHAR_IS_ALPHANUMERIC
        | ids::CHAR_IS_UPPERCASE
        | ids::CHAR_IS_LOWERCASE
        | ids::CHAR_IS_ASCII
        | ids::CHAR_IS_ASCII_OCTDIGIT
        | ids::CHAR_IS_ASCII_DIGIT
        | ids::CHAR_IS_ASCII_ALPHABETIC
        | ids::CHAR_IS_ASCII_HEXDIGIT
        | ids::CHAR_IS_ASCII_CONTROL
        | ids::CHAR_IS_ASCII_GRAPHIC
        | ids::CHAR_IS_ASCII_PUNCTUATION
        | ids::CHAR_IS_ASCII_WHITESPACE
        | ids::CHAR_IS_ASCII_ALPHANUMERIC
        | ids::CHAR_IS_ASCII_UPPERCASE
        | ids::CHAR_IS_ASCII_LOWERCASE
        | ids::CHAR_IS_CONTROL
        | ids::STR_IS_ASCII
        | ids::IS_NAN
        | ids::IS_INFINITE
        | ids::IS_FINITE
        | ids::IS_SIGN_NEGATIVE
        | ids::IS_SIGN_POSITIVE
        | ids::IS_POWER_OF_TWO
        | ids::CHAR_IS_DIGIT
        | ids::STR_EQ_IGNORE_ASCII_CASE
        | ids::IS_NORMAL
        | ids::IS_SUBNORMAL
        | ids::IS_MULTIPLE_OF
        | ids::STR_IS_CHAR_BOUNDARY
        | ids::CHAR_EQ_IGNORE_ASCII_CASE
        | ids::MAP_INSERT_ABSENT
        | ids::STR_IS_ASCII_DIGIT
        | ids::STR_IS_ASCII_ALPHABETIC
        | ids::STR_IS_ASCII_ALPHANUMERIC
        | ids::STR_IS_ASCII_LOWERCASE
        | ids::STR_IS_ASCII_UPPERCASE
        | ids::STR_IS_ASCII_WHITESPACE
        | ids::STR_IS_ASCII_HEXDIGIT
        | ids::STR_IS_ASCII_GRAPHIC
        | ids::STR_IS_ASCII_PUNCTUATION
        | ids::STR_IS_ASCII_CONTROL
        | ids::STR_IS_NUMERIC
        | ids::STR_IS_ALPHABETIC
        | ids::STR_IS_ALPHANUMERIC
        | ids::STR_IS_LOWERCASE
        | ids::STR_IS_UPPERCASE
        | ids::STR_IS_WHITESPACE
        | ids::STR_IS_CONTROL
        | ids::RANGE_IS_EMPTY
        | ids::RANGE_INCLUSIVE_IS_EMPTY
        | ids::RANGE_CONTAINS
        | ids::RANGE_INCLUSIVE_CONTAINS
        | ids::RANGE_FROM_CONTAINS
        | ids::RANGE_TO_CONTAINS
        | ids::RANGE_TO_INCLUSIVE_CONTAINS
        | ids::F64_IS_ZERO
        | ids::BOOL_NOT
        | ids::BOOL_AND
        | ids::BOOL_OR
        | ids::BOOL_XOR
        | ids::STR_IS_EMPTY
        | ids::STR_STARTS_WITH_CHAR
        | ids::STR_ENDS_WITH_CHAR
        | ids::STR_CONTAINS_CHAR
        | ids::LIST_IS_EMPTY
        | ids::MAP_IS_EMPTY
        | ids::FILE_IS_OPEN
        | ids::FS_EXISTS
        | ids::FS_IS_FILE
        | ids::FS_IS_DIR
        | ids::TASK_IS_READY
        | ids::BYTES_EQ => Intrinsic::Bool.type_index(),
        ids::CHAR_TO_ASCII_UPPERCASE
        | ids::CHAR_TO_ASCII_LOWERCASE
        | ids::CHAR_REPLACEMENT
        | ids::CHAR_MAX
        | ids::CHAR_MIN => Intrinsic::Char.type_index(),
        ids::TO_STRING
        | ids::STR_CONCAT
        | ids::STR_TRIM
        | ids::STR_TRIM_START
        | ids::STR_TRIM_END
        | ids::STR_TO_UPPER
        | ids::STR_TO_LOWER
        | ids::STR_REPEAT
        | ids::STR_REPLACE
        | ids::STR_SLICE
        | ids::CHAR_INTO_STRING
        | ids::STR_TO_ASCII_UPPERCASE
        | ids::STR_TO_ASCII_LOWERCASE
        | ids::CHAR_TO_UPPERCASE
        | ids::CHAR_TO_LOWERCASE
        | ids::STR_TRIM_START_MATCHES
        | ids::STR_TRIM_END_MATCHES
        | ids::STR_TRIM_MATCHES
        | ids::STR_REPLACEN
        | ids::CHAR_ESCAPE_DEBUG
        | ids::CHAR_ESCAPE_UNICODE
        | ids::CHAR_ESCAPE_DEFAULT
        | ids::STR_ESCAPE_DEBUG
        | ids::STR_ESCAPE_UNICODE
        | ids::STR_ESCAPE_DEFAULT
        | ids::STR_TRIM_ASCII
        | ids::STR_TRIM_ASCII_START
        | ids::STR_TRIM_ASCII_END
        | ids::STR_FROM_UTF8_LOSSY
        | ids::STR_FROM_UTF16_LOSSY
        | ids::INT_TO_STR_RADIX
        | ids::STR_TRIM_PREFIX
        | ids::STR_TRIM_SUFFIX
        | ids::F64_CLASSIFY
        | ids::STR_REPEAT_CHAR
        | ids::STR_TAKE
        | ids::STR_DROP
        | ids::STR_REPLACE_CHAR
        | ids::LIST_JOIN
        | ids::F64_TO_FIXED
        | ids::F64_TO_EXP
        | ids::F64_TO_PRECISION
        | ids::ENV_OS
        | ids::ENV_ARCH
        | ids::RT_BACKTRACE => Intrinsic::Str.type_index(),
        ids::TO_I64
        | ids::STR_LEN
        | ids::STR_CHAR_LEN
        | ids::CHAR_TO_CODE
        | ids::STR_COUNT
        | ids::STR_COUNT_CHAR
        | ids::RANGE_LEN
        | ids::RANGE_INCLUSIVE_LEN
        | ids::COUNT_ONES
        | ids::COUNT_ZEROS
        | ids::LEADING_ZEROS
        | ids::TRAILING_ZEROS
        | ids::LEADING_ONES
        | ids::TRAILING_ONES
        | ids::CHAR_UTF8_LEN
        | ids::CHAR_UTF16_LEN
        | ids::INT_SIGNUM
        | ids::STR_FLOOR_CHAR_BOUNDARY
        | ids::STR_CEIL_CHAR_BOUNDARY
        | ids::TOTAL_CMP
        | ids::F64_RADIX
        | ids::F64_MANTISSA_DIGITS
        | ids::F64_DIGITS
        | ids::F64_MIN_EXP
        | ids::F64_MAX_EXP
        | ids::STR_BYTE_AT
        | ids::F64_MIN_10_EXP
        | ids::F64_MAX_10_EXP
        | ids::STR_BYTE_COUNT
        | ids::I64_MIN
        | ids::I64_MAX
        | ids::INT_BIT_WIDTH
        | ids::I64_BITS
        | ids::I128_BITS
        | ids::ISIZE_BITS
        | ids::U128_BITS
        | ids::USIZE_BITS
        | ids::TIME_UNIX_SECS
        | ids::TIME_UNIX_MILLIS
        | ids::TIME_MONOTONIC_NANOS
        | ids::PROC_PID
        | ids::RT_GC_COLLECT
        | ids::FFI_DLOPEN
        | ids::FFI_DLSYM => Intrinsic::I64.type_index(),
        ids::BYTES_LEN | ids::BYTES_CAP => Intrinsic::Usize.type_index(),
        ids::BYTES_GET => Intrinsic::U8.type_index(),
        ids::U64_MAX | ids::SCALAR_HASH | ids::RNG_NEXT_U64 | ids::BYTES_READ_U64_LE => {
            Intrinsic::U64.type_index()
        }
        ids::I128_MIN | ids::I128_MAX => Intrinsic::I128.type_index(),
        ids::U128_MAX => Intrinsic::U128.type_index(),
        ids::TO_F64
        | ids::SIN
        | ids::COS
        | ids::SQRT
        | ids::FLOOR
        | ids::CEIL
        | ids::ROUND
        | ids::LOG
        | ids::EXP
        | ids::LOG2
        | ids::LOG10
        | ids::TRUNC
        | ids::FRACT
        | ids::COPY_SIGN
        | ids::ATAN2
        | ids::HYPOT
        | ids::TAN
        | ids::ASIN
        | ids::ACOS
        | ids::ATAN
        | ids::SINH
        | ids::COSH
        | ids::TANH
        | ids::CBRT
        | ids::EXP2
        | ids::EXPM1
        | ids::LOG1P
        | ids::SIGNUM
        | ids::RECIP
        | ids::MUL_ADD
        | ids::DIV_EUCLID
        | ids::REM_EUCLID
        | ids::TO_RADIANS
        | ids::TO_DEGREES
        | ids::NEXT_UP
        | ids::NEXT_DOWN
        | ids::MIDPOINT
        | ids::ROUND_TIES_EVEN
        | ids::ASINH
        | ids::ACOSH
        | ids::ATANH
        | ids::FROM_BITS
        | ids::POWI
        | ids::LOG_BASE
        | ids::PI
        | ids::E
        | ids::TAU
        | ids::LN_2
        | ids::LN_10
        | ids::LOG2_E
        | ids::LOG10_E
        | ids::SQRT_2
        | ids::INFINITY
        | ids::NEG_INFINITY
        | ids::NAN
        | ids::EPSILON
        | ids::MIN_POSITIVE
        | ids::FRAC_PI_2
        | ids::FRAC_PI_4
        | ids::FRAC_1_SQRT_2
        | ids::LOG2_10
        | ids::LOG10_2
        | ids::FRAC_1_PI
        | ids::FRAC_2_PI
        | ids::FRAC_2_SQRT_PI
        | ids::FRAC_PI_3
        | ids::FRAC_PI_6
        | ids::FRAC_PI_8
        | ids::F64_MIN
        | ids::F64_MAX
        | ids::GOLDEN_RATIO
        | ids::EULER_GAMMA
        | ids::F64_FROM_BE_BYTES
        | ids::F64_FROM_LE_BYTES
        | ids::F64_FROM_NE_BYTES
        | ids::ALGEBRAIC_ADD
        | ids::ALGEBRAIC_SUB
        | ids::ALGEBRAIC_MUL
        | ids::ALGEBRAIC_DIV
        | ids::ALGEBRAIC_REM
        | ids::F64_NAN_TO
        | ids::F64_ABS_DIFF
        | ids::F64_LDEXP
        | ids::TIME_CPU_SECS => Intrinsic::F64.type_index(),
        _ => Intrinsic::Any.type_index(),
    }
}

// ---------------------------------------------------------------------------
// Match expression type inference
// ---------------------------------------------------------------------------

fn resolve_match_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let children = ast.fixed_children(node_idx);
    // [0] scrutinee  multi = arms
    resolve_types(r, ast, children[0]);
    let scrutinee_type = r.node_types.get(&children[0]).copied();

    let mut bodies = Vec::new();
    for &arm in ast.multi_children(node_idx) {
        let arm_children = ast.fixed_children(arm);
        crate::enums::pattern(r, ast, arm_children[0], scrutinee_type);
        resolve_types_expected(r, ast, arm_children[1], expected);
        if !definitely_returns(ast, arm_children[1])
            && let Some(&ty) = r.node_types.get(&arm_children[1])
            && r.type_pool.as_intrinsic(ty) != Some(Intrinsic::NoReturn)
        {
            bodies.push((arm_children[1], ty));
        }
    }
    if let Some(input) = scrutinee_type {
        if r.type_pool.error_shape(input).ok().flatten().is_some() {
            if !crate::error_coverage::direct_exhaustive(
                r,
                ast,
                ast.multi_children(node_idx),
                input,
            ) {
                crate::errors::report(
                    r,
                    ast,
                    node_idx,
                    "Error match must exhaust both success and error branches; guards and refutable patterns do not prove coverage",
                );
            }
        } else {
            crate::error_coverage::check_match_exhaustiveness(r, ast, node_idx, input);
        }
    }
    let result = expected.unwrap_or_else(|| {
        if bodies.is_empty() {
            Intrinsic::NoReturn.type_index()
        } else {
            join_result_types(r, bodies.iter().map(|&(_, ty)| ty))
        }
    });
    for (body, _) in bodies {
        resolve_types_expected(r, ast, body, Some(result));
        check_expected_type(r, ast, body, result, "match arm");
    }
    Some(result)
}

fn resolve_pattern_literals(
    r: &mut Resolver,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
) {
    if node.is_null() {
        return;
    }
    if let Some(ty) = expected {
        r.node_types.insert(node, ty);
    }
    match ast.node(node).kind {
        NodeKind::PatternError
        | NodeKind::PatternErrorOk
        | NodeKind::PatternTypeFamily
        | NodeKind::PatternAsBind
        | NodeKind::PatternIfGuard
        | NodeKind::PatternNot
        | NodeKind::PatternAndIs => crate::enums::pattern(r, ast, node, expected),
        NodeKind::PatternCall => {
            if !crate::newtype::resolve_pattern(
                r,
                ast,
                node,
                expected,
                true,
                resolve_pattern_literals,
            ) {
                r.diag_ctx
                    .error(
                        "constructor patterns in bindings and parameters require a newtype; enum variants are refutable"
                            .into(),
                    )
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
            }
        }
        NodeKind::PatternTyped => {
            let children = ast.fixed_children(node);
            r.annotation_typed_patterns.insert(node);
            let Some(ann_ty) = resolve_type_expr(r, ast, children[1]) else {
                let span_node = if children[1].is_null() {
                    node
                } else {
                    children[1]
                };
                r.diag_ctx
                    .error("typed pattern requires a valid type".into())
                    .with_primary_span(ast.node(span_node).span)
                    .emit(r.diag_ctx);
                return;
            };
            r.node_types.insert(children[1], ann_ty);
            r.node_types.insert(node, ann_ty);
            if let Some(exp) = expected
                && exp != TypeIndex::INVALID
                && !crate::trait_typing::is_subtype(r, ast, node, exp, ann_ty)
                && !r.type_pool.is_gradually_consistent(exp, ann_ty)
            {
                let expected_name = type_display_name(&r.type_pool, ann_ty);
                let found_name = type_display_name(&r.type_pool, exp);
                r.diag_ctx
                    .error(format!(
                        "type mismatch in typed binding: expected `{expected_name}`, found `{found_name}`"
                    ))
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
            }
            let body_ty = crate::default_methods::parameter_body_type(r, ast, children[1], ann_ty)
                .unwrap_or(ann_ty);
            resolve_pattern_literals(r, ast, children[0], Some(body_ty));
        }
        NodeKind::PatternExtendedCall => {
            crate::structs::resolve_pattern(r, ast, node, expected, true, resolve_pattern_literals);
        }
        NodeKind::Int | NodeKind::Negative => resolve_types_expected(r, ast, node, expected),
        NodeKind::Id => {
            if let Some(ti) = expected {
                propagate_type_to_pattern(r, ast, node, ti);
            }
        }
        NodeKind::PatternTuple => {
            crate::tuples::pattern(r, ast, node, expected, resolve_pattern_literals);
        }
        NodeKind::PatternOr => {
            for &child in ast.fixed_children(node) {
                resolve_pattern_literals(r, ast, child, expected);
            }
        }
        _ => {
            for &child in ast
                .fixed_children(node)
                .iter()
                .chain(ast.multi_children(node))
            {
                resolve_pattern_literals(r, ast, child, None);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Projection type inference (a.b)
// ---------------------------------------------------------------------------

fn resolve_projection_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) -> Option<TypeIndex> {
    if crate::type_factories::bare_value(r, ast, node_idx) {
        return None;
    }
    if let Some(ty) = crate::type_factories::bare_type(r, node_idx) {
        r.node_type_values.insert(node_idx, ty);
        return Some(Intrinsic::Type.type_index());
    }
    if let Some(&symbol) = r.node_symbols.get(&node_idx) {
        let symbol = &r.symbols[symbol.0 as usize];
        if symbol.type_index != TypeIndex::INVALID {
            if matches!(
                symbol.kind,
                SymbolKind::Type | SymbolKind::Trait | SymbolKind::Effect
            ) {
                r.node_type_values.insert(node_idx, symbol.type_index);
                return Some(Intrinsic::Type.type_index());
            }
            return Some(symbol.type_index);
        }
    }
    let children = ast.fixed_children(node_idx);
    resolve_types(r, ast, children[0]);

    // Numeric tuple projections have no member symbol.
    let member_node = children[1];
    if !member_node.is_null() && ast.node(member_node).kind == NodeKind::Int {
        return crate::tuples::projection(r, ast, node_idx);
    }

    // If the member resolves to an enum variant, the type is the parent enum type.
    if !member_node.is_null()
        && let Some(&sym_id) = r.node_symbols.get(&member_node)
    {
        let sym = &r.symbols[sym_id.0 as usize];
        if sym.kind == SymbolKind::EnumVariant {
            return r.node_types.get(&children[0]).copied();
        }
    }

    // Try struct field access: look up field by name in the LHS struct type.
    let lhs_type = r.node_types.get(&children[0]).copied();
    if lhs_type.and_then(|ty| r.type_pool.as_intrinsic(ty)) == Some(Intrinsic::Continuation)
        && str_interner::get(ast.node(member_node).str_id) == "clone"
    {
        return Some(r.register_type(TypeKind::Function {
            params: Vec::new(),
            ret: Intrinsic::Continuation.type_index(),
        }));
    }
    if let Some(lhs_ti) = lhs_type
        && let Some(signature) = crate::optional::unwrap_signature(r, ast, node_idx, lhs_ti)
    {
        return Some(signature);
    }
    if let Some(lhs_ti) = lhs_type
        && let Some(signature) = crate::errors::unwrap_signature(r, ast, node_idx, lhs_ti)
    {
        return Some(signature);
    }
    if let Some(lhs_ti) = lhs_type {
        crate::associated::validate_instance_member(r, ast, node_idx, lhs_ti);
        if let Some(kind) = crate::collections::CollectionKind::from_type(&r.type_pool, lhs_ti) {
            return crate::collections::method_signature(r, ast, node_idx, lhs_ti, kind);
        }
        if r.type_pool
            .canonical_type(lhs_ti)
            .is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::Enum { .. }))
        {
            return crate::enums::method_signature(r, ast, node_idx, lhs_ti);
        }
    }
    if let Some(lhs_ti) = lhs_type.and_then(|ty| r.type_pool.canonical_type(ty))
        && !r.type_pool.is_reserved_collection_role(lhs_ti)
        && let type_pool::TypeKind::Struct { fields, .. } = &r.type_pool.get(lhs_ti).kind.clone()
        && !member_node.is_null()
    {
        let field_name = ast.node(member_node).str_id;
        for (idx, field) in fields.iter().enumerate() {
            if field.name == field_name {
                r.node_field_indices.insert(node_idx, idx as u32);
                return Some(field.ty);
            }
        }
    }

    if let Some(lhs_ti) = lhs_type.and_then(|ty| r.type_pool.canonical_type(ty)) {
        let name = ast.node(member_node).str_id;
        if crate::associated::member(r, lhs_ti, name).is_err()
            && let Some(signature) =
                crate::display_derivation::declared_signature(r, ast, lhs_ti, name)
                    .or_else(|| {
                        matches!(
                            r.type_pool.get(lhs_ti).kind,
                            TypeKind::Newtype { .. } | TypeKind::Tuple { .. }
                        )
                        .then(|| {
                            crate::comparison_derivation::declared_method_signature(
                                r, ast, lhs_ti, name,
                            )
                        })
                        .flatten()
                    })
                    .or_else(|| crate::hash_derivation::declared_signature(r, ast, lhs_ti, name))
                    .or_else(|| crate::json_derivation::declared_signature(r, ast, lhs_ti, name))
        {
            return Some(signature);
        }
        let intrinsic = r.type_pool.as_intrinsic(lhs_ti);
        let has_source_method = r
            .scopes
            .iter()
            .any(|scope| scope.assoc_type == Some(lhs_ti) && scope.bindings.contains_key(&name));
        let has_default = match crate::default_methods::source_member(
            r,
            ast,
            lhs_ti,
            name,
            r.node_scopes
                .get(&node_idx)
                .copied()
                .unwrap_or(r.current_scope),
        ) {
            Ok(member) => member.is_some(),
            Err(error) => {
                r.diag_ctx
                    .error(error)
                    .with_primary_span(ast.node(node_idx).span)
                    .emit(r.diag_ctx);
                return None;
            }
        };
        if has_source_method
            || has_default
            || r.type_pool.is_reserved_collection_role(lhs_ti)
            || matches!(
                r.type_pool.get(lhs_ti).kind,
                TypeKind::Trait { .. } | TypeKind::Newtype { .. }
            )
            || intrinsic.is_some_and(|kind| kind != Intrinsic::Any)
        {
            return crate::methods::projection(r, ast, node_idx, lhs_ti);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// If a pattern is a simple identifier, set its symbol's type_index.
fn propagate_type_to_pattern(r: &mut Resolver, ast: &Ast, pat: NodeIndex, ty: TypeIndex) {
    if pat.is_null() {
        return;
    }
    if ast.node(pat).kind == NodeKind::Id
        && let Some(&sym_id) = r.node_symbols.get(&pat)
    {
        r.symbols[sym_id.0 as usize].type_index = ty;
    }
}

/// Get a human-readable name for a type, for use in diagnostic messages.
pub(crate) fn type_display_name(pool: &type_pool::TypePool, ti: TypeIndex) -> String {
    if ti == TypeIndex::INVALID {
        return "<unknown>".to_string();
    }
    let info = pool.get(ti);
    match &info.kind {
        TypeKind::Intrinsic(intr) => intr.name().to_string(),
        TypeKind::Struct { name, .. } => str_interner::get(*name),
        TypeKind::Enum { name, .. } => str_interner::get(*name),
        TypeKind::Typealias { name, .. } => str_interner::get(*name),
        TypeKind::Newtype { name, .. } => str_interner::get(*name),
        TypeKind::Optional { inner } => format!("?{}", type_display_name(pool, *inner)),
        TypeKind::Future { inner } => format!("Future({})", type_display_name(pool, *inner)),
        TypeKind::Tuple { elements } => {
            let parts: Vec<String> = elements
                .iter()
                .map(|e| type_display_name(pool, *e))
                .collect();
            format!("({})", parts.join(", "))
        }
        TypeKind::Function { params, ret } => {
            let ps: Vec<String> = params.iter().map(|p| type_display_name(pool, *p)).collect();
            format!("fn({}) -> {}", ps.join(", "), type_display_name(pool, *ret))
        }
        TypeKind::Effect {
            params,
            ret,
            is_async,
        } => {
            let ps: Vec<String> = params.iter().map(|p| type_display_name(pool, *p)).collect();
            let prefix = if *is_async { "async effect" } else { "effect" };
            format!(
                "{prefix}({}) -> {}",
                ps.join(", "),
                type_display_name(pool, *ret)
            )
        }
        TypeKind::Module { name } => format!("module {}", str_interner::get(*name)),
        TypeKind::Trait { name, .. } => format!("trait {}", str_interner::get(*name)),
        TypeKind::ErrorQualified { errors, inner } => {
            let errs: Vec<String> = errors.iter().map(|e| type_display_name(pool, *e)).collect();
            if errs.len() == 1 {
                format!("!{} {}", errs[0], type_display_name(pool, *inner))
            } else {
                format!("![{}] {}", errs.join(", "), type_display_name(pool, *inner))
            }
        }
        TypeKind::EffectQualified { effects, inner } => {
            let effs: Vec<String> = effects
                .iter()
                .map(|e| type_display_name(pool, *e))
                .collect();
            if effs.len() == 1 {
                format!("#{} {}", effs[0], type_display_name(pool, *inner))
            } else {
                format!("#[{}] {}", effs.join(", "), type_display_name(pool, *inner))
            }
        }
        _ => format!("TypeIndex({})", ti.as_u32()),
    }
}

/// Choose the type for an unsuffixed integer literal.
/// If the expected type is an integer type, use it; otherwise default to i64.
fn contextual_int_type(pool: &type_pool::TypePool, expected: Option<TypeIndex>) -> TypeIndex {
    if let Some(exp) = expected
        && let Some(intr) = pool.as_intrinsic(exp)
        && intr.is_integer()
    {
        return exp;
    }
    Intrinsic::I64.type_index()
}

fn resolve_integer_literal(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
    negative: bool,
) -> TypeIndex {
    let expected = payload_inner_expected(&r.type_pool, expected);
    let ti = contextual_int_type(&r.type_pool, expected);
    let node = ast.node(node_idx);
    let text = str_interner::get(node.str_id);
    let valid = ast::literal::integer_magnitude(&text).is_ok_and(|magnitude| {
        r.type_pool
            .as_intrinsic(ti)
            .is_some_and(|kind| kind.contains_integer_literal(magnitude, negative))
    });
    if !valid {
        let name = type_display_name(&r.type_pool, ti);
        let sign = if negative { "-" } else { "" };
        r.diag_ctx
            .error(format!(
                "integer literal `{sign}{text}` is out of range for `{name}`"
            ))
            .with_primary_span(node.span)
            .emit(r.diag_ctx);
    }
    ti
}

/// Choose the type for an unsuffixed float literal.
/// If the expected type is f32, use it; otherwise default to f64.
fn contextual_float_type(pool: &type_pool::TypePool, expected: Option<TypeIndex>) -> TypeIndex {
    let expected = payload_inner_expected(pool, expected);
    if let Some(exp) = expected
        && let Some(intr) = pool.as_intrinsic(exp)
        && matches!(intr, Intrinsic::F32 | Intrinsic::F64)
    {
        return exp;
    }
    Intrinsic::F64.type_index()
}

fn payload_inner_expected(
    pool: &type_pool::TypePool,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let expected = expected?;
    let canonical = pool.canonical_type(expected)?;
    match pool.get(canonical).kind {
        TypeKind::Optional { inner } => Some(inner),
        TypeKind::ErrorQualified { inner, .. } => payload_inner_expected(pool, Some(inner)),
        _ => Some(expected),
    }
}

/// Native registration deliberately carries no language signature. Every native
/// value needs a source annotation; raw direct calls still use the dynamic ABI.
pub(crate) fn validate_native_values(r: &Resolver, ast: &Ast) {
    let callees: std::collections::HashSet<_> = ast
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Call)
        .map(|node| node.children[0])
        .collect();
    let mut bindings: Vec<_> = r
        .node_symbols
        .iter()
        .map(|(&node, &symbol)| (node, symbol))
        .collect();
    bindings.sort_by_key(|(node, _)| node.0);
    for (node, symbol) in bindings {
        if !r.builtin_fns.contains_key(&symbol)
            || callees.contains(&node)
            || ast.node(node).kind == NodeKind::InfixFnCall
        {
            continue;
        }
        let ty = r
            .node_types
            .get(&node)
            .copied()
            .unwrap_or(r.symbols[symbol.0 as usize].type_index);
        if r.type_pool
            .canonical_type(ty)
            .is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::Function { .. }))
        {
            continue;
        }
        r.diag_ctx
            .error("native function values require a function type annotation".into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
}

/// Enforce `[ffi] libraries = [...]` allowlist constraints on `.__ffi_dlopen'builtin`
/// views and any non-public `const` bindings aliasing them, ensuring restricted
/// `dlopen` capabilities cannot escape or open unlisted shared libraries.
pub(crate) fn validate_restricted_ffi_dlopen(r: &Resolver, ast: &Ast) {
    if r.restricted_dlopen_views.is_empty() {
        return;
    }
    let mut bound_views = std::collections::HashSet::new();
    let mut restricted_symbols: std::collections::HashMap<
        crate::SymbolId,
        (NodeIndex, &std::collections::HashSet<String>),
    > = std::collections::HashMap::new();

    for (idx, node) in ast.nodes.iter().enumerate() {
        if node.kind != NodeKind::ConstDecl {
            continue;
        }
        let decl_idx = NodeIndex(idx as u32);
        let children = ast.fixed_children(decl_idx);
        if children.len() <= 2
            || (!children
                .get(3)
                .copied()
                .unwrap_or(NodeIndex::NULL)
                .is_null())
        {
            continue;
        }
        let pat = children[0];
        let rhs = children[2];
        let Some(allowlist) = r.restricted_dlopen_views.get(&rhs) else {
            continue;
        };
        if pat.is_null() || ast.node(pat).kind != NodeKind::Id {
            continue;
        }
        let Some(&sym_id) = r.node_symbols.get(&pat) else {
            continue;
        };
        if r.symbols[sym_id.0 as usize].visibility == crate::Visibility::Public {
            r.diag_ctx
                .error("restricted `__ffi_dlopen` binding cannot have `pub` visibility".to_string())
                .with_primary_span(ast.node(decl_idx).span)
                .emit(r.diag_ctx);
        }
        bound_views.insert(rhs);
        restricted_symbols.insert(sym_id, (pat, allowlist));
    }

    let mut called_views = std::collections::HashSet::new();
    let mut called_id_nodes = std::collections::HashSet::new();

    for (idx, node) in ast.nodes.iter().enumerate() {
        if node.kind != NodeKind::Call {
            continue;
        }
        let call_idx = NodeIndex(idx as u32);
        let callee = node.children[0];
        let allowlist = if let Some(allowlist) = r.restricted_dlopen_views.get(&callee) {
            called_views.insert(callee);
            Some(allowlist)
        } else if !callee.is_null()
            && ast.node(callee).kind == NodeKind::Id
            && let Some(&sym_id) = r.node_symbols.get(&callee)
            && let Some(&(_, allowlist)) = restricted_symbols.get(&sym_id)
        {
            called_id_nodes.insert(callee);
            Some(allowlist)
        } else {
            None
        };
        let Some(allowlist) = allowlist else {
            continue;
        };
        let args = ast.multi_children(call_idx);
        let literal_lib = (args.len() == 1)
            .then(|| unescape_ast_str_literal(ast, args[0]))
            .flatten();
        match literal_lib {
            Some(lib_name) if allowlist.contains(&lib_name) => {}
            Some(lib_name) => {
                r.diag_ctx
                    .error(format!(
                        "FFI library `{lib_name}` is not permitted by `[ffi] libraries` allowlist in package.toml"
                    ))
                    .with_primary_span(ast.node(args[0]).span)
                    .emit(r.diag_ctx);
            }
            None => {
                r.diag_ctx
                    .error(
                        "restricted `__ffi_dlopen` requires a string literal library name from `[ffi] libraries`"
                            .to_string(),
                    )
                    .with_primary_span(ast.node(call_idx).span)
                    .emit(r.diag_ctx);
            }
        }
    }

    let mut view_nodes: Vec<_> = r.restricted_dlopen_views.keys().copied().collect();
    view_nodes.sort_by_key(|n| n.0);
    for view_node in view_nodes {
        if !bound_views.contains(&view_node) && !called_views.contains(&view_node) {
            r.diag_ctx
                .error(
                    "restricted `__ffi_dlopen` must be called directly with a literal library name from `[ffi] libraries`"
                        .to_string(),
                )
                .with_primary_span(ast.node(view_node).span)
                .emit(r.diag_ctx);
        }
    }

    let mut bindings: Vec<_> = r.node_symbols.iter().map(|(&n, &s)| (n, s)).collect();
    bindings.sort_by_key(|(n, _)| n.0);
    for (node, sym_id) in bindings {
        let Some(&(pat_node, _)) = restricted_symbols.get(&sym_id) else {
            continue;
        };
        if node == pat_node || ast.node(node).kind != NodeKind::Id {
            continue;
        }
        if !called_id_nodes.contains(&node) {
            r.diag_ctx
                .error(
                    "restricted `__ffi_dlopen` binding cannot escape; call it directly with a literal library name"
                        .to_string(),
                )
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
        }
    }
}

fn unescape_ast_str_literal(ast: &Ast, node: NodeIndex) -> Option<String> {
    if node.is_null() || ast.node(node).kind != NodeKind::Str {
        return None;
    }
    let raw = str_interner::get(ast.node(node).str_id);
    let inner = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        &raw[1..raw.len() - 1]
    } else {
        raw.as_str()
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '{' | '}') && chars.peek() == Some(&c) {
            chars.next();
            out.push(c);
        } else if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('x') => {
                    let h1 = chars.next()?;
                    let h2 = chars.next()?;
                    let hex = format!("{h1}{h2}");
                    let v = u8::from_str_radix(&hex, 16).ok()?;
                    out.push(v as char);
                }
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() != 4 {
                        return None;
                    }
                    let v = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(v)?);
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

#[cfg(test)]
mod cast_tests {
    use ast::{NodeIndex, NodeKind};
    use diagnostic::{DiagnosticContext, Level};
    use rustc_span::{
        FileName,
        source_map::{FilePathMapping, SourceMap},
    };
    use type_pool::Intrinsic;

    fn resolve_cast(source: &str) -> (crate::ResolvedAst, NodeIndex, Vec<diagnostic::Diagnostic>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let source_map = SourceMap::new(FilePathMapping::empty());
        let file =
            source_map.new_source_file(FileName::Custom("cast.ns".into()), source.to_owned());
        let diagnostics = DiagnosticContext::new(&source_map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let cast = ast
            .nodes
            .iter()
            .position(|node| node.kind == NodeKind::TypeCast)
            .unwrap();
        let resolved = crate::resolve(ast, &diagnostics);
        (
            resolved,
            NodeIndex(cast as u32),
            diagnostics.diagnostics().to_vec(),
        )
    }

    #[test]
    fn explicit_cast_records_destination_without_retyping_operand() {
        for (source, destination) in [
            ("const x = 42.as(f64)", Intrinsic::F64),
            ("const x = 300.as(i8)", Intrinsic::I8),
            ("const x = 42.as(u128)", Intrinsic::U128),
        ] {
            let (resolved, cast, diagnostics) = resolve_cast(source);
            assert!(
                !diagnostics.iter().any(|diag| diag.level == Level::Error),
                "{:?}",
                diagnostics
            );
            let children = resolved.ast.fixed_children(cast);
            assert_eq!(resolved.node_types[&cast], destination.type_index());
            assert_eq!(resolved.node_types[&children[1]], destination.type_index());
            assert_eq!(
                resolved.node_types[&children[0]],
                Intrinsic::I64.type_index()
            );
        }
    }

    #[test]
    fn invalid_or_missing_cast_targets_are_diagnosed() {
        for source in [
            "const x = 42.as(Unknown)",
            "const x = 42.as(true)",
            "const x = 42.as()",
        ] {
            let (resolved, cast, diagnostics) = resolve_cast(source);
            assert!(
                diagnostics.iter().any(|diag| diag.level == Level::Error
                    && diag.message == "cast target must be a valid type"),
                "{:?}",
                diagnostics
            );
            assert!(!resolved.node_types.contains_key(&cast));
        }
    }
}

#[cfg(test)]
mod coercion_tests {
    use ast::{NodeIndex, NodeKind};
    use diagnostic::{Diagnostic, DiagnosticContext, Level};
    use rustc_span::{
        FileName,
        source_map::{FilePathMapping, SourceMap},
    };
    use type_pool::Intrinsic;

    use crate::{CoercionKind, ResolvedAst};

    fn resolve_source(source: &str) -> (ResolvedAst, Vec<Diagnostic>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let source_map = SourceMap::new(FilePathMapping::empty());
        let file =
            source_map.new_source_file(FileName::Custom("coercion.ns".into()), source.to_owned());
        let diagnostics = DiagnosticContext::new(&source_map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        (resolved, diagnostics.diagnostics().to_vec())
    }

    #[test]
    fn native_values_require_signatures_but_direct_calls_stay_dynamic() {
        for source in [
            "fn main() { let f = print; f(42) }",
            "fn main() { let f: Any = print; f(42) }",
        ] {
            let (_, diagnostics) = resolve_source(source);
            assert!(
                diagnostics.iter().any(|diagnostic| diagnostic.message
                    == "native function values require a function type annotation"),
                "{source}: {diagnostics:?}"
            );
        }
        for source in [
            "fn main() { print(42) }",
            "fn main() { let f: fn(Any) -> Unit = print; f(42) }",
            "typealias Printer = fn(Any) -> Unit\nfn main() { let f: Printer = print; f(42) }",
        ] {
            let (_, diagnostics) = resolve_source(source);
            assert!(
                !diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.level == Level::Error),
                "{source}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn static_returns_assignments_and_conditions_reject_incompatible_values() {
        for source in [
            "fn main() -> i64 { return true; 42 }",
            "fn main() -> i8 { return 128; 42 }",
            "fn main() -> i64 { if true { return false }; 42 }",
            "fn main() -> i64 { if true { 42 } else { false } }",
            "fn main() { var x: i64 = 1; x = true; }",
            "fn main() { if 1 { print(42) }; }",
            "fn main() { while 1 { print(42) }; }",
            "fn main() -> i64 { let f = |x| -> bool { return 42 }; 42 }",
        ] {
            let (_, diagnostics) = resolve_source(source);
            assert!(
                diagnostics.iter().any(|diag| diag.level == Level::Error),
                "accepted {source}"
            );
        }
        let (_, diagnostics) = resolve_source(
            "fn main() -> i64 { let f = |x| -> bool { return true }; return 42; 42 }",
        );
        assert!(
            !diagnostics.iter().any(|diag| diag.level == Level::Error),
            "nested lambda changed outer return context: {diagnostics:?}"
        );
    }

    #[test]
    fn any_boundaries_attach_assertions_to_actual_values_once() {
        for (source, target, count) in [
            (
                "fn consume(x: i64) -> i64 { x }\nfn main() { let x: Any = true; consume(x) }",
                Intrinsic::I64,
                1,
            ),
            ("fn main(x: Any) -> i64 { x }", Intrinsic::I64, 1),
            ("fn main(x: Any) -> i64 { return x; 42 }", Intrinsic::I64, 1),
            ("fn main(x: Any) { let y: i64 = x; }", Intrinsic::I64, 1),
            (
                "fn main(x: Any) { var y: i64 = 0; y = x; }",
                Intrinsic::I64,
                1,
            ),
            (
                "fn main(x: Any) { if x { print(42) }; while x { print(42) }; }",
                Intrinsic::Bool,
                2,
            ),
            (
                "fn main(x: Any) -> bool { return true if x; true }",
                Intrinsic::Bool,
                1,
            ),
            ("fn main(x: Any) -> bool { x and true }", Intrinsic::Bool, 1),
        ] {
            let (resolved, diagnostics) = resolve_source(source);
            assert!(
                !diagnostics.iter().any(|diag| diag.level == Level::Error),
                "{source}: {diagnostics:?}"
            );
            let assertions: Vec<_> = resolved
                .node_coercions
                .iter()
                .filter(|(_, coercion)| coercion.kind == CoercionKind::Assert)
                .collect();
            assert_eq!(assertions.len(), count, "{source}");
            for (&node, coercion) in assertions {
                assert_eq!(coercion.target, target.type_index());
                assert_eq!(resolved.ast.node(node).kind, NodeKind::Id);
                assert_eq!(resolved.node_types[&node], Intrinsic::Any.type_index());
            }
        }
    }

    #[test]
    fn widening_and_narrow_literal_identity_are_explicit_conversions() {
        let (resolved, diagnostics) =
            resolve_source("fn widen(x: i64) -> f64 { x }\nfn narrow() -> i8 { print(1); -128 }");
        assert!(
            !diagnostics.iter().any(|diag| diag.level == Level::Error),
            "{diagnostics:?}"
        );
        let widened = resolved
            .node_coercions
            .iter()
            .find(|&(&node, _)| resolved.ast.node(node).kind == NodeKind::Id)
            .unwrap();
        assert_eq!(widened.1.target, Intrinsic::F64.type_index());
        assert_eq!(widened.1.kind, CoercionKind::Convert);
        let negative = resolved
            .ast
            .nodes
            .iter()
            .position(|node| node.kind == NodeKind::Negative)
            .unwrap();
        let coercion = resolved.node_coercions[&NodeIndex(negative as u32)];
        assert_eq!(coercion.target, Intrinsic::I8.type_index());
        assert_eq!(coercion.kind, CoercionKind::Convert);
        for &node in resolved.node_coercions.keys() {
            assert!(!matches!(
                resolved.ast.node(node).kind,
                NodeKind::Block
                    | NodeKind::ExprStatement
                    | NodeKind::ReturnStatement
                    | NodeKind::NamedArg
            ));
        }
    }

    #[test]
    fn dynamic_math_results_have_any_type_and_require_destination_assertions() {
        for source in [
            "fn main() { let x: f64 = abs(1); }",
            "fn main() { let x: f64 = pow(2, 3); }",
        ] {
            let (resolved, diagnostics) = resolve_source(source);
            assert!(
                !diagnostics.iter().any(|diag| diag.level == Level::Error),
                "{diagnostics:?}"
            );
            let call = resolved
                .ast
                .nodes
                .iter()
                .position(|node| node.kind == NodeKind::Call)
                .unwrap();
            let node = NodeIndex(call as u32);
            assert_eq!(resolved.node_types[&node], Intrinsic::Any.type_index());
            assert_eq!(resolved.node_coercions[&node].kind, CoercionKind::Assert);
            assert_eq!(
                resolved.node_coercions[&node].target,
                Intrinsic::F64.type_index()
            );
        }
    }

    #[test]
    fn dynamically_typed_callees_keep_any_result_and_return_boundary() {
        let (resolved, diagnostics) = resolve_source("fn main(f: Any) -> i64 { f() }");
        assert!(
            !diagnostics.iter().any(|diag| diag.level == Level::Error),
            "{diagnostics:?}"
        );
        let call = resolved
            .ast
            .nodes
            .iter()
            .position(|node| node.kind == NodeKind::Call)
            .unwrap();
        let node = NodeIndex(call as u32);
        assert_eq!(resolved.node_types[&node], Intrinsic::Any.type_index());
        assert_eq!(resolved.node_coercions[&node].kind, CoercionKind::Assert);
        assert_eq!(
            resolved.node_coercions[&node].target,
            Intrinsic::I64.type_index()
        );
    }

    fn return_type(resolved: &ResolvedAst, name: &str) -> type_pool::TypeIndex {
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == name)
            .unwrap();
        let type_pool::TypeKind::Function { ret, .. } =
            resolved.type_pool.get(symbol.type_index).kind
        else {
            panic!("function must have a complete signature");
        };
        ret
    }

    #[test]
    fn associated_globals_prepare_types_before_function_return_inference() {
        for source in [
            "struct Holder { pub fn answer() { value }\nglobal value: i64 = 42 }",
            "enum Holder { ready, pub fn answer() { value }; global value: i64 = 42 }",
            "fn answer() { Holder.value }\nstruct Holder { pub global value: i64 = 42 }",
            "fn answer() { Holder.value }\nenum Holder { ready, pub global value: i64 = 42 }",
        ] {
            let (resolved, diagnostics) = resolve_source(source);
            assert!(
                !diagnostics.iter().any(|diag| diag.level == Level::Error),
                "{source}: {diagnostics:?}"
            );
            assert_eq!(
                return_type(&resolved, "answer"),
                Intrinsic::I64.type_index(),
                "{source}"
            );
            let global = resolved
                .symbols
                .iter()
                .find(|symbol| str_interner::get(symbol.name) == "value")
                .unwrap();
            assert_eq!(global.type_index, Intrinsic::I64.type_index(), "{source}");
        }
    }

    #[test]
    fn unannotated_signatures_preserve_any_boundaries_in_both_definition_orders() {
        for source in [
            "fn id(x: Any) { x }\nfn main() { let y: i64 = id(true); }",
            "fn main() { let y: i64 = id(true); }\nfn id(x: Any) { x }",
        ] {
            let (resolved, diagnostics) = resolve_source(source);
            assert!(
                !diagnostics.iter().any(|diag| diag.level == Level::Error),
                "{diagnostics:?}"
            );
            assert_eq!(return_type(&resolved, "id"), Intrinsic::Any.type_index());
            let call = resolved
                .ast
                .nodes
                .iter()
                .position(|node| node.kind == NodeKind::Call)
                .unwrap();
            let node = NodeIndex(call as u32);
            assert_eq!(resolved.node_types[&node], Intrinsic::Any.type_index());
            assert_eq!(resolved.node_coercions[&node].kind, CoercionKind::Assert);
            assert_eq!(
                resolved.node_coercions[&node].target,
                Intrinsic::I64.type_index()
            );
        }
    }

    #[test]
    fn return_inference_joins_all_early_returns_and_reachable_tail_values() {
        for (source, name, expected) in [
            (
                "fn main(b: bool) { return true if b; 42 }",
                "main",
                Intrinsic::Any,
            ),
            (
                "fn main(b: bool) { if b { return true }; 42 }",
                "main",
                Intrinsic::Any,
            ),
            ("fn main() { return true; 42 }", "main", Intrinsic::Bool),
            (
                "fn main() { return 42; let x = 0; }",
                "main",
                Intrinsic::I64,
            ),
            (
                "fn main(b: bool) { if b { 42 } else { 0.5 } }",
                "main",
                Intrinsic::F64,
            ),
            ("fn main(b: bool) { if b { 42 } }", "main", Intrinsic::Any),
            (
                "fn main() { let f = |b: bool| { return true if b; 42 }; }",
                "f",
                Intrinsic::Any,
            ),
        ] {
            let (resolved, diagnostics) = resolve_source(source);
            assert!(
                !diagnostics.iter().any(|diag| diag.level == Level::Error),
                "{source}: {diagnostics:?}"
            );
            assert_eq!(
                return_type(&resolved, name),
                expected.type_index(),
                "{source}"
            );
        }
    }

    #[test]
    fn inferred_numeric_returns_convert_every_value_path_to_the_common_type() {
        let (resolved, diagnostics) = resolve_source("fn main(b: bool) { return 2 if b; 0.5 }");
        assert!(
            !diagnostics.iter().any(|diag| diag.level == Level::Error),
            "{diagnostics:?}"
        );
        assert_eq!(return_type(&resolved, "main"), Intrinsic::F64.type_index());
        let integer = resolved
            .ast
            .nodes
            .iter()
            .position(|node| node.kind == NodeKind::Int)
            .unwrap();
        let node = NodeIndex(integer as u32);
        assert_eq!(resolved.node_types[&node], Intrinsic::I64.type_index());
        assert_eq!(resolved.node_coercions[&node].kind, CoercionKind::Convert);
        assert_eq!(
            resolved.node_coercions[&node].target,
            Intrinsic::F64.type_index()
        );
    }

    #[test]
    fn annotated_tail_if_requires_an_else_value() {
        let (_, diagnostics) = resolve_source("fn main(b: bool) -> i64 { if b { 42 } }");
        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.level == Level::Error && diag.message.contains("without else")),
            "{diagnostics:?}"
        );
    }
}
