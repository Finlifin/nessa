//! Expression lowering — converts AST expression nodes to NIR values.

use ast::{Ast, NodeIndex, NodeKind};
use nsbc::FuncId;
use resolution::{ResolvedAst, SymbolKind};

use crate::builder::FunctionBuilder;
use crate::{BinOp, BlockId, NirExpr, NirLocal, NirParam, NirStmt, NirValue, Terminator, UnaryOp};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Lower an AST expression node into a [`NirValue`], emitting any necessary
/// statements into `block`.
pub(crate) fn lower_expr(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if node_idx.is_null() {
        return NirValue::Unit;
    }
    let ast = &resolved.ast;
    let node = ast.node(node_idx);
    match node.kind {
        // ── Literals ───────────────────────────────────────────────
        NodeKind::Int => lower_int_literal(node),
        NodeKind::Real => lower_real_literal(node),
        NodeKind::Bool => lower_bool_literal(node),
        NodeKind::Str => lower_str_literal(node),
        NodeKind::FStringConcat => lower_fstring_concat(resolved, ast, node_idx, builder, block),
        NodeKind::Char => lower_char_literal(node),
        NodeKind::Null => NirValue::Null,
        NodeKind::Unit => NirValue::Unit,

        // ── Identifier ─────────────────────────────────────────────
        NodeKind::Id => lower_id(resolved, node_idx, builder),

        // ── Arithmetic ─────────────────────────────────────────────
        NodeKind::Add | NodeKind::Sub | NodeKind::Mul | NodeKind::Div | NodeKind::Mod => {
            lower_arith(resolved, ast, node_idx, node.kind, builder, block)
        }

        // ── String concat ──────────────────────────────────────────
        NodeKind::Concat => {
            let children = ast.fixed_children(node_idx);
            let lhs = lower_expr(resolved, children[0], builder, block);
            let rhs = lower_expr(resolved, children[1], builder, block);
            let local = builder.alloc_local();
            let concat_name = str_interner::intern("concat");
            builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                local,
                NirExpr::MethodCall(lhs, concat_name, vec![rhs]),
            ));
            NirValue::Local(local)
        }

        // ── Comparison / Boolean ───────────────────────────────────
        NodeKind::BoolEq
        | NodeKind::BoolNotEq
        | NodeKind::BoolGt
        | NodeKind::BoolGtEq
        | NodeKind::BoolLt
        | NodeKind::BoolLtEq => {
            lower_comparison(resolved, ast, node_idx, node.kind, builder, block)
        }
        NodeKind::BoolAnd | NodeKind::BoolOr => {
            lower_logical(resolved, ast, node_idx, node.kind, builder, block)
        }

        // ── Unary ──────────────────────────────────────────────────
        NodeKind::Negative => lower_unary(resolved, ast, node_idx, UnaryOp::Neg, builder, block),
        NodeKind::BoolNot => lower_unary(resolved, ast, node_idx, UnaryOp::Not, builder, block),

        // ── Call ───────────────────────────────────────────────────
        NodeKind::Call => lower_call(resolved, ast, node_idx, builder, block),

        // ── Struct / enum construction: TypeName { field: val, ... }
        NodeKind::ExtendedCall => lower_extended_call(resolved, ast, node_idx, builder, block),

        // ── Projection (field access / enum variant construction) ──
        NodeKind::Projection => lower_projection(resolved, ast, node_idx, builder, block),

        // ── Block ──────────────────────────────────────────────────
        NodeKind::Block => lower_block_expr(resolved, ast, node_idx, builder, block),

        // ── Assignment ─────────────────────────────────────────────
        NodeKind::Assign => {
            let children = ast.fixed_children(node_idx);
            let lhs_node = ast.node(children[0]);

            // obj(args) = val → obj.update(args..., val) desugaring.
            if lhs_node.kind == NodeKind::Call {
                let val = lower_expr(resolved, children[1], builder, block);
                let call_children = ast.fixed_children(children[0]);
                let callee = call_children[0];
                let call_args = ast.multi_children(children[0]);

                let obj = lower_expr(resolved, callee, builder, block);
                let mut args: Vec<NirValue> = call_args
                    .iter()
                    .map(|&a| lower_expr(resolved, a, builder, block))
                    .collect();
                args.push(val);

                let update_str = str_interner::intern("update");
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::MethodCall(obj, update_str, args),
                ));
                return NirValue::Unit;
            }

            let val = lower_expr(resolved, children[1], builder, block);
            // If LHS is a resolved local, assign to it.
            if let Some(local) = resolve_local(resolved, children[0], builder) {
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::Use(val)));
            }
            NirValue::Unit
        }

        // ── Match expression ────────────────────────────────────────
        NodeKind::PostMatch => lower_match(resolved, ast, node_idx, builder, block),

        // ── Lambda ─────────────────────────────────────────────────
        NodeKind::Lambda => lower_lambda(resolved, ast, node_idx, builder, block),

        // ── Named argument — lower value only ──────────────────────
        NodeKind::NamedArg => {
            let children = ast.fixed_children(node_idx);
            // children[0] = parameter name (Id) — skip
            // children[1] = value expression — lower
            lower_expr(resolved, children[1], builder, block)
        }

        // ── Default: produce unit ──────────────────────────────────
        _ => NirValue::Unit,
    }
}

