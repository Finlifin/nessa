//! Synthesize canonical bodyless Nessa interface stubs, exported symbol link tables,
//! and deterministic SHA-256 interface digests for modular `.nsbc` archives.

use std::collections::{BTreeSet, HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use nir::{
    ExternalFunctionOrigin, LocalExportOrigin, ModularImportedFunction, ModularImportedGlobal,
    ModularLocalExport, ModularScopeUnit,
};
use nsbc::{
    CodegenOutput, ExportTarget, ExportedRuntimeSymbol, ImportedFunction, ImportedGlobal,
    InterfaceModuleStub, SymbolLinkSelector,
};
use resolution::{ResolvedAst, ScopeId, SymbolId, SymbolKind, Visibility};
use rustc_span::{BytePos, SourceMap, Span};
use sha2::{Digest, Sha256};
use type_pool::{Intrinsic, TypeId, TypeIndex, TypeKind, TypePool};

fn unwrap_modifiers(ast: &Ast, mut node: NodeIndex) -> (NodeIndex, bool, bool, bool) {
    let mut is_pub = false;
    let mut is_priv = false;
    let mut is_global = false;
    while !node.is_null() {
        match ast.node(node).kind {
            NodeKind::PubDef => {
                is_pub = true;
                node = ast.fixed_children(node)[0];
            }
            NodeKind::PrivateDef => {
                is_priv = true;
                node = ast.fixed_children(node)[0];
            }
            NodeKind::GlobalDecl => {
                is_global = true;
                node = ast.fixed_children(node)[0];
            }
            _ => break,
        }
    }
    (node, is_pub, is_priv, is_global)
}

fn is_effect_or_error_set_expr(ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() {
        return false;
    }
    match ast.node(node).kind {
        NodeKind::ListOf => ast
            .multi_children(node)
            .iter()
            .all(|&c| is_effect_or_error_set_expr(ast, c)),
        NodeKind::Concat => {
            let children = ast.fixed_children(node);
            is_effect_or_error_set_expr(ast, children[0])
                && is_effect_or_error_set_expr(ast, children[1])
        }
        NodeKind::Id | NodeKind::Projection | NodeKind::View | NodeKind::SelfUpper => true,
        _ => false,
    }
}

/// True when a `ConstDecl` / `LetDecl` / `VarDecl` RHS is a compile-time `'builtin` view,
/// `#effect` / `!error` `const` set alias, or static boolean expression whose AST RHS must be
/// retained verbatim (and NOT marked in `external_stub_nodes`).
pub(crate) fn is_preserved_compile_time_rhs(
    ast: &Ast,
    decl_kind: NodeKind,
    rhs: NodeIndex,
) -> bool {
    if rhs.is_null() {
        return false;
    }
    if decl_kind == NodeKind::ConstDecl && is_effect_or_error_set_expr(ast, rhs) {
        return true;
    }
    let mut stack = vec![rhs];
    let mut has_compile_time_construct = false;
    while let Some(cur) = stack.pop() {
        if cur.is_null() {
            continue;
        }
        match ast.node(cur).kind {
            NodeKind::View | NodeKind::EffectPropagation | NodeKind::Bool => {
                has_compile_time_construct = true;
            }
            NodeKind::Lambda | NodeKind::Block | NodeKind::Call | NodeKind::ExtendedCall => {
                return false;
            }
            _ => {}
        }
        stack.extend_from_slice(ast.fixed_children(cur));
        stack.extend_from_slice(ast.multi_children(cur));
    }
    has_compile_time_construct
}

/// Strip `--` line comments and trailing whitespace while preserving string/char literals verbatim.
fn canonicalize_nessa_snippet(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    let mut in_str = false;
    let mut in_char = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                i += 1;
                out.push(chars[i]);
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if in_char {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                i += 1;
                out.push(chars[i]);
            } else if c == '\'' {
                in_char = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '\'' {
            // Distinguish `'a'` char literal from `'builtin` view suffix.
            if i + 2 < chars.len() && (chars[i + 1] == '\\' || chars[i + 2] == '\'') {
                in_char = true;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }

    let mut cleaned_lines = Vec::new();
    for line in out.lines() {
        let trimmed_end = line.trim_end();
        if !trimmed_end.is_empty() {
            cleaned_lines.push(trimmed_end);
        }
    }
    cleaned_lines.join("\n")
}

fn span_slice(source_map: &SourceMap, span: Span) -> Result<String, String> {
    if span.is_dummy() {
        return Err("cannot slice dummy span while synthesizing interface stub".into());
    }
    source_map
        .span_to_snippet(span)
        .map_err(|e| format!("failed to slice span for interface stub: {e:?}"))
}

fn slice_range(source_map: &SourceMap, lo: BytePos, hi: BytePos) -> Result<String, String> {
    span_slice(source_map, Span::new(lo, hi))
}

/// Compute the full source span covering `root` and all non-dummy descendant AST nodes.
///
/// Compound nodes in Nessa's parser (such as `Call`, `ErrorQualifiedType`, `Projection`, `PubDef`)
/// store only the operator or modifier token span on `ast.node(n).span`, while left/right operand
/// spans live on child nodes.
fn subtree_span(ast: &Ast, root: NodeIndex) -> Span {
    if root.is_null() {
        return Span::default();
    }
    let mut min_lo: Option<BytePos> = None;
    let mut max_hi: Option<BytePos> = None;
    let mut stack = vec![root];
    while let Some(cur) = stack.pop() {
        if cur.is_null() {
            continue;
        }
        let sp = ast.node(cur).span;
        if !sp.is_dummy() {
            min_lo = Some(match min_lo {
                Some(prev) => prev.min(sp.lo()),
                None => sp.lo(),
            });
            max_hi = Some(match max_hi {
                Some(prev) => prev.max(sp.hi()),
                None => sp.hi(),
            });
        }
        stack.extend_from_slice(ast.fixed_children(cur));
        stack.extend_from_slice(ast.multi_children(cur));
    }
    match (min_lo, max_hi) {
        (Some(lo), Some(hi)) => Span::new(lo, hi),
        _ => ast.node(root).span,
    }
}

fn qualified_nominal_type_name(
    resolved: &ResolvedAst,
    use_scope: ScopeId,
    target_ty: TypeIndex,
) -> String {
    let pool = &resolved.type_pool;
    let Some(canon) = pool.canonical_type(target_ty) else {
        return "Any".into();
    };
    if let Some(intr) = pool.as_intrinsic(canon) {
        return intr.name().to_owned();
    }
    if canon == pool.null_type() {
        return "?NoReturn".into();
    }
    // Find defining symbol in `resolved.symbols`.
    let sym_opt = resolved.symbols.iter().find(|s| {
        matches!(
            s.kind,
            SymbolKind::Type | SymbolKind::Trait | SymbolKind::Effect | SymbolKind::Module
        ) && !s.def_node.is_null()
            && pool.canonical_type(s.type_index) == Some(canon)
    });
    let Some(sym) = sym_opt else {
        if let Some(role) = pool.checked_collection_role(canon).ok().flatten() {
            return format!("std.collections.{}", role_display_name(role));
        }
        return match &pool.get(canon).kind {
            TypeKind::Struct { name, .. }
            | TypeKind::Enum { name, .. }
            | TypeKind::Newtype { name, .. }
            | TypeKind::Trait { name, .. }
            | TypeKind::Module { name, .. } => str_interner::get(*name),
            TypeKind::AssociatedType { name, .. } => str_interner::get(*name),
            _ => "Any".into(),
        };
    };

    // Check if `sym.name` resolves directly to `canon` from `use_scope`.
    let mut check_sc = Some(use_scope);
    while let Some(sid) = check_sc {
        if let Some(&bound_sym) = resolved.scopes[sid.0 as usize].bindings.get(&sym.name) {
            if resolved
                .type_pool
                .canonical_type(resolved.symbols[bound_sym.0 as usize].type_index)
                == Some(canon)
            {
                return str_interner::get(sym.name);
            }
            break;
        }
        check_sc = resolved.scopes[sid.0 as usize].parent;
    }

    // Build qualified module path within its package.
    let mut parts = vec![str_interner::get(sym.name)];
    let mut cur_sc = Some(sym.scope);
    let sym_pkg = pool.scope_context(sym.scope.0).map(|c| c.package);
    let use_pkg = pool.scope_context(use_scope.0).map(|c| c.package);
    while let Some(sid) = cur_sc {
        let sc = &resolved.scopes[sid.0 as usize];
        if !sc.node.is_null() && resolved.ast.node(sc.node).kind == NodeKind::ModuleDef {
            let name_node = resolved.ast.fixed_children(sc.node)[0];
            if !name_node.is_null() {
                parts.push(str_interner::get(resolved.ast.node(name_node).str_id));
            }
        }
        let next = sc.parent;
        if next.and_then(|p| pool.scope_context(p.0).map(|c| c.package)) != sym_pkg {
            break;
        }
        cur_sc = next;
    }
    parts.reverse();
    if sym_pkg == use_pkg {
        format!("@{}", parts.join("."))
    } else {
        parts.join(".")
    }
}

fn role_display_name(role: type_pool::CollectionRole) -> &'static str {
    match role {
        type_pool::CollectionRole::List => "List",
        type_pool::CollectionRole::Buffer => "Buffer",
        type_pool::CollectionRole::Map => "Map",
        type_pool::CollectionRole::MapBuffer => "MapBuffer",
        type_pool::CollectionRole::Bytes => "Bytes",
        type_pool::CollectionRole::BytesBuffer => "BytesBuffer",
    }
}

/// Format a resolved [`TypeIndex`] as a valid Nessa type annotation in `use_scope`.
pub(crate) fn format_type_expr(
    resolved: &ResolvedAst,
    use_scope: ScopeId,
    ty: TypeIndex,
) -> String {
    if ty == TypeIndex::INVALID {
        return "Any".into();
    }
    let pool = &resolved.type_pool;
    let Some(canon) = pool.canonical_type(ty) else {
        return "Any".into();
    };
    if canon == pool.null_type() {
        return "?Any".into();
    }
    if let Ok(Some(item)) = pool.checked_iteration_step_item(canon) {
        return format!(
            "std.traits.IterationStep({})",
            format_type_expr(resolved, use_scope, item)
        );
    }
    match &pool.get(canon).kind {
        TypeKind::Intrinsic(intr) => intr.name().to_owned(),
        TypeKind::Optional { inner } => {
            if pool.as_intrinsic(*inner) == Some(Intrinsic::NoReturn) {
                "?Any".into()
            } else {
                format!("?{}", format_type_expr(resolved, use_scope, *inner))
            }
        }
        TypeKind::Future { inner } => {
            format!("Future({})", format_type_expr(resolved, use_scope, *inner))
        }
        TypeKind::Tuple { elements } => {
            if elements.is_empty() {
                "Unit".into()
            } else if elements.len() == 1 {
                format!("({},)", format_type_expr(resolved, use_scope, elements[0]))
            } else {
                let elems: Vec<String> = elements
                    .iter()
                    .map(|&e| format_type_expr(resolved, use_scope, e))
                    .collect();
                format!("({})", elems.join(", "))
            }
        }
        TypeKind::Function { params, ret } => {
            let ps: Vec<String> = params
                .iter()
                .map(|&p| format_type_expr(resolved, use_scope, p))
                .collect();
            format!(
                "fn({}) -> {}",
                ps.join(", "),
                format_type_expr(resolved, use_scope, *ret)
            )
        }
        TypeKind::ErrorQualified { errors, inner } => {
            let errs: Vec<String> = errors
                .iter()
                .map(|&e| qualified_nominal_type_name(resolved, use_scope, e))
                .collect();
            let err_str = if errs.len() == 1 {
                format!("!{}", errs[0])
            } else {
                format!("!{{{}}}", errs.join(", "))
            };
            format!(
                "{err_str} {}",
                format_type_expr(resolved, use_scope, *inner)
            )
        }
        TypeKind::EffectQualified { effects, inner } => {
            let effs: Vec<String> = effects
                .iter()
                .map(|&e| qualified_nominal_type_name(resolved, use_scope, e))
                .collect();
            let eff_str = if effs.len() == 1 {
                format!("#{}", effs[0])
            } else {
                format!("#{{{}}}", effs.join(", "))
            };
            format!(
                "{eff_str} {}",
                format_type_expr(resolved, use_scope, *inner)
            )
        }
        TypeKind::IterationStepTemplate { item } => format!(
            "std.traits.IterationStep({})",
            format_type_expr(resolved, use_scope, *item)
        ),
        TypeKind::AssociatedType { name, .. } => str_interner::get(*name),
        TypeKind::Typealias { target, .. } => format_type_expr(resolved, use_scope, *target),
        TypeKind::Struct { .. }
        | TypeKind::Enum { .. }
        | TypeKind::Newtype { .. }
        | TypeKind::Trait { .. }
        | TypeKind::Module { .. }
        | TypeKind::Effect { .. } => qualified_nominal_type_name(resolved, use_scope, canon),
    }
}

fn collect_ast_referenced_symbols(
    resolved: &ResolvedAst,
    root_node: NodeIndex,
    out: &mut Vec<SymbolId>,
) {
    if root_node.is_null() {
        return;
    }
    let mut stack = vec![root_node];
    while let Some(n) = stack.pop() {
        if n.is_null() {
            continue;
        }
        if let Some(&sym) = resolved.node_symbols.get(&n) {
            out.push(sym);
        }
        for map in [
            &resolved.instance_methods,
            &resolved.application_calls,
            &resolved.concat_calls,
            &resolved.equality_calls,
            &resolved.update_calls,
            &resolved.constructor_types,
        ] {
            if let Some(&sym) = map.get(&n) {
                out.push(sym);
            }
        }
        stack.extend_from_slice(resolved.ast.fixed_children(n));
        stack.extend_from_slice(resolved.ast.multi_children(n));
    }
}

fn compute_retained_template_symbols(
    resolved: &ResolvedAst,
    target_root: NodeIndex,
) -> HashSet<SymbolId> {
    let ast = &resolved.ast;
    let mut seeds = Vec::new();
    // Retain compiler-trusted internal `std` entrypoints looked up directly by `resolution::trusted`.
    for sym in &resolved.symbols {
        let sname = str_interner::get(sym.name);
        let is_pub_const = sym.visibility == Visibility::Public
            && !sym.def_node.is_null()
            && ast.node(sym.def_node).kind == NodeKind::ConstDecl;
        if matches!(sname.as_str(), "__derived_from_json_sig" | "dynamic_equal") || is_pub_const {
            seeds.push(sym.id);
        }
    }
    let mut walk = vec![target_root];
    while let Some(node) = walk.pop() {
        if node.is_null() || resolved.external_package_roots.contains(&node) {
            continue;
        }
        match ast.node(node).kind {
            NodeKind::TraitDeriveFn => {
                collect_ast_referenced_symbols(resolved, node, &mut seeds);
                continue;
            }
            NodeKind::TraitDefFn | NodeKind::Typealias | NodeKind::Newtype => {
                collect_ast_referenced_symbols(resolved, node, &mut seeds);
                continue;
            }
            NodeKind::FunctionDef => {
                let fch = ast.fixed_children(node);
                let name_node = fch[0];
                let sym_opt = (!name_node.is_null())
                    .then(|| resolved.node_symbols.get(&name_node).copied())
                    .flatten();
                let is_trait_impl_template = sym_opt
                    .and_then(|sym| {
                        let sc = resolved.symbols[sym.0 as usize].scope;
                        resolved.scopes[sc.0 as usize].assoc_type
                    })
                    .and_then(|ty| resolved.type_pool.canonical_type(ty))
                    .is_some_and(|ty| {
                        matches!(resolved.type_pool.get(ty).kind, TypeKind::Trait { .. })
                    });
                if is_trait_impl_template || resolved.iterator_template_nodes.contains(&node) {
                    collect_ast_referenced_symbols(resolved, node, &mut seeds);
                    continue;
                }
                if sym_opt.is_some_and(|sym| {
                    resolved.symbols[sym.0 as usize].visibility == Visibility::Public
                }) {
                    collect_ast_referenced_symbols(resolved, fch[1], &mut seeds);
                    collect_ast_referenced_symbols(resolved, fch[3], &mut seeds);
                    for &param in ast.multi_children(node) {
                        collect_ast_referenced_symbols(resolved, param, &mut seeds);
                    }
                    continue;
                }
                for &param in ast.multi_children(node) {
                    if !param.is_null() && ast.node(param).kind == NodeKind::ParamOptional {
                        collect_ast_referenced_symbols(
                            resolved,
                            ast.fixed_children(param)[2],
                            &mut seeds,
                        );
                    }
                }
            }
            NodeKind::EffectDef | NodeKind::AsyncEffectDef => {
                let fch = ast.fixed_children(node);
                collect_ast_referenced_symbols(resolved, fch[1], &mut seeds);
                for &param in ast.multi_children(node) {
                    collect_ast_referenced_symbols(resolved, param, &mut seeds);
                }
            }
            NodeKind::StructField => {
                collect_ast_referenced_symbols(resolved, ast.fixed_children(node)[1], &mut seeds);
                collect_ast_referenced_symbols(resolved, ast.fixed_children(node)[2], &mut seeds);
            }
            _ => {}
        }
        walk.extend_from_slice(ast.fixed_children(node));
        walk.extend_from_slice(ast.multi_children(node));
    }

    let mut retained = HashSet::new();
    while let Some(sym_id) = seeds.pop() {
        if !retained.insert(sym_id) {
            continue;
        }
        let sym = &resolved.symbols[sym_id.0 as usize];
        if sym.def_node.is_null() {
            continue;
        }
        let kind = ast.node(sym.def_node).kind;
        if matches!(
            kind,
            NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl
        ) {
            let rhs = ast.fixed_children(sym.def_node)[2];
            if is_preserved_compile_time_rhs(ast, kind, rhs) {
                collect_ast_referenced_symbols(resolved, rhs, &mut seeds);
            }
        } else if kind == NodeKind::FunctionDef {
            for &param in ast.multi_children(sym.def_node) {
                if !param.is_null() && ast.node(param).kind == NodeKind::ParamOptional {
                    collect_ast_referenced_symbols(
                        resolved,
                        ast.fixed_children(param)[2],
                        &mut seeds,
                    );
                }
            }
        }
    }
    retained
}

fn find_params_rparen_end(
    source_map: &SourceMap,
    fn_name_hi: BytePos,
    upper_bound: BytePos,
) -> Result<BytePos, String> {
    let seg = slice_range(source_map, fn_name_hi, upper_bound)?;
    // Scan from right after the function name, matching the balanced `(...)` of its parameter list.
    let bytes = seg.as_bytes();
    let mut depth = 0i32;
    let mut seen_lparen = false;
    let mut in_str = false;
    let mut in_char = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if in_char {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'\'' {
                in_char = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if b == b'\'' && i + 2 < bytes.len() && (bytes[i + 1] == b'\\' || bytes[i + 2] == b'\'') {
            in_char = true;
            i += 1;
            continue;
        }
        if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'(' {
            seen_lparen = true;
            depth += 1;
        } else if b == b')' && seen_lparen {
            depth -= 1;
            if depth == 0 {
                return Ok(BytePos(fn_name_hi.0 + (i + 1) as u32));
            }
        }
        i += 1;
    }
    Err(format!(
        "could not locate closing `)` of function parameter list in `{seg}`"
    ))
}

fn emit_stubbed_fn(
    resolved: &ResolvedAst,
    source_map: &SourceMap,
    outer_item: NodeIndex,
    fn_node: NodeIndex,
    enclosing_scope: ScopeId,
) -> Result<String, String> {
    let ast = &resolved.ast;
    let children = ast.fixed_children(fn_node);
    let name_node = children[0];
    let ret_node = children[1];
    let body_node = children[2];
    let cap_node = children[3];

    let outer_span = subtree_span(ast, outer_item);
    let upper = if !ret_node.is_null() {
        subtree_span(ast, ret_node).lo()
    } else if !cap_node.is_null() {
        subtree_span(ast, cap_node).lo()
    } else {
        subtree_span(ast, body_node).lo()
    };
    let rparen_hi = find_params_rparen_end(source_map, ast.node(name_node).span.hi(), upper)?;

    let prefix = canonicalize_nessa_snippet(&slice_range(source_map, outer_span.lo(), rparen_hi)?);

    let ret_part = if !ret_node.is_null() {
        // Slice directly from `rparen_hi` up to `subtree_span(ast, ret_node).hi()` so prefix
        // type operators (`!Err T`, `?T`, `#Eff T`) eaten before `ret_node` are preserved intact!
        let raw_ret = canonicalize_nessa_snippet(&slice_range(
            source_map,
            rparen_hi,
            subtree_span(ast, ret_node).hi(),
        )?);
        let ret_snip = raw_ret.trim().trim_end_matches(['{', '=', ':']).trim();
        format!(" {ret_snip}")
    } else {
        let inferred_ret = resolved
            .node_symbols
            .get(&name_node)
            .and_then(|&sym| {
                resolved
                    .type_pool
                    .canonical_type(resolved.symbols[sym.0 as usize].type_index)
            })
            .and_then(|sig| match resolved.type_pool.get(sig).kind {
                TypeKind::Function { ret, .. } => Some(ret),
                _ => None,
            })
            .unwrap_or(Intrinsic::Unit.type_index());
        format!(
            " -> {}",
            format_type_expr(resolved, enclosing_scope, inferred_ret)
        )
    };

    let cap_part = if !cap_node.is_null() {
        let raw_cap =
            canonicalize_nessa_snippet(&span_slice(source_map, subtree_span(ast, cap_node))?);
        let cap_snip = raw_cap
            .trim_end()
            .trim_end_matches(['{', '=', ':'])
            .trim_end();
        format!(" handles {cap_snip}")
    } else {
        String::new()
    };

    Ok(format!("{prefix}{ret_part}{cap_part} {{}}"))
}

fn emit_stubbed_decl(
    resolved: &ResolvedAst,
    source_map: &SourceMap,
    outer_item: NodeIndex,
    decl_node: NodeIndex,
    enclosing_scope: ScopeId,
) -> Result<String, String> {
    let ast = &resolved.ast;
    let children = ast.fixed_children(decl_node);
    let pat_node = children[0];
    let ty_node = children[1];
    let rhs_node = children[2];

    if is_preserved_compile_time_rhs(ast, ast.node(decl_node).kind, rhs_node) {
        return Ok(canonicalize_nessa_snippet(&span_slice(
            source_map,
            subtree_span(ast, outer_item),
        )?));
    }

    let outer_lo = subtree_span(ast, outer_item).lo();
    if !ty_node.is_null() {
        let ty_hi = subtree_span(ast, ty_node).hi();
        let raw_header = canonicalize_nessa_snippet(&slice_range(source_map, outer_lo, ty_hi)?);
        let header = raw_header.trim_end().trim_end_matches('=').trim_end();
        Ok(format!("{header} = ()"))
    } else {
        let pat_hi = subtree_span(ast, pat_node).hi();
        let raw_header = canonicalize_nessa_snippet(&slice_range(source_map, outer_lo, pat_hi)?);
        let header = raw_header
            .trim_end()
            .trim_end_matches(['=', ':'])
            .trim_end();
        let sym_ty = resolved
            .node_symbols
            .get(&pat_node)
            .map(|&s| resolved.symbols[s.0 as usize].type_index)
            .or_else(|| {
                resolved
                    .symbols
                    .iter()
                    .find(|s| s.def_node == decl_node && s.scope == enclosing_scope)
                    .map(|s| s.type_index)
            })
            .unwrap_or(Intrinsic::Any.type_index());
        let ty_str = format_type_expr(resolved, enclosing_scope, sym_ty);
        Ok(format!("{header}: {ty_str} = ()"))
    }
}

fn emit_impl_or_extend_block(
    resolved: &ResolvedAst,
    source_map: &SourceMap,
    outer_item: NodeIndex,
    block_node: NodeIndex,
    is_pub_outer: bool,
    retained_syms: &HashSet<SymbolId>,
) -> Result<Option<String>, String> {
    let ast = &resolved.ast;
    let kind = ast.node(block_node).kind;
    let block_scope = resolved
        .scopes
        .iter()
        .find(|s| s.node == block_node)
        .map(|s| s.id)
        .unwrap_or(ScopeId::ROOT);

    // If this is `impl <Trait>` (`Ord`, `Json`, `Iterator`, `IntoIterator`), keep verbatim
    // because its methods are specialized default method templates!
    if kind == NodeKind::ImplDef
        && resolved.scopes[block_scope.0 as usize]
            .assoc_type
            .and_then(|ty| resolved.type_pool.canonical_type(ty))
            .is_some_and(|ty| matches!(resolved.type_pool.get(ty).kind, TypeKind::Trait { .. }))
    {
        return Ok(Some(canonicalize_nessa_snippet(&span_slice(
            source_map,
            subtree_span(ast, outer_item),
        )?)));
    }

    // Private `extend` / `extend Trait` blocks are scoped to their declaring module/package
    // and never visible to downstream packages unless referenced by a retained template.
    let is_extend = matches!(kind, NodeKind::ExtendDef | NodeKind::ExtendTraitDef);
    if is_extend && !is_pub_outer {
        let any_retained = ast.multi_children(block_node).iter().any(|&m| {
            let (inner, _, _, _) = unwrap_modifiers(ast, m);
            !inner.is_null()
                && ast.node(inner).kind == NodeKind::FunctionDef
                && resolved
                    .node_symbols
                    .get(&ast.fixed_children(inner)[0])
                    .is_some_and(|s| retained_syms.contains(s))
        });
        if !any_retained {
            return Ok(None);
        }
    }

    let fixed = ast.fixed_children(block_node);
    let last_header_node = if matches!(kind, NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef) {
        fixed[1]
    } else {
        fixed[0]
    };
    let raw_header_str = canonicalize_nessa_snippet(&slice_range(
        source_map,
        subtree_span(ast, outer_item).lo(),
        subtree_span(ast, last_header_node).hi(),
    )?);
    let header_str = raw_header_str
        .trim_end()
        .trim_end_matches(['{', ':'])
        .trim_end();

    let is_trait_impl = matches!(kind, NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef);
    let mut member_lines = Vec::new();
    for &member in ast.multi_children(block_node) {
        let (inner, m_pub, m_priv, _) = unwrap_modifiers(ast, member);
        if inner.is_null() {
            continue;
        }
        match ast.node(inner).kind {
            NodeKind::Typealias | NodeKind::AssocDecl | NodeKind::AssocBinding => {
                member_lines.push(format!(
                    "    {}",
                    canonicalize_nessa_snippet(&span_slice(source_map, subtree_span(ast, member))?)
                ));
            }
            NodeKind::FunctionDef => {
                let name_node = ast.fixed_children(inner)[0];
                if !name_node.is_null()
                    && str_interner::get(ast.node(name_node).str_id) == "__init__"
                {
                    continue;
                }
                let sym_opt = (!name_node.is_null())
                    .then(|| resolved.node_symbols.get(&name_node).copied())
                    .flatten();
                let keep = is_trait_impl
                    || m_pub
                    || (!m_priv && is_pub_outer)
                    || sym_opt.is_some_and(|s| retained_syms.contains(&s));
                if keep {
                    let stub = emit_stubbed_fn(resolved, source_map, member, inner, block_scope)?;
                    member_lines.push(format!("    {stub}"));
                }
            }
            NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl => {
                let any_ret = resolved.symbols.iter().any(|s| {
                    s.def_node == inner
                        && s.scope == block_scope
                        && (s.visibility == Visibility::Public || retained_syms.contains(&s.id))
                });
                if m_pub || any_ret {
                    let stub = emit_stubbed_decl(resolved, source_map, member, inner, block_scope)?;
                    member_lines.push(format!("    {stub}"));
                }
            }
            _ => {}
        }
    }

    if !is_trait_impl && member_lines.is_empty() {
        return Ok(None);
    }

    if member_lines.is_empty() {
        Ok(Some(format!("{header_str} {{}}")))
    } else {
        Ok(Some(format!(
            "{header_str} {{\n{}\n}}",
            member_lines.join("\n")
        )))
    }
}

/// Emit a `StructDef` or `EnumDef`, stubbing any inline `FunctionDef` methods (`pub enum Either { ... pub fn unwrap_left(self) ... }`)
/// while preserving all struct fields (including `private` fields & default expressions) and enum variants verbatim!
fn emit_struct_or_enum_def(
    resolved: &ResolvedAst,
    source_map: &SourceMap,
    outer_item: NodeIndex,
    def_node: NodeIndex,
    retained_syms: &HashSet<SymbolId>,
) -> Result<String, String> {
    let ast = &resolved.ast;
    let has_inline_fns_or_decls = ast.multi_children(def_node).iter().any(|&m| {
        let (inner, _, _, _) = unwrap_modifiers(ast, m);
        !inner.is_null()
            && matches!(
                ast.node(inner).kind,
                NodeKind::FunctionDef | NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl
            )
    });
    if !has_inline_fns_or_decls {
        return Ok(canonicalize_nessa_snippet(&span_slice(
            source_map,
            subtree_span(ast, outer_item),
        )?));
    }

    let def_scope = resolved
        .scopes
        .iter()
        .find(|s| s.node == def_node)
        .map(|s| s.id)
        .unwrap_or(ScopeId::ROOT);
    let name_node = ast.fixed_children(def_node)[0];
    let raw_header = canonicalize_nessa_snippet(&slice_range(
        source_map,
        subtree_span(ast, outer_item).lo(),
        subtree_span(ast, name_node).hi(),
    )?);
    let header_str = raw_header
        .trim_end()
        .trim_end_matches(['{', ':'])
        .trim_end();

    let mut lines = Vec::new();
    for &member in ast.multi_children(def_node) {
        let (inner, m_pub, _, _) = unwrap_modifiers(ast, member);
        if inner.is_null() {
            continue;
        }
        match ast.node(inner).kind {
            NodeKind::StructField | NodeKind::EnumVariant => {
                let snip =
                    canonicalize_nessa_snippet(&span_slice(source_map, subtree_span(ast, member))?);
                let clean = snip.trim_end().trim_end_matches(',').trim_end();
                lines.push(format!("    {clean},"));
            }
            NodeKind::FunctionDef => {
                let fn_name_node = ast.fixed_children(inner)[0];
                if !fn_name_node.is_null()
                    && str_interner::get(ast.node(fn_name_node).str_id) == "__init__"
                {
                    continue;
                }
                let sym_opt = (!fn_name_node.is_null())
                    .then(|| resolved.node_symbols.get(&fn_name_node).copied())
                    .flatten();
                if m_pub || sym_opt.is_some_and(|s| retained_syms.contains(&s)) {
                    let stub = emit_stubbed_fn(resolved, source_map, member, inner, def_scope)?;
                    lines.push(format!("    {stub}"));
                }
            }
            NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl => {
                let any_ret = resolved.symbols.iter().any(|s| {
                    s.def_node == inner
                        && s.scope == def_scope
                        && (s.visibility == Visibility::Public || retained_syms.contains(&s.id))
                });
                if m_pub || any_ret {
                    let stub = emit_stubbed_decl(resolved, source_map, member, inner, def_scope)?;
                    lines.push(format!("    {stub}"));
                }
            }
            _ => {}
        }
    }

    Ok(format!("{header_str} {{\n{}\n}}", lines.join("\n")))
}

/// Synthesize canonical bodyless [`InterfaceModuleStub`]s for every module in the compiled package.
pub(crate) fn synthesize_interface_modules(
    resolved: &ResolvedAst,
    source_map: &SourceMap,
    target_root: NodeIndex,
    modules: &[ModularScopeUnit],
) -> Result<Vec<InterfaceModuleStub>, String> {
    let ast = &resolved.ast;
    let retained_syms = compute_retained_template_symbols(resolved, target_root);
    let mut stubs = Vec::with_capacity(modules.len());

    for unit in modules {
        let mut items_out: Vec<String> = Vec::new();
        for &item in ast.multi_children(unit.scope_node) {
            if item.is_null() || resolved.external_package_roots.contains(&item) {
                continue;
            }
            let (inner, is_pub, is_priv, _) = unwrap_modifiers(ast, item);
            if inner.is_null() || resolved.external_package_roots.contains(&inner) {
                continue;
            }
            match ast.node(inner).kind {
                NodeKind::UseStatement => {
                    // Omit synthetic implicit-prelude imports injected at dummy spans.
                    let sp = subtree_span(ast, item);
                    if !sp.is_dummy() {
                        items_out.push(canonicalize_nessa_snippet(&span_slice(source_map, sp)?));
                    }
                }
                NodeKind::ModuleDef => {
                    let name_node = ast.fixed_children(inner)[0];
                    if !name_node.is_null() {
                        let mname = str_interner::get(ast.node(name_node).str_id);
                        if is_pub {
                            items_out.push(format!("pub mod {mname}"));
                        } else if is_priv {
                            items_out.push(format!("private mod {mname}"));
                        } else {
                            items_out.push(format!("mod {mname}"));
                        }
                    }
                }
                NodeKind::StructDef | NodeKind::EnumDef => {
                    items_out.push(emit_struct_or_enum_def(
                        resolved,
                        source_map,
                        item,
                        inner,
                        &retained_syms,
                    )?);
                }
                NodeKind::Newtype
                | NodeKind::Typealias
                | NodeKind::TraitDef
                | NodeKind::EffectDef
                | NodeKind::AsyncEffectDef
                | NodeKind::DeriveDef => {
                    items_out.push(canonicalize_nessa_snippet(&span_slice(
                        source_map,
                        subtree_span(ast, item),
                    )?));
                }
                NodeKind::ImplDef
                | NodeKind::ImplTraitDef
                | NodeKind::ExtendDef
                | NodeKind::ExtendTraitDef => {
                    if let Some(rendered) = emit_impl_or_extend_block(
                        resolved,
                        source_map,
                        item,
                        inner,
                        is_pub,
                        &retained_syms,
                    )? {
                        items_out.push(rendered);
                    }
                }
                NodeKind::FunctionDef => {
                    let name_node = ast.fixed_children(inner)[0];
                    if !name_node.is_null()
                        && str_interner::get(ast.node(name_node).str_id) == "__init__"
                    {
                        continue;
                    }
                    let sym_opt = (!name_node.is_null())
                        .then(|| resolved.node_symbols.get(&name_node).copied())
                        .flatten();
                    if is_pub || sym_opt.is_some_and(|s| retained_syms.contains(&s)) {
                        // Synthetic `[[ffi.wasm]]` wrappers may have spans in `ffi-wasm-generated-wrappers`,
                        // which is also registered in `source_map`!
                        items_out.push(emit_stubbed_fn(
                            resolved,
                            source_map,
                            item,
                            inner,
                            unit.scope_id,
                        )?);
                    }
                }
                NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl => {
                    let keep = is_pub
                        || resolved.symbols.iter().any(|s| {
                            s.def_node == inner
                                && s.scope == unit.scope_id
                                && (s.visibility == Visibility::Public
                                    || retained_syms.contains(&s.id))
                        });
                    if keep {
                        items_out.push(emit_stubbed_decl(
                            resolved,
                            source_map,
                            item,
                            inner,
                            unit.scope_id,
                        )?);
                    }
                }
                _ => {}
            }
        }
        let mut stub_source = items_out.join("\n");
        if !stub_source.is_empty() {
            stub_source.push('\n');
        }
        stubs.push(InterfaceModuleStub {
            module_path: unit.module_path.clone(),
            stub_source,
        });
    }

    Ok(stubs)
}

fn stable_id_of(pool: &TypePool, ty: TypeIndex) -> Result<TypeId, String> {
    let canon = pool.canonical_type(ty).ok_or_else(|| {
        format!(
            "invalid TypeIndex {} while building symbol selector",
            ty.as_u32()
        )
    })?;
    pool.stable_type_id(canon).map_err(|e| {
        format!(
            "missing stable TypeId for TypeIndex {}: {e}",
            canon.as_u32()
        )
    })
}

fn non_module_owner_id(
    pool: &TypePool,
    assoc_ty: Option<TypeIndex>,
) -> Result<Option<TypeId>, String> {
    let Some(ty) = assoc_ty else {
        return Ok(None);
    };
    let canon = pool
        .canonical_type(ty)
        .ok_or_else(|| format!("invalid owner TypeIndex {}", ty.as_u32()))?;
    if matches!(pool.get(canon).kind, TypeKind::Module { .. }) {
        return Ok(None);
    }
    Ok(Some(stable_id_of(pool, canon)?))
}

fn selector_for_symbol(
    resolved: &ResolvedAst,
    pool: &TypePool,
    sym_id: SymbolId,
    module_path: &[String],
) -> Result<SymbolLinkSelector, String> {
    let sym = &resolved.symbols[sym_id.0 as usize];
    let name = str_interner::get(sym.name);
    let sc = &resolved.scopes[sym.scope.0 as usize];
    if sym.kind == SymbolKind::Function && !sc.node.is_null() {
        match resolved.ast.node(sc.node).kind {
            NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef => {
                let fixed = resolved.ast.fixed_children(sc.node);
                let denote = |n: NodeIndex| -> Option<TypeIndex> {
                    resolved
                        .node_symbols
                        .get(&n)
                        .map(|&s| resolved.symbols[s.0 as usize].type_index)
                        .or_else(|| resolved.node_types.get(&n).copied())
                };
                if let (Some(tr_ty), Some(imp_ty)) = (denote(fixed[0]), denote(fixed[1])) {
                    return Ok(SymbolLinkSelector::TraitMethod {
                        trait_type_id: stable_id_of(pool, tr_ty)?,
                        implementor_type_id: stable_id_of(pool, imp_ty)?,
                        name,
                    });
                }
            }
            NodeKind::ImplDef | NodeKind::ExtendDef => {
                if let Some(assoc_ty) = sc.assoc_type {
                    return Ok(SymbolLinkSelector::InherentMethod {
                        receiver_type_id: stable_id_of(pool, assoc_ty)?,
                        name,
                    });
                }
            }
            _ => {}
        }
    }
    let owner_type_id = non_module_owner_id(pool, sc.assoc_type)?;
    Ok(SymbolLinkSelector::ScopedSymbol {
        module_path: module_path.to_vec(),
        owner_type_id,
        name,
    })
}

/// Build [`ExportedRuntimeSymbol`] entries for a compiled package.
pub(crate) fn build_exported_symbols(
    resolved: &ResolvedAst,
    pool: &TypePool,
    codegen_output: &CodegenOutput,
    modules: &[ModularScopeUnit],
    local_exports: &[ModularLocalExport],
) -> Result<Vec<ExportedRuntimeSymbol>, String> {
    let fn_by_id: HashMap<u32, &nsbc::CompiledFunction> = codegen_output
        .functions
        .iter()
        .map(|f| (f.func_id.0, f))
        .collect();
    let mut seen_selectors = BTreeSet::new();
    let mut out = Vec::with_capacity(local_exports.len());

    for entry in local_exports {
        let mpath = &modules[entry.owner_module_index as usize].module_path;
        let (selector, target) = match &entry.origin {
            LocalExportOrigin::FunctionSymbol { symbol, func_id } => {
                let sym = &resolved.symbols[symbol.0 as usize];
                // Skip scoped `extend` private duplicates if another global selector already claimed it,
                // and skip local nested functions inside other functions (whose parent scope is a `FunctionDef`).
                let parent_node = resolved.scopes[sym.scope.0 as usize].node;
                if !parent_node.is_null()
                    && resolved.ast.node(parent_node).kind == NodeKind::FunctionDef
                {
                    continue;
                }
                let sel = selector_for_symbol(resolved, pool, *symbol, mpath)?;
                let cf = fn_by_id.get(&func_id.0).ok_or_else(|| {
                    format!("missing compiled function #{} for export", func_id.0)
                })?;
                (
                    sel,
                    ExportTarget::Function {
                        func_id: *func_id,
                        function_type: cf.function_type,
                        param_count: cf.param_count,
                        abi: cf.abi.clone(),
                        display_owner: cf.display_owner,
                    },
                )
            }
            LocalExportOrigin::DefaultMethod {
                implementor,
                trait_type,
                method_name,
                func_id,
            }
            | LocalExportOrigin::DerivedMethod {
                implementor,
                trait_type,
                method_name,
                func_id,
            } => {
                let sel = SymbolLinkSelector::TraitMethod {
                    trait_type_id: stable_id_of(pool, *trait_type)?,
                    implementor_type_id: stable_id_of(pool, *implementor)?,
                    name: str_interner::get(*method_name),
                };
                let cf = fn_by_id.get(&func_id.0).ok_or_else(|| {
                    format!("missing compiled function #{} for export", func_id.0)
                })?;
                (
                    sel,
                    ExportTarget::Function {
                        func_id: *func_id,
                        function_type: cf.function_type,
                        param_count: cf.param_count,
                        abi: cf.abi.clone(),
                        display_owner: cf.display_owner,
                    },
                )
            }
            LocalExportOrigin::DefaultHelper {
                implementor,
                trait_type,
                method_name,
                nested_symbol,
                func_id,
            } => {
                let sel = SymbolLinkSelector::DefaultTraitHelper {
                    trait_type_id: stable_id_of(pool, *trait_type)?,
                    implementor_type_id: stable_id_of(pool, *implementor)?,
                    method_name: str_interner::get(*method_name),
                    nested_name: str_interner::get(resolved.symbols[nested_symbol.0 as usize].name),
                };
                let cf = fn_by_id.get(&func_id.0).ok_or_else(|| {
                    format!("missing compiled function #{} for export", func_id.0)
                })?;
                (
                    sel,
                    ExportTarget::Function {
                        func_id: *func_id,
                        function_type: cf.function_type,
                        param_count: cf.param_count,
                        abi: cf.abi.clone(),
                        display_owner: cf.display_owner,
                    },
                )
            }
            LocalExportOrigin::DerivedJsonFrom {
                implementor,
                func_id,
            } => {
                let sel = SymbolLinkSelector::DerivedJsonFrom {
                    implementor_type_id: stable_id_of(pool, *implementor)?,
                };
                let cf = fn_by_id.get(&func_id.0).ok_or_else(|| {
                    format!("missing compiled function #{} for export", func_id.0)
                })?;
                (
                    sel,
                    ExportTarget::Function {
                        func_id: *func_id,
                        function_type: cf.function_type,
                        param_count: cf.param_count,
                        abi: cf.abi.clone(),
                        display_owner: cf.display_owner,
                    },
                )
            }
            LocalExportOrigin::DerivedJsonHelper {
                implementor,
                is_write,
                func_id,
            } => {
                let sel = SymbolLinkSelector::DerivedJsonHelper {
                    implementor_type_id: stable_id_of(pool, *implementor)?,
                    is_write: *is_write,
                };
                let cf = fn_by_id.get(&func_id.0).ok_or_else(|| {
                    format!("missing compiled function #{} for export", func_id.0)
                })?;
                (
                    sel,
                    ExportTarget::Function {
                        func_id: *func_id,
                        function_type: cf.function_type,
                        param_count: cf.param_count,
                        abi: cf.abi.clone(),
                        display_owner: cf.display_owner,
                    },
                )
            }
            LocalExportOrigin::Global {
                symbol,
                global_id,
                type_index,
                is_mutable,
            } => {
                let sel = selector_for_symbol(resolved, pool, *symbol, mpath)?;
                (
                    sel,
                    ExportTarget::Global {
                        global_id: *global_id,
                        type_index: *type_index,
                        is_mutable: *is_mutable,
                    },
                )
            }
            LocalExportOrigin::Type { symbol, type_index } => {
                let sel = selector_for_symbol(resolved, pool, *symbol, mpath)?;
                let tid = stable_id_of(pool, *type_index)?;
                (
                    sel,
                    ExportTarget::Type {
                        type_index: *type_index,
                        type_id: tid,
                    },
                )
            }
        };

        if seen_selectors.insert(selector.clone()) {
            out.push(ExportedRuntimeSymbol {
                selector,
                owner_module_index: entry.owner_module_index,
                target,
            });
        }
    }

    Ok(out)
}

/// Build [`ImportedFunction`] and [`ImportedGlobal`] descriptors from compacted modular NIR references.
pub(crate) fn build_imported_descriptors(
    resolved: &ResolvedAst,
    pool: &TypePool,
    pkg_index_by_root: &HashMap<NodeIndex, u32>,
    dep_exports_by_pkg_index: &HashMap<u32, HashMap<SymbolLinkSelector, &ExportTarget>>,
    imported_functions: &[ModularImportedFunction],
    imported_globals: &[ModularImportedGlobal],
) -> Result<(Vec<ImportedFunction>, Vec<ImportedGlobal>), String> {
    let mut out_fns = Vec::with_capacity(imported_functions.len());
    for imp in imported_functions {
        let &pkg_idx = pkg_index_by_root
            .get(&imp.package_root)
            .ok_or_else(|| "missing package_index for imported function root".to_owned())?;
        let selector = match &imp.origin {
            ExternalFunctionOrigin::Symbol(sym) => {
                selector_for_symbol(resolved, pool, *sym, &imp.module_path)?
            }
            ExternalFunctionOrigin::DefaultMethod {
                implementor,
                trait_type,
                method_name,
                ..
            }
            | ExternalFunctionOrigin::DerivedMethod {
                implementor,
                trait_type,
                method_name,
                ..
            } => SymbolLinkSelector::TraitMethod {
                trait_type_id: stable_id_of(pool, *trait_type)?,
                implementor_type_id: stable_id_of(pool, *implementor)?,
                name: str_interner::get(*method_name),
            },
            ExternalFunctionOrigin::DefaultHelper {
                implementor,
                trait_type,
                method_name,
                nested_symbol,
                ..
            } => SymbolLinkSelector::DefaultTraitHelper {
                trait_type_id: stable_id_of(pool, *trait_type)?,
                implementor_type_id: stable_id_of(pool, *implementor)?,
                method_name: str_interner::get(*method_name),
                nested_name: str_interner::get(resolved.symbols[nested_symbol.0 as usize].name),
            },
            ExternalFunctionOrigin::DerivedJsonFrom { implementor, .. } => {
                SymbolLinkSelector::DerivedJsonFrom {
                    implementor_type_id: stable_id_of(pool, *implementor)?,
                }
            }
            ExternalFunctionOrigin::DerivedJsonHelper {
                implementor,
                is_write,
                ..
            } => SymbolLinkSelector::DerivedJsonHelper {
                implementor_type_id: stable_id_of(pool, *implementor)?,
                is_write: *is_write,
            },
        };

        // Match exact ABI (`param_count`, `ParameterAbi` view mapped into `pool`, `display_owner`)
        // from the dependency's exported `ExportTarget::Function`!
        let dep_target = dep_exports_by_pkg_index
            .get(&pkg_idx)
            .and_then(|m| m.get(&selector))
            .copied()
            .ok_or_else(|| {
                format!(
                    "dependency package #{pkg_idx} does not export required function {selector:?}"
                )
            })?;
        let ExportTarget::Function {
            param_count: dep_param_count,
            abi: dep_abi,
            display_owner: dep_display_owner,
            ..
        } = dep_target
        else {
            return Err(format!(
                "dependency symbol {selector:?} is not exported as a function"
            ));
        };

        let function_type = match &imp.origin {
            ExternalFunctionOrigin::Symbol(sym) => resolved.symbols[sym.0 as usize].type_index,
            ExternalFunctionOrigin::DefaultMethod { signature, .. }
            | ExternalFunctionOrigin::DefaultHelper { signature, .. }
            | ExternalFunctionOrigin::DerivedMethod { signature, .. }
            | ExternalFunctionOrigin::DerivedJsonFrom { signature, .. }
            | ExternalFunctionOrigin::DerivedJsonHelper { signature, .. } => *signature,
        };

        let map_view_from_sig =
            |pos_idx: usize, fallback_tid: Option<TypeId>| -> Result<TypeIndex, String> {
                if let Some(tid) = fallback_tid
                    && let Some(local_ty) = pool.lookup_by_id(tid)
                {
                    return Ok(local_ty);
                }
                if let Some(csig) = pool.canonical_type(function_type)
                    && let TypeKind::Function { params, .. } = &pool.get(csig).kind
                    && let Some(&pty) = params.get(pos_idx)
                {
                    return Ok(pty);
                }
                Err(format!(
                    "cannot map trait parameter ABI view for imported function {selector:?}"
                ))
            };

        let mut mapped_params = Vec::with_capacity(dep_abi.parameters.len());
        for (lidx, pabi) in dep_abi.parameters.iter().enumerate() {
            match pabi {
                nsbc::ParameterAbi::Value => mapped_params.push(nsbc::ParameterAbi::Value),
                nsbc::ParameterAbi::Trait { .. } => {
                    let view = map_view_from_sig(lidx, None)?;
                    mapped_params.push(nsbc::ParameterAbi::Trait { view });
                }
                nsbc::ParameterAbi::TraitSelf { .. } => {
                    let trait_tid = match &selector {
                        SymbolLinkSelector::TraitMethod { trait_type_id, .. }
                        | SymbolLinkSelector::DefaultTraitHelper { trait_type_id, .. } => {
                            Some(*trait_type_id)
                        }
                        _ => None,
                    };
                    let view = map_view_from_sig(lidx, trait_tid)?;
                    mapped_params.push(nsbc::ParameterAbi::TraitSelf { view });
                }
            }
        }

        let display_owner = match (&imp.origin, dep_display_owner) {
            (
                ExternalFunctionOrigin::DerivedMethod {
                    implementor,
                    is_display: true,
                    ..
                },
                _,
            ) => Some(*implementor),
            (_, Some(_)) => match &selector {
                SymbolLinkSelector::TraitMethod {
                    implementor_type_id,
                    ..
                } => pool.lookup_by_id(*implementor_type_id),
                _ => None,
            },
            _ => None,
        };

        out_fns.push(ImportedFunction {
            package_index: pkg_idx,
            selector,
            function_type,
            param_count: *dep_param_count,
            abi: nsbc::FunctionAbi {
                captures: Vec::new(),
                parameters: mapped_params,
            },
            display_owner,
        });
    }

    let mut out_globs = Vec::with_capacity(imported_globals.len());
    for g in imported_globals {
        let &pkg_idx = pkg_index_by_root
            .get(&g.package_root)
            .ok_or_else(|| "missing package_index for imported global root".to_owned())?;
        let sym = &resolved.symbols[g.symbol.0 as usize];
        let sc = &resolved.scopes[sym.scope.0 as usize];
        let owner_type_id = non_module_owner_id(pool, sc.assoc_type)?;
        out_globs.push(ImportedGlobal {
            package_index: pkg_idx,
            module_path: g.module_path.clone(),
            owner_type_id,
            name: str_interner::get(sym.name),
            type_index: g.type_index,
            is_mutable: g.is_mutable,
        });
    }

    Ok((out_fns, out_globs))
}

/// Compute a deterministic SHA-256 digest of a package's public interface (`qualified_name`,
/// `version`, `identity`, `manifest_toml`, and `interface_modules`).
///
/// Implementation-only edits inside function bodies or private helpers leave `interface_hash`
/// completely unchanged, avoiding cascading downstream recompilations!
pub(crate) fn compute_interface_hash(
    qualified_name: &str,
    version: &str,
    identity: &[u8; 16],
    manifest_toml: &str,
    interface_modules: &[InterfaceModuleStub],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"nessa.modular.interface.v1\0");
    hasher.update((qualified_name.len() as u64).to_le_bytes());
    hasher.update(qualified_name.as_bytes());
    hasher.update((version.len() as u64).to_le_bytes());
    hasher.update(version.as_bytes());
    hasher.update(identity);
    hasher.update((manifest_toml.len() as u64).to_le_bytes());
    hasher.update(manifest_toml.as_bytes());
    hasher.update((interface_modules.len() as u64).to_le_bytes());
    for stub in interface_modules {
        hasher.update((stub.module_path.len() as u64).to_le_bytes());
        for seg in &stub.module_path {
            hasher.update((seg.len() as u64).to_le_bytes());
            hasher.update(seg.as_bytes());
        }
        hasher.update((stub.stub_source.len() as u64).to_le_bytes());
        hasher.update(stub.stub_source.as_bytes());
    }
    hasher.finalize().into()
}