// ---------------------------------------------------------------------------
// Literal lowering
// ---------------------------------------------------------------------------

/// Check if an AST node has a meaningful str_id set by the parser.
/// Since the parser now interns all token text, leaf nodes always have valid str_ids.
fn has_str_id(_node: &ast::Node) -> bool {
    true
}

fn lower_int_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        let text = str_interner::get(node.str_id);
        let val: i64 = text.parse().unwrap_or(0);
        NirValue::ConstInt(val)
    } else {
        // Literal value unavailable — produce 0 as placeholder.
        NirValue::ConstInt(0)
    }
}

fn lower_real_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        let text = str_interner::get(node.str_id);
        let val: f64 = text.parse().unwrap_or(0.0);
        NirValue::ConstFloat(val)
    } else {
        NirValue::ConstFloat(0.0)
    }
}

fn lower_bool_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        let text = str_interner::get(node.str_id);
        NirValue::ConstBool(text == "true")
    } else {
        NirValue::ConstBool(true)
    }
}

fn lower_str_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        NirValue::ConstStr(node.str_id)
    } else {
        NirValue::Unit
    }
}

/// Lower an f-string (FStringConcat) node.
///
/// Strategy:
///   - `Str` parts → lowered as string constants (already have type String)
///   - non-`Str` parts → call `to_string()` method on the value
///   - chain all parts with `StrConcat` intrinsic calls left-to-right
///   - if there are zero parts, produce an empty string constant
fn lower_fstring_concat(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    use runtime::ids;

    let parts = ast.multi_children(node_idx);
    if parts.is_empty() {
        // Empty string constant.
        let empty_id = str_interner::intern("\"\"");
        return NirValue::ConstStr(empty_id);
    }

    // Lower each part to a String-typed NirValue.
    let to_string_name = str_interner::intern("to_string");
    let mut string_vals: Vec<NirValue> = Vec::with_capacity(parts.len());

    for &part in parts {
        if part.is_null() {
            continue;
        }
        let part_node = ast.node(part);
        if part_node.kind == NodeKind::Str {
            // Literal segment — already a String constant.
            string_vals.push(lower_str_literal(part_node));
        } else {
            // Interpolated expression — lower then call to_string().
            let val = lower_expr(resolved, part, builder, block);
            let local = builder.alloc_local();
            builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                local,
                NirExpr::MethodCall(val, to_string_name, Vec::new()),
            ));
            string_vals.push(NirValue::Local(local));
        }
    }

    // Fold left with str_concat: result = concat(concat(a, b), c) ...
    let mut acc = string_vals[0].clone();
    for part_val in string_vals.into_iter().skip(1) {
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::CallBuiltin(ids::STR_CONCAT, vec![acc, part_val]),
        ));
        acc = NirValue::Local(local);
    }
    acc
}

fn lower_char_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        NirValue::ConstStr(node.str_id)
    } else {
        NirValue::Unit
    }
}

// ---------------------------------------------------------------------------
// Identifier lowering
// ---------------------------------------------------------------------------

fn lower_id(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
) -> NirValue {
    if let Some(local) = resolve_local(resolved, node_idx, builder) {
        NirValue::Local(local)
    } else {
        // Unresolved identifier — produce unit to avoid crashes.
        NirValue::Unit
    }
}

/// Map a resolved symbol to a NirLocal, allocating if needed.
fn resolve_local(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
) -> Option<NirLocal> {
    resolved
        .node_symbols
        .get(&node_idx)
        .map(|sym_id| builder.local_for_symbol(*sym_id))
}

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

fn lower_arith(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    kind: NodeKind,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let lhs = lower_expr(resolved, children[0], builder, block);
    let rhs = lower_expr(resolved, children[1], builder, block);
    let op = match kind {
        NodeKind::Add => BinOp::Add,
        NodeKind::Sub => BinOp::Sub,
        NodeKind::Mul => BinOp::Mul,
        NodeKind::Div => BinOp::Div,
        NodeKind::Mod => BinOp::Mod,
        _ => unreachable!(),
    };
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::BinOp(op, lhs, rhs)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

fn lower_comparison(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    kind: NodeKind,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let lhs = lower_expr(resolved, children[0], builder, block);
    let rhs = lower_expr(resolved, children[1], builder, block);

    // Check if the LHS operand's type has a trait impl for this comparison.
    let lhs_ti = resolved.node_types.get(&children[0]).copied();
    let wk = &resolved.type_pool.well_known;

    // For == and !=, check for Eq trait impl.
    // For <, <=, >, >=, check for Ord trait impl.
    let trait_dispatch = match kind {
        NodeKind::BoolEq | NodeKind::BoolNotEq => lhs_ti.and_then(|ti| {
            if resolved.type_pool.has_trait_impl(ti, wk.eq) {
                Some((ti, wk.eq, "eq"))
            } else if resolved.type_pool.has_trait_impl(ti, wk.partial_eq) {
                Some((ti, wk.partial_eq, "eq"))
            } else {
                None
            }
        }),
        NodeKind::BoolLt | NodeKind::BoolLtEq | NodeKind::BoolGt | NodeKind::BoolGtEq => lhs_ti
            .and_then(|ti| {
                if resolved.type_pool.has_trait_impl(ti, wk.ord) {
                    Some((ti, wk.ord, "cmp"))
                } else if resolved.type_pool.has_trait_impl(ti, wk.partial_ord) {
                    Some((ti, wk.partial_ord, "partial_cmp"))
                } else {
                    None
                }
            }),
        _ => None,
    };

    if let Some((_type_idx, _trait_idx, method_name)) = trait_dispatch {
        // Dispatch to the trait method.
        let method_str = str_interner::intern(method_name);
        let local = builder.alloc_local();

        match kind {
            NodeKind::BoolEq => {
                // lhs.eq(rhs)
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::MethodCall(lhs, method_str, vec![rhs]),
                ));
            }
            NodeKind::BoolNotEq => {
                // !lhs.eq(rhs)
                let eq_local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    eq_local,
                    NirExpr::MethodCall(lhs, method_str, vec![rhs]),
                ));
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::UnaryOp(UnaryOp::Not, NirValue::Local(eq_local)),
                ));
            }
            NodeKind::BoolLt | NodeKind::BoolLtEq | NodeKind::BoolGt | NodeKind::BoolGtEq => {
                // lhs.cmp(rhs) then compare ordering result
                let cmp_local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    cmp_local,
                    NirExpr::MethodCall(lhs, method_str, vec![rhs]),
                ));
                // Compare the ordering result with 0 (Equal).
                // cmp returns: -1 (Less), 0 (Equal), 1 (Greater)
                let zero = NirValue::ConstInt(0);
                let cmp_val = NirValue::Local(cmp_local);
                let op = match kind {
                    NodeKind::BoolLt => BinOp::Lt,
                    NodeKind::BoolLtEq => BinOp::Le,
                    NodeKind::BoolGt => BinOp::Gt,
                    NodeKind::BoolGtEq => BinOp::Ge,
                    _ => unreachable!(),
                };
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::BinOp(op, cmp_val, zero)));
            }
            _ => unreachable!(),
        }
        return NirValue::Local(local);
    }

    // Fallback: use intrinsic comparison.
    let op = match kind {
        NodeKind::BoolEq => BinOp::Eq,
        NodeKind::BoolNotEq => BinOp::Ne,
        NodeKind::BoolGt => BinOp::Gt,
        NodeKind::BoolGtEq => BinOp::Ge,
        NodeKind::BoolLt => BinOp::Lt,
        NodeKind::BoolLtEq => BinOp::Le,
        _ => unreachable!(),
    };
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::BinOp(op, lhs, rhs)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Logical operators
// ---------------------------------------------------------------------------

fn lower_logical(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    kind: NodeKind,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let lhs = lower_expr(resolved, children[0], builder, block);
    let rhs = lower_expr(resolved, children[1], builder, block);
    let op = if kind == NodeKind::BoolAnd {
        BinOp::And
    } else {
        BinOp::Or
    };
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::BinOp(op, lhs, rhs)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Unary operators
// ---------------------------------------------------------------------------

fn lower_unary(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    op: UnaryOp,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let inner = lower_expr(resolved, children[0], builder, block);
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::UnaryOp(op, inner)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Extended call (struct construction): TypeName { field: val, ... }
// ---------------------------------------------------------------------------

fn lower_extended_call(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let callee = children[0];

    // Get the struct TypeIndex from the callee's symbol.
    let type_idx = resolved
        .node_symbols
        .get(&callee)
        .map(|&sym_id| resolved.symbols[sym_id.0 as usize].type_index)
        .unwrap_or(type_pool::TypeIndex::INVALID);

    // Lower each property value in source order.
    // For now, assume fields appear in declaration order (matching the struct).
    let field_vals: Vec<NirValue> = ast
        .multi_children(node_idx)
        .iter()
        .map(|&arg| {
            if ast.node(arg).kind == NodeKind::Property {
                let prop_children = ast.fixed_children(arg);
                if prop_children.len() > 1 {
                    lower_expr(resolved, prop_children[1], builder, block)
                } else {
                    NirValue::Unit
                }
            } else {
                lower_expr(resolved, arg, builder, block)
            }
        })
        .collect();

    let local = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        local,
        NirExpr::NewObject(type_idx, field_vals),
    ));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Call expression
// ---------------------------------------------------------------------------

fn lower_call(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    // Call: [0] callee  multi = args
    let args: Vec<NirValue> = ast
        .multi_children(node_idx)
        .iter()
        .map(|&arg| lower_expr(resolved, arg, builder, block))
        .collect();

    // Check if the callee is a builtin function.
    let callee_node = children[0];
    if !callee_node.is_null() {
        if let Some(&sym_id) = resolved.node_symbols.get(&callee_node) {
            // Check if this symbol is a builtin function.
            if let Some(&builtin_id) = resolved.builtin_fns.get(&sym_id) {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::CallBuiltin(builtin_id, args),
                ));
                return NirValue::Local(local);
            }
            // Check if this is a user-defined function referenced by symbol.
            let sym = &resolved.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Function {
                if let Some(&func_id) = builder.func_map.get(&sym_id) {
                    let local = builder.alloc_local();
                    builder.blocks[block.0 as usize]
                        .stmts
                        .push(NirStmt::Assign(local, NirExpr::Call(func_id, args)));
                    return NirValue::Local(local);
                }
            }
            // Check if callee is a variable/parameter — indirect call through closure
            // or apply desugaring for non-function types.
            if matches!(sym.kind, SymbolKind::Variable | SymbolKind::Parameter) {
                // Check the type to determine call strategy.
                let callee_ti = resolved
                    .node_types
                    .get(&callee_node)
                    .copied()
                    .unwrap_or(sym.type_index);
                let is_function_type = callee_ti != type_pool::TypeIndex::INVALID
                    && matches!(
                        resolved.type_pool.get(callee_ti).kind,
                        type_pool::TypeKind::Function { .. }
                    );

                if is_function_type || callee_ti == type_pool::TypeIndex::INVALID {
                    // Function type or unknown type → indirect call.
                    let callee_local = if let Some(&l) = builder.symbol_to_local.get(&sym_id) {
                        l
                    } else {
                        builder.local_for_symbol(sym_id)
                    };
                    let callee_val = NirValue::Local(callee_local);
                    let local = builder.alloc_local();
                    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                        local,
                        NirExpr::CallIndirect(callee_val, args),
                    ));
                    return NirValue::Local(local);
                } else {
                    // Non-function type → desugar obj(args) to obj.apply(args).
                    let callee_local = if let Some(&l) = builder.symbol_to_local.get(&sym_id) {
                        l
                    } else {
                        builder.local_for_symbol(sym_id)
                    };
                    let callee_val = NirValue::Local(callee_local);
                    let apply_str = str_interner::intern("apply");
                    let local = builder.alloc_local();
                    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                        local,
                        NirExpr::MethodCall(callee_val, apply_str, args),
                    ));
                    return NirValue::Local(local);
                }
            }
            // Type(args) → Type.new(args) desugaring.
            if sym.kind == SymbolKind::Type {
                let new_str = str_interner::intern("new");
                // Look up `new` in the type's associated scope.
                if let Some(new_sym_id) = lookup_in_type_scope(resolved, sym.type_index, new_str) {
                    if let Some(&func_id) = builder.func_map.get(&new_sym_id) {
                        let local = builder.alloc_local();
                        builder.blocks[block.0 as usize]
                            .stmts
                            .push(NirStmt::Assign(local, NirExpr::Call(func_id, args)));
                        return NirValue::Local(local);
                    }
                }
                // No `new` found — emit a method call so the interpreter can
                // look it up at runtime.
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::MethodCall(NirValue::Unit, new_str, args),
                ));
                return NirValue::Local(local);
            }
        }
    }

    // Fallback: unresolved call — emit Call(FuncId(0)) as placeholder.
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::Call(FuncId(0), args)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Block expression
// ---------------------------------------------------------------------------

fn lower_block_expr(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let stmts = ast.multi_children(node_idx);
    let mut last_val = NirValue::Unit;
    for (i, &stmt) in stmts.iter().enumerate() {
        if i == stmts.len() - 1 {
            last_val = lower_expr(resolved, stmt, builder, block);
        } else {
            crate::stmt::lower_stmt(resolved, stmt, builder, block, false);
        }
    }
    last_val
}

// ---------------------------------------------------------------------------
// Projection lowering (field access or enum variant construction)
// ---------------------------------------------------------------------------

fn lower_projection(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let member_node = children[1];

    // Check if the member resolves to an enum variant.
    if !member_node.is_null() {
        if let Some(&sym_id) = resolved.node_symbols.get(&member_node) {
            let sym = &resolved.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::EnumVariant {
                if let Some(&idx) = resolved.enum_variant_indices.get(&sym_id) {
                    return NirValue::ConstInt(idx as i64);
                }
            }
        }
    }

    // Fallback: regular field access / method call.
    let object = lower_expr(resolved, children[0], builder, block);

    // If type resolution populated a field index for this projection, emit FieldAccess.
    if let Some(&field_idx) = resolved.node_field_indices.get(&node_idx) {
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::FieldAccess(object, field_idx),
        ));
        return NirValue::Local(local);
    }

    let field_name = ast.node(member_node).str_id;
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        local,
        NirExpr::MethodCall(object, field_name, Vec::new()),
    ));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Lambda lowering — capture analysis + closure creation
// ---------------------------------------------------------------------------

/// Lower a Lambda AST node into a `NewClosure` NIR expression.
///
/// 1. Create a new NirFunction for the lambda body with an extra parameter
///    per captured variable (captures come before user params).
/// 2. Walk the lambda body to discover free variables — any symbol that is
///    already bound in the *parent* builder's symbol_to_local map.
/// 3. Emit `NewClosure(lambda_func_id, [captured_values])` in the parent.
fn lower_lambda(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    // Lambda AST: fixed: [0] body  [1] return_type   multi: params
    let children = ast.fixed_children(node_idx);
    let body_node = children[0];
    let params = ast.multi_children(node_idx);

    // Phase 1: discover free variables by collecting all referenced symbols
    // in the body that are already known in the parent's scope.
    let mut free_vars: Vec<(resolution::SymbolId, NirLocal)> = Vec::new();
    collect_free_vars(resolved, ast, body_node, builder, &mut free_vars);
    // Deduplicate preserving order.
    let mut seen = std::collections::HashSet::new();
    free_vars.retain(|(sym, _)| seen.insert(*sym));

    // Phase 2: build the lambda's NirFunction.
    let lambda_name = str_interner::intern("<lambda>");
    let lambda_func_id = builder.alloc_func_id();
    let mut lambda_builder = FunctionBuilder::new(lambda_func_id, lambda_name);
    lambda_builder.func_map = builder.func_map.clone();
    lambda_builder.next_func_id = builder.next_func_id;
    lambda_builder.is_closure = true;

    // Add capture parameters first (LoadCapture will access these).
    // In the lambda body, each captured variable gets a fresh local that
    // is bound to the corresponding capture parameter.
    for (sym_id, _parent_local) in &free_vars {
        let local = lambda_builder.alloc_local();
        lambda_builder.symbol_to_local.insert(*sym_id, local);
        lambda_builder.params.push(NirParam {
            local,
            name: resolved.symbols[sym_id.0 as usize].name,
            type_index: type_pool::TypeIndex::INVALID,
            is_evidence: false,
        });
    }

    // Add explicit lambda parameters.
    for &param in params {
        let param_node = ast.node(param);
        match param_node.kind {
            NodeKind::ParamTyped | NodeKind::ParamLambda => {
                let param_children = ast.fixed_children(param);
                let pat_node_idx = param_children[0];
                let pat_node = ast.node(pat_node_idx);
                let local = if let Some(&sym_id) = resolved.node_symbols.get(&pat_node_idx) {
                    lambda_builder.local_for_symbol(sym_id)
                } else {
                    lambda_builder.alloc_local()
                };
                lambda_builder.params.push(NirParam {
                    local,
                    name: pat_node.str_id,
                    type_index: type_pool::TypeIndex::INVALID,
                    is_evidence: false,
                });
            }
            NodeKind::ParamSelf => {
                let local = lambda_builder.alloc_local();
                lambda_builder.params.push(NirParam {
                    local,
                    name: param_node.str_id,
                    type_index: type_pool::TypeIndex::INVALID,
                    is_evidence: false,
                });
            }
            NodeKind::ParamOptional => {
                let param_children = ast.fixed_children(param);
                let id_node_idx = param_children[0];
                let id_node = ast.node(id_node_idx);
                let local = if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                    lambda_builder.local_for_symbol(sym_id)
                } else {
                    lambda_builder.alloc_local()
                };
                lambda_builder.params.push(NirParam {
                    local,
                    name: id_node.str_id,
                    type_index: type_pool::TypeIndex::INVALID,
                    is_evidence: false,
                });
            }
            NodeKind::ParamVarargs => {
                let param_children = ast.fixed_children(param);
                let id_node_idx = param_children[0];
                let id_node = ast.node(id_node_idx);
                let local = if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                    lambda_builder.local_for_symbol(sym_id)
                } else {
                    lambda_builder.alloc_local()
                };
                lambda_builder.params.push(NirParam {
                    local,
                    name: id_node.str_id,
                    type_index: type_pool::TypeIndex::INVALID,
                    is_evidence: false,
                });
            }
            _ => {}
        }
    }

    // Create the entry block and lower the body.
    let mut entry = lambda_builder.new_block();
    lambda_builder.entry_block = entry;

    if !body_node.is_null() {
        crate::stmt::lower_body_into(resolved, body_node, &mut lambda_builder, &mut entry);
    } else {
        lambda_builder.blocks[entry.0 as usize].terminator = Terminator::Return(NirValue::Unit);
    }

    // Reclaim any nested lambda func ids.
    builder.next_func_id = lambda_builder.next_func_id;

    // Collect nested lambda functions.
    let nested_lambdas = std::mem::take(&mut lambda_builder.lambda_functions);
    let lambda_fn = lambda_builder.build();
    builder.lambda_functions.push(lambda_fn);
    builder.lambda_functions.extend(nested_lambdas);

    // Phase 3: emit NewClosure in the parent function.
    let captures: Vec<NirValue> = free_vars
        .iter()
        .map(|(_, parent_local)| NirValue::Local(*parent_local))
        .collect();

    let result = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        result,
        NirExpr::NewClosure(lambda_func_id, captures),
    ));
    NirValue::Local(result)
}

/// Recursively collect free variables referenced in a subtree.
///
/// A "free variable" is any identifier whose resolved SymbolId is already
/// mapped to a NirLocal in the *parent* builder — i.e., it was defined in
/// an enclosing scope and needs to be captured.
fn collect_free_vars(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    parent_builder: &FunctionBuilder,
    out: &mut Vec<(resolution::SymbolId, NirLocal)>,
) {
    if node_idx.is_null() {
        return;
    }
    let node = ast.node(node_idx);

    // If this is an identifier that resolves to a symbol the parent knows about,
    // it's a free variable that needs capturing.
    if node.kind == NodeKind::Id {
        if let Some(&sym_id) = resolved.node_symbols.get(&node_idx) {
            let sym = &resolved.symbols[sym_id.0 as usize];
            match sym.kind {
                SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant => {
                    if let Some(&parent_local) = parent_builder.symbol_to_local.get(&sym_id) {
                        out.push((sym_id, parent_local));
                    }
                }
                _ => {}
            }
        }
    }

    // Don't recurse into nested lambdas — they get their own capture analysis.
    if node.kind == NodeKind::Lambda && node_idx != node_idx {
        // This condition is always false; nested lambdas ARE recursed into
        // so that they can reference the same outer variables. The nested
        // lambda's own lowering will re-capture from the inner scope.
    }

    // Recurse into children.
    for &child in ast.fixed_children(node_idx) {
        collect_free_vars(resolved, ast, child, parent_builder, out);
    }
    for &child in ast.multi_children(node_idx) {
        collect_free_vars(resolved, ast, child, parent_builder, out);
    }
}

// ---------------------------------------------------------------------------
// Match expression lowering
// ---------------------------------------------------------------------------

/// Lower `expr match { arms }` into a chain of compare-and-branch blocks.
fn lower_match(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    // [0] scrutinee, multi = arms
    let scrutinee = lower_expr(resolved, children[0], builder, block);

    let arms = ast.multi_children(node_idx);
    if arms.is_empty() {
        return NirValue::Unit;
    }

    let result_local = builder.alloc_local();
    let merge_block = builder.new_block();

    let mut current_block = *block;
    for (i, &arm_node) in arms.iter().enumerate() {
        let arm_children = ast.fixed_children(arm_node);
        // CaseArm: [0] pattern  [1] body
        let pattern_node = arm_children[0];
        let body_node = arm_children[1];

        let is_last = i == arms.len() - 1;
        let is_wildcard = is_wildcard_pattern(ast, pattern_node);

        if is_wildcard || is_last {
            let body_val = lower_expr(resolved, body_node, builder, &mut current_block);
            builder.blocks[current_block.0 as usize]
                .stmts
                .push(NirStmt::Assign(result_local, NirExpr::Use(body_val)));
            builder.blocks[current_block.0 as usize].terminator = Terminator::Goto(merge_block);
            break;
        }

        let pattern_val =
            lower_pattern_value(resolved, ast, pattern_node, builder, &mut current_block);

        let cmp_local = builder.alloc_local();
        builder.blocks[current_block.0 as usize]
            .stmts
            .push(NirStmt::Assign(
                cmp_local,
                NirExpr::BinOp(BinOp::Eq, scrutinee.clone(), pattern_val),
            ));

        let mut then_block = builder.new_block();
        let else_block = builder.new_block();

        builder.blocks[current_block.0 as usize].terminator =
            Terminator::Branch(NirValue::Local(cmp_local), then_block, else_block);

        let body_val = lower_expr(resolved, body_node, builder, &mut then_block);
        builder.blocks[then_block.0 as usize]
            .stmts
            .push(NirStmt::Assign(result_local, NirExpr::Use(body_val)));
        builder.blocks[then_block.0 as usize].terminator = Terminator::Goto(merge_block);

        current_block = else_block;
    }

    // If we ran through all arms without a wildcard, add default.
    if matches!(
        builder.blocks[current_block.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        builder.blocks[current_block.0 as usize]
            .stmts
            .push(NirStmt::Assign(result_local, NirExpr::Use(NirValue::Unit)));
        builder.blocks[current_block.0 as usize].terminator = Terminator::Goto(merge_block);
    }

    *block = merge_block;
    NirValue::Local(result_local)
}

fn is_wildcard_pattern(ast: &Ast, node_idx: NodeIndex) -> bool {
    if node_idx.is_null() {
        return false;
    }
    matches!(ast.node(node_idx).kind, NodeKind::Underscore)
}

/// Lower a pattern into a value for comparison.
fn lower_pattern_value(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    _block: &mut BlockId,
) -> NirValue {
    if node_idx.is_null() {
        return NirValue::Unit;
    }
    let node = ast.node(node_idx);
    match node.kind {
        NodeKind::PropertyPattern => {
            let children = ast.fixed_children(node_idx);
            let member_node = children[1];
            if let Some(&sym_id) = resolved.node_symbols.get(&member_node) {
                let sym = &resolved.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::EnumVariant {
                    if let Some(&idx) = resolved.enum_variant_indices.get(&sym_id) {
                        return NirValue::ConstInt(idx as i64);
                    }
                }
            }
            NirValue::Unit
        }
        NodeKind::Int => lower_int_literal(node),
        NodeKind::Real => lower_real_literal(node),
        NodeKind::Bool => lower_bool_literal(node),
        NodeKind::Str => lower_str_literal(node),
        NodeKind::Char => lower_char_literal(node),
        NodeKind::Id => lower_id(resolved, node_idx, builder),
        _ => NirValue::Unit,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Look up a name in (any) scope associated with a given type.
fn lookup_in_type_scope(
    resolved: &ResolvedAst,
    type_idx: type_pool::TypeIndex,
    name: str_interner::StrId,
) -> Option<resolution::SymbolId> {
    for scope in &resolved.scopes {
        if scope.assoc_type == Some(type_idx) {
            if let Some(&sym_id) = scope.bindings.get(&name) {
                return Some(sym_id);
            }
        }
    }
    None
}
