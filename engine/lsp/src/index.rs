//! Resilient semantic index, Nessa signature formatting, doc comment extraction,
//! member/trait-impl tables, and LSP 3.17 semantic token encoding.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ast::{Ast, NodeIndex, NodeKind};
use driver::{CheckedSourceUnit, IdeCheckResult};
use lexer::TokenKind;
use resolution::{ResolvedAst, ScopeId, SymbolId, SymbolKind, Visibility};
use rustc_span::Span;
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind, TypePool};

use crate::position::LineIndex;

/// LSP 3.17 `SymbolKind` numeric constants.
pub mod lsp_symbol_kind {
    pub const FILE: u32 = 1;
    pub const MODULE: u32 = 2;
    pub const NAMESPACE: u32 = 3;
    pub const PACKAGE: u32 = 4;
    pub const CLASS: u32 = 5;
    pub const METHOD: u32 = 6;
    pub const PROPERTY: u32 = 7;
    pub const FIELD: u32 = 8;
    pub const CONSTRUCTOR: u32 = 9;
    pub const ENUM: u32 = 10;
    pub const INTERFACE: u32 = 11;
    pub const FUNCTION: u32 = 12;
    pub const VARIABLE: u32 = 13;
    pub const CONSTANT: u32 = 14;
    pub const ENUM_MEMBER: u32 = 22;
    pub const STRUCT: u32 = 23;
    pub const EVENT: u32 = 24;
    pub const OPERATOR: u32 = 25;
    pub const TYPE_PARAMETER: u32 = 26;
}

/// LSP 3.17 `CompletionItemKind` numeric constants.
pub mod lsp_completion_kind {
    pub const TEXT: u32 = 1;
    pub const METHOD: u32 = 2;
    pub const FUNCTION: u32 = 3;
    pub const CONSTRUCTOR: u32 = 4;
    pub const FIELD: u32 = 5;
    pub const VARIABLE: u32 = 6;
    pub const CLASS: u32 = 7;
    pub const INTERFACE: u32 = 8;
    pub const MODULE: u32 = 9;
    pub const PROPERTY: u32 = 10;
    pub const ENUM: u32 = 13;
    pub const KEYWORD: u32 = 14;
    pub const SNIPPET: u32 = 15;
    pub const ENUM_MEMBER: u32 = 20;
    pub const CONSTANT: u32 = 21;
    pub const STRUCT: u32 = 22;
    pub const EVENT: u32 = 23;
    pub const TYPE_PARAMETER: u32 = 25;
}

/// Canonical Semantic Token type indices advertised in `initialize`.
pub mod semantic_token_type {
    pub const NAMESPACE: u32 = 0;
    pub const TYPE: u32 = 1;
    pub const STRUCT: u32 = 2;
    pub const ENUM: u32 = 3;
    pub const INTERFACE: u32 = 4;
    pub const TYPE_PARAMETER: u32 = 5;
    pub const PARAMETER: u32 = 6;
    pub const VARIABLE: u32 = 7;
    pub const PROPERTY: u32 = 8;
    pub const ENUM_MEMBER: u32 = 9;
    pub const FUNCTION: u32 = 10;
    pub const METHOD: u32 = 11;
    pub const KEYWORD: u32 = 12;
    pub const COMMENT: u32 = 13;
    pub const STRING: u32 = 14;
    pub const NUMBER: u32 = 15;
    pub const OPERATOR: u32 = 16;
    pub const DECORATOR: u32 = 17;

    pub const LEGEND: &[&str] = &[
        "namespace",
        "type",
        "struct",
        "enum",
        "interface",
        "typeParameter",
        "parameter",
        "variable",
        "property",
        "enumMember",
        "function",
        "method",
        "keyword",
        "comment",
        "string",
        "number",
        "operator",
        "decorator",
    ];
}

/// Canonical Semantic Token modifier bit indices advertised in `initialize`.
pub mod semantic_token_modifier {
    pub const DECLARATION: u32 = 1 << 0;
    pub const DEFINITION: u32 = 1 << 1;
    pub const READONLY: u32 = 1 << 2;
    pub const MUTABLE: u32 = 1 << 3;
    pub const MODIFICATION: u32 = 1 << 4;
    pub const DEFAULT_LIBRARY: u32 = 1 << 5;
    pub const ASYNC: u32 = 1 << 6;

    pub const LEGEND: &[&str] = &[
        "declaration",
        "definition",
        "readonly",
        "Mutable",
        "modification",
        "defaultLibrary",
        "async",
    ];
}

/// Unique symbol key within a [`SemanticIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKey {
    Sym(SymbolId),
    ImplBlock(NodeIndex),
}

/// Parameter descriptor for signature help and hover formatting.
#[derive(Debug, Clone)]
pub struct ParamDoc {
    pub name: Option<String>,
    pub label: String,
    pub is_self: bool,
    pub is_optional: bool,
    pub is_varargs: bool,
}

/// Rich metadata for a symbol or outline container in [`SemanticIndex`].
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub key: SymbolKey,
    pub name: String,
    pub kind: u32,
    pub completion_kind: u32,
    pub detail: String,
    pub doc: Option<String>,
    pub container_name: Option<String>,
    pub file: PathBuf,
    pub full_span: (usize, usize),
    pub selection_span: (usize, usize),
    pub children: Vec<SymbolKey>,
    pub params: Vec<ParamDoc>,
    pub type_index: Option<TypeIndex>,
    pub return_type: Option<TypeIndex>,
    pub type_def_target: Option<SymbolKey>,
    pub trait_owner: Option<TypeIndex>,
    pub is_external: bool,
    pub is_mutable: bool,
    pub is_async: bool,
    pub is_synthesized_module: bool,
    pub has_self_receiver: bool,
}

/// A resolved identifier occurrence in a source file.
#[derive(Debug, Clone)]
pub struct Occurrence {
    pub span: (usize, usize),
    pub target: SymbolKey,
    pub is_decl: bool,
    pub is_write: bool,
    /// Optional narrowed or concrete expression type at this occurrence site.
    pub site_type_detail: Option<String>,
    pub site_type_def: Option<SymbolKey>,
}

/// Member attached to a nominal type or module (`struct`, `enum`, `trait`, `mod`, `std` types).
#[derive(Debug, Clone)]
pub struct MemberEntry {
    pub name: String,
    pub key: Option<SymbolKey>,
    pub kind: u32,
    pub completion_kind: u32,
    pub detail: String,
    pub doc: Option<String>,
    pub is_static: bool,
    pub result_type: Option<TypeIndex>,
}

/// Recorded `impl Trait for T`, `extend Trait for T`, or `derive Trait for T`.
#[derive(Debug, Clone)]
pub struct TraitImplEntry {
    pub trait_type: TypeIndex,
    pub implementor: TypeIndex,
    pub file: PathBuf,
    pub span: (usize, usize),
    /// Method name -> concrete implementation `SymbolKey`.
    pub methods: HashMap<String, SymbolKey>,
}

/// Lexical scope range in a file for position-sensitive scope completion and dot-receiver lookup.
#[derive(Debug, Clone)]
pub struct FileScopeSpan {
    pub span: (usize, usize),
    pub bindings: Vec<(String, SymbolKey)>,
}

/// Pre-resolved receiver metadata for dot-expressions (`expr.member`) in a file.
#[derive(Debug, Clone)]
pub struct ProjectionSite {
    pub receiver_span: (usize, usize),
    pub dot_offset: usize,
    pub receiver_type: Option<TypeIndex>,
    pub receiver_sym: Option<SymbolKey>,
    pub is_type_or_module_receiver: bool,
}

/// Complete indexed semantic snapshot of an IDE check.
#[derive(Debug, Clone, Default)]
pub struct SemanticIndex {
    pub symbols: HashMap<SymbolKey, SymbolInfo>,
    pub file_roots: HashMap<PathBuf, Vec<SymbolKey>>,
    pub file_occurrences: HashMap<PathBuf, Vec<Occurrence>>,
    pub file_scopes: HashMap<PathBuf, Vec<FileScopeSpan>>,
    pub file_projections: HashMap<PathBuf, Vec<ProjectionSite>>,
    pub type_defs: HashMap<TypeIndex, SymbolKey>,
    pub type_members: HashMap<TypeIndex, Vec<MemberEntry>>,
    pub type_names: HashMap<TypeIndex, String>,
    pub tuple_elements: HashMap<TypeIndex, Vec<(TypeIndex, String)>>,
    pub optional_inners: HashMap<TypeIndex, (TypeIndex, String)>,
    pub error_inners: HashMap<TypeIndex, (TypeIndex, String)>,
    pub future_inners: HashMap<TypeIndex, (TypeIndex, String)>,
    pub trait_impls: HashMap<TypeIndex, Vec<TraitImplEntry>>,
    pub std_module_sym: Option<SymbolKey>,
    pub package_modules: Vec<(String, SymbolKey)>,
    pub prelude_symbols: Vec<(String, SymbolKey)>,
    pub privileged_files: HashSet<PathBuf>,
}

impl SemanticIndex {
    pub fn symbol(&self, key: SymbolKey) -> Option<&SymbolInfo> {
        self.symbols.get(&key)
    }

    pub fn occurrence_at(&self, file: &Path, offset: usize) -> Option<&Occurrence> {
        let occs = self.file_occurrences.get(file)?;
        let end = occs.partition_point(|o| o.span.0 <= offset);
        let start = end.saturating_sub(32);
        let mut best: Option<&Occurrence> = None;
        for occ in &occs[start..end] {
            let (lo, hi) = occ.span;
            if lo <= offset && offset <= hi && hi > lo {
                match best {
                    None => best = Some(occ),
                    Some(prev) => {
                        let prev_len = prev.span.1.saturating_sub(prev.span.0);
                        let cur_len = hi.saturating_sub(lo);
                        let prev_interior = offset < prev.span.1;
                        let cur_interior = offset < hi;
                        if (cur_interior && !prev_interior)
                            || (cur_interior == prev_interior && cur_len <= prev_len)
                        {
                            best = Some(occ);
                        }
                    }
                }
            }
        }
        best
    }

    pub fn exact_occurrence_at(&self, file: &Path, span: (usize, usize)) -> Option<&Occurrence> {
        let occs = self.file_occurrences.get(file)?;
        let idx = occs.partition_point(|o| o.span < span);
        occs.get(idx).filter(|o| o.span == span)
    }

    pub fn references_to(
        &self,
        target: SymbolKey,
        include_decl: bool,
    ) -> Vec<(PathBuf, (usize, usize))> {
        let mut results = Vec::new();
        let mut files: Vec<_> = self.file_occurrences.keys().cloned().collect();
        files.sort();
        for file in files {
            if let Some(occs) = self.file_occurrences.get(&file) {
                for occ in occs {
                    if occ.target == target && (include_decl || !occ.is_decl) {
                        results.push((file.clone(), occ.span));
                    }
                }
            }
        }
        results.dedup();
        results
    }

    pub fn highlights_in_file(&self, file: &Path, target: SymbolKey) -> Vec<&Occurrence> {
        let Some(occs) = self.file_occurrences.get(file) else {
            return Vec::new();
        };
        occs.iter().filter(|o| o.target == target).collect()
    }

    /// Look up visible lexical bindings at `offset` inside `file`, from innermost scope outward.
    pub fn visible_bindings_at(&self, file: &Path, offset: usize) -> Vec<(String, SymbolKey)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        if let Some(scopes) = self.file_scopes.get(file) {
            let mut matching: Vec<&FileScopeSpan> = scopes
                .iter()
                .filter(|s| s.span.0 <= offset && offset <= s.span.1)
                .collect();
            matching.sort_by_key(|s| s.span.1.saturating_sub(s.span.0));
            for sc in matching {
                for (name, key) in &sc.bindings {
                    if let Some(sym) = self.symbol(*key) {
                        // Local variables/parameters declared strictly after cursor in a block aren't in scope yet.
                        if matches!(
                            sym.kind,
                            lsp_symbol_kind::VARIABLE | lsp_symbol_kind::CONSTANT
                        ) && sym.file == file
                            && sym.selection_span.0 > offset
                            && sc.span.0 > 0
                        {
                            continue;
                        }
                    }
                    if seen.insert(name.clone()) {
                        out.push((name.clone(), *key));
                    }
                }
            }
        }
        for (name, key) in &self.prelude_symbols {
            if seen.insert(name.clone()) {
                out.push((name.clone(), *key));
            }
        }
        out
    }

    /// Merge per-file information from an older `last_good` index for `file` if needed.
    pub fn lookup_named_symbol_in_file(
        &self,
        file: &Path,
        name: &str,
        offset: usize,
    ) -> Option<&SymbolInfo> {
        for (bname, key) in self.visible_bindings_at(file, offset) {
            if bname == name {
                return self.symbol(key);
            }
        }
        self.symbols
            .values()
            .find(|s| s.file == file && s.name == name)
            .or_else(|| {
                self.symbols
                    .values()
                    .find(|s| !s.is_external && s.name == name)
            })
    }
}

struct UnitMapped {
    path: PathBuf,
    is_external: bool,
}

/// Build a [`SemanticIndex`] from an [`IdeCheckResult`].
pub fn build_semantic_index<F>(
    check: &IdeCheckResult,
    mut resolve_unit_path: F,
    privileged_files: HashSet<PathBuf>,
) -> SemanticIndex
where
    F: FnMut(&CheckedSourceUnit) -> PathBuf,
{
    let mut index = SemanticIndex {
        privileged_files,
        ..SemanticIndex::default()
    };
    let Some(resolved) = check.resolved.as_ref() else {
        return index;
    };

    let unit_paths: Vec<UnitMapped> = check
        .units
        .iter()
        .map(|u| UnitMapped {
            path: resolve_unit_path(u),
            is_external: u.is_std || u.is_bundled,
        })
        .collect();

    let locate = |span: Span| -> Option<(usize, &CheckedSourceUnit, &UnitMapped, (usize, usize))> {
        if span.is_dummy() {
            return None;
        }
        let part = check.units.partition_point(|u| u.start_pos <= span.lo());
        for cand in [part.checked_sub(1), Some(part)].into_iter().flatten() {
            if let Some(unit) = check.units.get(cand)
                && let Some(raw) = unit.original_range(span)
            {
                return Some((
                    cand,
                    unit,
                    &unit_paths[cand],
                    (raw.start as usize, raw.end as usize),
                ));
            }
        }
        None
    };

    let effects_by_ty: HashMap<TypeIndex, (String, bool)> = resolved
        .effects
        .iter()
        .map(|e| {
            let canon = resolved
                .type_pool
                .canonical_type(e.type_index)
                .unwrap_or(e.type_index);
            (canon, (str_interner::get(e.name), e.is_async))
        })
        .collect();

    // Precompute parent wrapper spans (`PubDef` / `PrivateDef` / `LetDecl` / `ConstDecl` / `VarDecl` / `GlobalDecl`)
    // and write-target AST nodes (`Assign`, `+=`, etc.).
    let ast = &resolved.ast;
    let mut wrapper_node: HashMap<NodeIndex, NodeIndex> = HashMap::new();
    let mut decl_parent: HashMap<NodeIndex, (NodeIndex, NodeKind)> = HashMap::new();
    let mut write_nodes: HashSet<NodeIndex> = HashSet::new();
    let mut parent_container: HashMap<NodeIndex, String> = HashMap::new();

    for idx in 1..ast.nodes.len() {
        let node_idx = NodeIndex(idx as u32);
        let kind = ast.node(node_idx).kind;
        match kind {
            NodeKind::PubDef | NodeKind::PrivateDef => {
                let inner = ast.fixed_children(node_idx)[0];
                if !inner.is_null() {
                    wrapper_node.insert(inner, node_idx);
                }
            }
            NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl => {
                let outer = wrapper_node.get(&node_idx).copied().unwrap_or(node_idx);
                let pat = ast.fixed_children(node_idx)[0];
                record_pattern_decl_parents(ast, pat, outer, kind, &mut decl_parent);
            }
            NodeKind::GlobalDecl => {
                let outer = wrapper_node.get(&node_idx).copied().unwrap_or(node_idx);
                let inner = ast.fixed_children(node_idx)[0];
                if !inner.is_null()
                    && matches!(
                        ast.node(inner).kind,
                        NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl
                    )
                {
                    let pat = ast.fixed_children(inner)[0];
                    record_pattern_decl_parents(
                        ast,
                        pat,
                        outer,
                        NodeKind::GlobalDecl,
                        &mut decl_parent,
                    );
                }
            }
            NodeKind::Assign
            | NodeKind::AddAssign
            | NodeKind::SubAssign
            | NodeKind::MulAssign
            | NodeKind::DivAssign
            | NodeKind::ModAssign => {
                let lhs = ast.fixed_children(node_idx)[0];
                if !lhs.is_null() {
                    if ast.node(lhs).kind == NodeKind::Projection {
                        let mem = ast.fixed_children(lhs)[1];
                        if !mem.is_null() {
                            write_nodes.insert(mem);
                        }
                    } else {
                        write_nodes.insert(lhs);
                    }
                }
            }
            NodeKind::ModuleDef | NodeKind::StructDef | NodeKind::EnumDef | NodeKind::TraitDef => {
                let cname = str_interner::get(ast.node(ast.fixed_children(node_idx)[0]).str_id);
                if !cname.is_empty() {
                    for &member in ast.multi_children(node_idx) {
                        let inner = unwrap_vis(ast, member);
                        parent_container.insert(inner, cname.clone());
                    }
                }
            }
            NodeKind::ImplDef | NodeKind::ExtendDef => {
                let ty_node = ast.fixed_children(node_idx)[0];
                let cname =
                    node_source_label(ast, &locate, ty_node).unwrap_or_else(|| "impl".to_owned());
                for &member in ast.multi_children(node_idx) {
                    let inner = unwrap_vis(ast, member);
                    parent_container.insert(inner, cname.clone());
                }
            }
            NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef => {
                let tr_node = ast.fixed_children(node_idx)[0];
                let ty_node = ast.fixed_children(node_idx)[1];
                let tr_str = node_source_label(ast, &locate, tr_node).unwrap_or_default();
                let ty_str = node_source_label(ast, &locate, ty_node).unwrap_or_default();
                let cname = format!("{tr_str} for {ty_str}");
                for &member in ast.multi_children(node_idx) {
                    let inner = unwrap_vis(ast, member);
                    parent_container.insert(inner, cname.clone());
                }
            }
            _ => {}
        }
    }

    // Cache formatted names & structural shapes for types in TypePool.
    for raw_idx in 0..resolved.type_pool.len() {
        let ti = TypeIndex::from_raw(raw_idx as u32);
        let canon = resolved.type_pool.canonical_type(ti).unwrap_or(ti);
        let formatted = format_type(&resolved.type_pool, &effects_by_ty, ti);
        index.type_names.insert(ti, formatted);
        match &resolved.type_pool.get(canon).kind {
            TypeKind::Tuple { elements } => {
                let elems = elements
                    .iter()
                    .map(|&e| (e, format_type(&resolved.type_pool, &effects_by_ty, e)))
                    .collect();
                index.tuple_elements.insert(canon, elems);
            }
            TypeKind::Optional { inner } => {
                index.optional_inners.insert(
                    canon,
                    (
                        *inner,
                        format_type(&resolved.type_pool, &effects_by_ty, *inner),
                    ),
                );
            }
            TypeKind::ErrorQualified { inner, .. } => {
                index.error_inners.insert(
                    canon,
                    (
                        *inner,
                        format_type(&resolved.type_pool, &effects_by_ty, *inner),
                    ),
                );
            }
            TypeKind::Future { inner } => {
                index.future_inners.insert(
                    canon,
                    (
                        *inner,
                        format_type(&resolved.type_pool, &effects_by_ty, *inner),
                    ),
                );
            }
            _ => {}
        }
    }

    // Pre-build O(1) lookup tables for associated scopes, fields/variants, and enum tags.
    let assoc_lookup = AssocSymbolLookup::new(resolved);

    // Pass 1: Register symbols in `resolved.symbols` and populate `type_defs`.
    for sym in &resolved.symbols {
        let key = SymbolKey::Sym(sym.id);
        let name = str_interner::get(sym.name);
        if name.is_empty() || sym.def_node.is_null() {
            continue;
        }
        let def_node = sym.def_node;
        let def_kind = ast.node(def_node).kind;
        let outer_node = wrapper_node
            .get(&def_node)
            .copied()
            .or_else(|| decl_parent.get(&def_node).map(|(p, _)| *p))
            .unwrap_or(def_node);

        let (sel_node, is_synth_mod) = selection_node_for_def(ast, def_node);
        let located_full =
            locate(ast.node(outer_node).span).or_else(|| locate(ast.node(def_node).span));
        let Some((_, unit, mapped, raw_full_span)) = located_full else {
            continue;
        };
        // Skip function-local variables, parameters, and labels inside external `std` / bundled bodies:
        // external function bodies are never indexed into `file_occurrences` or `file_scopes`.
        if mapped.is_external
            && matches!(
                sym.kind,
                SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Label
            )
            && resolved
                .scopes
                .get(sym.scope.0 as usize)
                .is_some_and(|sc| sc.assoc_type.is_none() && sc.node != ast.root)
        {
            continue;
        }
        let full_span = if is_synth_mod {
            raw_full_span
        } else {
            (
                trim_leading_trivia_offset(&unit.source, raw_full_span.0, raw_full_span.1),
                raw_full_span.1,
            )
        };
        let selection_span = if is_synth_mod {
            (0, 0)
        } else if let Some((_, _, sel_mapped, sel_range)) = locate(ast.node(sel_node).span)
            && sel_mapped.path == mapped.path
        {
            sel_range
        } else {
            full_span
        };

        let is_trait_ty = sym.type_index != TypeIndex::INVALID
            && resolved
                .type_pool
                .canonical_type(sym.type_index)
                .is_some_and(|c| matches!(resolved.type_pool.get(c).kind, TypeKind::Trait { .. }));

        let has_self = matches!(
            def_kind,
            NodeKind::FunctionDef | NodeKind::TraitDefFn | NodeKind::TraitDeriveFn
        ) && ast
            .multi_children(def_node)
            .first()
            .is_some_and(|&p| ast.node(p).kind == NodeKind::ParamSelf);

        let encl_assoc_ty = resolved
            .scopes
            .get(sym.scope.0 as usize)
            .and_then(|sc| sc.assoc_type);

        let is_method = matches!(def_kind, NodeKind::TraitDefFn | NodeKind::TraitDeriveFn)
            || (def_kind == NodeKind::FunctionDef && (has_self || encl_assoc_ty.is_some()));

        let (lsp_kind, comp_kind) = classify_symbol_kinds(
            sym.kind,
            def_kind,
            &resolved.type_pool,
            sym.type_index,
            is_trait_ty,
            is_method,
        );

        let is_mutable = decl_parent
            .get(&def_node)
            .is_some_and(|(_, k)| matches!(k, NodeKind::VarDecl | NodeKind::GlobalDecl));

        let (detail, params, ret_ty, is_async) = format_symbol_signature(
            resolved,
            sym,
            def_node,
            &effects_by_ty,
            &locate,
            decl_parent.get(&def_node).map(|(_, k)| *k),
        );

        // Local variables, parameters, and labels inside blocks do not carry doc comments;
        // extract doc comments only for top-level / member / type declarations.
        let doc = if is_synth_mod {
            extract_file_header_doc(&unit.source)
        } else if matches!(sym.kind, SymbolKind::Parameter | SymbolKind::Label) {
            None
        } else if selection_span.0 == full_span.0 {
            extract_leading_doc(&unit.source, selection_span.0)
        } else {
            extract_leading_doc(&unit.source, selection_span.0)
                .or_else(|| extract_leading_doc(&unit.source, full_span.0))
        };

        let trait_owner = encl_assoc_ty.and_then(|ty| {
            let canon = resolved.type_pool.canonical_type(ty)?;
            matches!(resolved.type_pool.get(canon).kind, TypeKind::Trait { .. }).then_some(canon)
        });

        let canon_ty = (sym.type_index != TypeIndex::INVALID)
            .then(|| resolved.type_pool.canonical_type(sym.type_index))
            .flatten();

        if matches!(
            def_kind,
            NodeKind::StructDef
                | NodeKind::EnumDef
                | NodeKind::TraitDef
                | NodeKind::Newtype
                | NodeKind::EffectDef
                | NodeKind::AsyncEffectDef
                | NodeKind::ModuleDef
        ) && let Some(cty) = canon_ty
        {
            index.type_defs.entry(cty).or_insert(key);
        } else if def_kind == NodeKind::Typealias
            && !mapped.is_external
            && let Some(cty) = canon_ty
        {
            index.type_defs.entry(cty).or_insert(key);
        } else if def_kind == NodeKind::Typealias
            && mapped.is_external
            && let Some(cty) = canon_ty
        {
            // `std/builtin.ns` defines `pub typealias Display = .Display'builtin`, `List`, etc.
            index.type_defs.entry(cty).or_insert(key);
        }

        if name == "std" && sym.kind == SymbolKind::Module && unit.is_std {
            index.std_module_sym = Some(key);
        }

        let info = SymbolInfo {
            key,
            name,
            kind: lsp_kind,
            completion_kind: comp_kind,
            detail,
            doc,
            container_name: parent_container.get(&def_node).cloned(),
            file: mapped.path.clone(),
            full_span,
            selection_span,
            children: Vec::new(),
            params,
            type_index: (sym.type_index != TypeIndex::INVALID).then_some(sym.type_index),
            return_type: ret_ty,
            type_def_target: None,
            trait_owner,
            is_external: mapped.is_external,
            is_mutable,
            is_async,
            is_synthesized_module: is_synth_mod,
            has_self_receiver: has_self,
        };
        index.symbols.insert(key, info);
    }

    // Wire `type_def_target` on every symbol whose type resolves to a nominal type definition.
    let type_defs_snapshot = index.type_defs.clone();
    for info in index.symbols.values_mut() {
        let base_ty = info.return_type.or(info.type_index);
        if let Some(ty) = base_ty
            && let Some(target_key) =
                resolve_nominal_type_def(&resolved.type_pool, &type_defs_snapshot, ty)
        {
            info.type_def_target = Some(target_key);
        }
    }

    // Record prelude symbols (from `std/prelude.ns` or root scope bindings of `std`).
    for scope in &resolved.scopes {
        if !scope.node.is_null()
            && let Some((_, unit, _, _)) = locate(ast.node(scope.node).span)
            && unit.logical_name == "std/prelude.ns"
        {
            for (&name_id, &sym_id) in &scope.bindings {
                let sname = str_interner::get(name_id);
                let skey = SymbolKey::Sym(sym_id);
                if !sname.is_empty() && index.symbols.contains_key(&skey) {
                    index.prelude_symbols.push((sname, skey));
                }
            }
        }
    }
    // Also include `std.builtin` public aliases in `prelude_symbols` (since `use std.prelude.*` re-exports them).
    for scope in &resolved.scopes {
        if !scope.node.is_null()
            && let Some((_, unit, _, _)) = locate(ast.node(scope.node).span)
            && (unit.logical_name == "std/builtin.ns" || unit.logical_name == "std/traits.ns")
        {
            for (&name_id, &sym_id) in &scope.bindings {
                let sym = &resolved.symbols[sym_id.0 as usize];
                if sym.visibility == Visibility::Public {
                    let sname = str_interner::get(name_id);
                    let skey = SymbolKey::Sym(sym_id);
                    if !sname.is_empty()
                        && index.symbols.contains_key(&skey)
                        && !index.prelude_symbols.iter().any(|(n, _)| n == &sname)
                    {
                        index.prelude_symbols.push((sname, skey));
                    }
                }
            }
        }
    }

    // Build hierarchical outline (`file_roots` + `SymbolInfo.children`) and `ImplBlock` symbols,
    // plus `trait_impls` for `textDocument/implementation`.
    build_outline_and_impls(resolved, &locate, &mut index);

    // Collect top-level user package modules for `@` completions.
    for (&name_id, &sym_id) in &resolved.scopes[ScopeId::ROOT.0 as usize].bindings {
        let sym = &resolved.symbols[sym_id.0 as usize];
        if sym.kind == SymbolKind::Module {
            let key = SymbolKey::Sym(sym_id);
            if let Some(info) = index.symbols.get(&key)
                && !info.is_external
            {
                index
                    .package_modules
                    .push((str_interner::get(name_id), key));
            }
        }
    }
    for scope in &resolved.scopes {
        if !scope.node.is_null() && ast.node(scope.node).kind == NodeKind::FileScope {
            for (&name_id, &sym_id) in &scope.bindings {
                let sym = &resolved.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Module {
                    let key = SymbolKey::Sym(sym_id);
                    let mname = str_interner::get(name_id);
                    if let Some(info) = index.symbols.get(&key)
                        && !info.is_external
                        && !index.package_modules.iter().any(|(n, _)| n == &mname)
                    {
                        index.package_modules.push((mname, key));
                    }
                }
            }
        }
    }

    // Build `type_members` table (struct fields, enum variants, module exports, inherent methods, trait methods, derived methods).
    build_type_members(resolved, &effects_by_ty, &assoc_lookup, &mut index);

    // Build per-file `file_scopes` for lexical completions.
    for scope in &resolved.scopes {
        if scope.node.is_null() {
            continue;
        }
        if let Some((_, _, mapped, span)) = locate(ast.node(scope.node).span)
            && !mapped.is_external
        {
            let mut bindings = Vec::new();
            for (&name_id, &sym_id) in &scope.bindings {
                let key = SymbolKey::Sym(sym_id);
                if index.symbols.contains_key(&key) {
                    bindings.push((str_interner::get(name_id), key));
                }
            }
            if !bindings.is_empty() {
                index
                    .file_scopes
                    .entry(mapped.path.clone())
                    .or_default()
                    .push(FileScopeSpan { span, bindings });
            }
        }
    }

    // Build leaf-only `file_occurrences` and `file_projections` across all AST nodes!
    collect_occurrences_and_projections(
        resolved,
        &locate,
        &effects_by_ty,
        &write_nodes,
        &assoc_lookup,
        &mut index,
    );

    index
}

fn record_pattern_decl_parents(
    ast: &Ast,
    pat: NodeIndex,
    outer_decl: NodeIndex,
    decl_kind: NodeKind,
    out: &mut HashMap<NodeIndex, (NodeIndex, NodeKind)>,
) {
    if pat.is_null() {
        return;
    }
    if ast.node(pat).kind == NodeKind::Id {
        out.insert(pat, (outer_decl, decl_kind));
    }
    for &c in ast.fixed_children(pat) {
        record_pattern_decl_parents(ast, c, outer_decl, decl_kind, out);
    }
    for &c in ast.multi_children(pat) {
        record_pattern_decl_parents(ast, c, outer_decl, decl_kind, out);
    }
}

fn unwrap_vis(ast: &Ast, mut node: NodeIndex) -> NodeIndex {
    while !node.is_null() && matches!(ast.node(node).kind, NodeKind::PubDef | NodeKind::PrivateDef)
    {
        node = ast.fixed_children(node)[0];
    }
    node
}

fn selection_node_for_def(ast: &Ast, def_node: NodeIndex) -> (NodeIndex, bool) {
    match ast.node(def_node).kind {
        NodeKind::FunctionDef
        | NodeKind::TraitDefFn
        | NodeKind::TraitDeriveFn
        | NodeKind::StructDef
        | NodeKind::EnumDef
        | NodeKind::TraitDef
        | NodeKind::EffectDef
        | NodeKind::AsyncEffectDef
        | NodeKind::Typealias
        | NodeKind::Newtype
        | NodeKind::StructField
        | NodeKind::EnumVariant => {
            let child = ast.fixed_children(def_node)[0];
            if !child.is_null() && !ast.node(child).span.is_dummy() {
                (child, false)
            } else {
                (def_node, false)
            }
        }
        NodeKind::ModuleDef => {
            let child = ast.fixed_children(def_node)[0];
            if !child.is_null() && !ast.node(child).span.is_dummy() {
                (child, false)
            } else {
                (def_node, true)
            }
        }
        _ => (def_node, false),
    }
}

fn classify_symbol_kinds(
    sym_kind: SymbolKind,
    def_kind: NodeKind,
    pool: &TypePool,
    ty: TypeIndex,
    is_trait_ty: bool,
    is_method: bool,
) -> (u32, u32) {
    match sym_kind {
        SymbolKind::Module => (lsp_symbol_kind::MODULE, lsp_completion_kind::MODULE),
        SymbolKind::Trait => (lsp_symbol_kind::INTERFACE, lsp_completion_kind::INTERFACE),
        SymbolKind::Effect => (lsp_symbol_kind::EVENT, lsp_completion_kind::EVENT),
        SymbolKind::EnumVariant => (
            lsp_symbol_kind::ENUM_MEMBER,
            lsp_completion_kind::ENUM_MEMBER,
        ),
        SymbolKind::Field => (lsp_symbol_kind::FIELD, lsp_completion_kind::FIELD),
        SymbolKind::Constant => (lsp_symbol_kind::CONSTANT, lsp_completion_kind::CONSTANT),
        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Label => {
            (lsp_symbol_kind::VARIABLE, lsp_completion_kind::VARIABLE)
        }
        SymbolKind::Function | SymbolKind::BuiltinFunction(_) => {
            if is_method {
                (lsp_symbol_kind::METHOD, lsp_completion_kind::METHOD)
            } else {
                (lsp_symbol_kind::FUNCTION, lsp_completion_kind::FUNCTION)
            }
        }
        SymbolKind::TypeFactory(_) => (lsp_symbol_kind::CLASS, lsp_completion_kind::CLASS),
        SymbolKind::Type => {
            if is_trait_ty {
                return (lsp_symbol_kind::INTERFACE, lsp_completion_kind::INTERFACE);
            }
            if def_kind == NodeKind::EnumDef {
                return (lsp_symbol_kind::ENUM, lsp_completion_kind::ENUM);
            }
            if def_kind == NodeKind::StructDef {
                return (lsp_symbol_kind::STRUCT, lsp_completion_kind::STRUCT);
            }
            if ty != TypeIndex::INVALID
                && let Some(canon) = pool.canonical_type(ty)
            {
                match pool.get(canon).kind {
                    TypeKind::Struct { .. } => {
                        return (lsp_symbol_kind::STRUCT, lsp_completion_kind::STRUCT);
                    }
                    TypeKind::Enum { .. } => {
                        return (lsp_symbol_kind::ENUM, lsp_completion_kind::ENUM);
                    }
                    TypeKind::Trait { .. } => {
                        return (lsp_symbol_kind::INTERFACE, lsp_completion_kind::INTERFACE);
                    }
                    _ => {}
                }
            }
            (lsp_symbol_kind::CLASS, lsp_completion_kind::CLASS)
        }
    }
}

fn vis_prefix(vis: Visibility) -> &'static str {
    match vis {
        Visibility::Public => "pub ",
        Visibility::Private => "private ",
        Visibility::Package => "",
    }
}

pub fn format_type(
    pool: &TypePool,
    effects_by_ty: &HashMap<TypeIndex, (String, bool)>,
    ty: TypeIndex,
) -> String {
    format_type_inner(pool, effects_by_ty, ty, 0)
}

fn format_type_inner(
    pool: &TypePool,
    effects_by_ty: &HashMap<TypeIndex, (String, bool)>,
    ty: TypeIndex,
    depth: usize,
) -> String {
    if ty == TypeIndex::INVALID || depth > 64 {
        return "Any".to_owned();
    }
    // Preserve explicit `typealias` name at outermost surface if helpful? Actually Nessa canonical names are crisp,
    // except if `ty` itself is a `Typealias` we can check `canonical_type`.
    let Some(canon) = pool.canonical_type(ty) else {
        return "Any".to_owned();
    };
    if let Some((eff_name, _)) = effects_by_ty.get(&canon) {
        return eff_name.clone();
    }
    match &pool.get(canon).kind {
        TypeKind::Intrinsic(intr) => intr.name().to_owned(),
        TypeKind::Struct { name, .. }
        | TypeKind::Enum { name, .. }
        | TypeKind::Newtype { name, .. }
        | TypeKind::Module { name }
        | TypeKind::Trait { name, .. } => str_interner::get(*name),
        TypeKind::Tuple { elements } => {
            let parts: Vec<String> = elements
                .iter()
                .map(|&e| format_type_inner(pool, effects_by_ty, e, depth + 1))
                .collect();
            format!("({})", parts.join(", "))
        }
        TypeKind::Function { params, ret } => {
            let parts: Vec<String> = params
                .iter()
                .map(|&p| format_type_inner(pool, effects_by_ty, p, depth + 1))
                .collect();
            let ret_s = format_type_inner(pool, effects_by_ty, *ret, depth + 1);
            format!("fn({}) -> {ret_s}", parts.join(", "))
        }
        TypeKind::Effect {
            params,
            ret,
            is_async,
        } => {
            let parts: Vec<String> = params
                .iter()
                .map(|&p| format_type_inner(pool, effects_by_ty, p, depth + 1))
                .collect();
            let ret_s = format_type_inner(pool, effects_by_ty, *ret, depth + 1);
            let kw = if *is_async { "async effect" } else { "effect" };
            format!("{kw}({}) -> {ret_s}", parts.join(", "))
        }
        TypeKind::Optional { inner } => {
            format!(
                "?{}",
                format_type_inner(pool, effects_by_ty, *inner, depth + 1)
            )
        }
        TypeKind::Future { inner } => {
            format!(
                "Future({})",
                format_type_inner(pool, effects_by_ty, *inner, depth + 1)
            )
        }
        TypeKind::ErrorQualified { errors, inner } => {
            let errs: Vec<String> = errors
                .iter()
                .map(|&e| format_type_inner(pool, effects_by_ty, e, depth + 1))
                .collect();
            let inner_s = format_type_inner(pool, effects_by_ty, *inner, depth + 1);
            if errs.len() == 1 {
                format!("!{} {inner_s}", errs[0])
            } else {
                format!("![{}] {inner_s}", errs.join(", "))
            }
        }
        TypeKind::EffectQualified { effects, inner } => {
            let effs: Vec<String> = effects
                .iter()
                .map(|&e| format_type_inner(pool, effects_by_ty, e, depth + 1))
                .collect();
            let inner_s = format_type_inner(pool, effects_by_ty, *inner, depth + 1);
            if effs.len() == 1 {
                format!("#{} {inner_s}", effs[0])
            } else {
                format!("#[{}] {inner_s}", effs.join(", "))
            }
        }
        TypeKind::AssociatedType { trait_owner, name } => format!(
            "{}.{}",
            format_type_inner(pool, effects_by_ty, *trait_owner, depth + 1),
            str_interner::get(*name)
        ),
        TypeKind::IterationStepTemplate { item } => format!(
            "IterationStep({})",
            format_type_inner(pool, effects_by_ty, *item, depth + 1)
        ),
        TypeKind::Typealias { name, .. } => str_interner::get(*name),
    }
}

fn node_source_label<'a, L>(ast: &Ast, locate: &L, node: NodeIndex) -> Option<String>
where
    L: Fn(Span) -> Option<(usize, &'a CheckedSourceUnit, &'a UnitMapped, (usize, usize))>,
{
    if node.is_null() {
        return None;
    }
    let (_, unit, _, (lo, hi)) = locate(ast.node(node).span)?;
    unit.source.get(lo..hi).map(|s| s.trim().to_owned())
}

fn format_callable_params<'a, L>(
    resolved: &ResolvedAst,
    param_nodes: &[NodeIndex],
    fn_param_tys: Option<&[TypeIndex]>,
    effects_by_ty: &HashMap<TypeIndex, (String, bool)>,
    locate: &L,
) -> Vec<ParamDoc>
where
    L: Fn(Span) -> Option<(usize, &'a CheckedSourceUnit, &'a UnitMapped, (usize, usize))>,
{
    let ast = &resolved.ast;
    let mut out = Vec::with_capacity(param_nodes.len());
    for (idx, &pnode) in param_nodes.iter().enumerate() {
        if pnode.is_null() {
            continue;
        }
        let fallback_ty = fn_param_tys.and_then(|pts| pts.get(idx).copied());
        match ast.node(pnode).kind {
            NodeKind::ParamSelf => {
                out.push(ParamDoc {
                    name: Some("self".to_owned()),
                    label: "self".to_owned(),
                    is_self: true,
                    is_optional: false,
                    is_varargs: false,
                });
            }
            NodeKind::ParamTyped => {
                let children = ast.fixed_children(pnode);
                let pat = children[0];
                let pname = if !pat.is_null() && ast.node(pat).kind == NodeKind::Id {
                    Some(str_interner::get(ast.node(pat).str_id))
                } else {
                    node_source_label(ast, locate, pat)
                };
                let ty_idx = resolved
                    .node_symbols
                    .get(&pat)
                    .map(|s| resolved.symbols[s.0 as usize].type_index)
                    .filter(|&t| t != TypeIndex::INVALID)
                    .or_else(|| resolved.node_type_values.get(&children[1]).copied())
                    .or(fallback_ty)
                    .unwrap_or(TypeIndex::INVALID);
                let ty_str = if ty_idx != TypeIndex::INVALID {
                    format_type(&resolved.type_pool, effects_by_ty, ty_idx)
                } else {
                    node_source_label(ast, locate, children[1]).unwrap_or_else(|| "Any".to_owned())
                };
                let pat_label = pname.clone().unwrap_or_else(|| "_".to_owned());
                out.push(ParamDoc {
                    name: pname,
                    label: format!("{pat_label}: {ty_str}"),
                    is_self: false,
                    is_optional: false,
                    is_varargs: false,
                });
            }
            NodeKind::ParamOptional => {
                let children = ast.fixed_children(pnode);
                let name_node = children[0];
                let pname = str_interner::get(ast.node(name_node).str_id);
                let ty_idx = resolved
                    .node_symbols
                    .get(&name_node)
                    .map(|s| resolved.symbols[s.0 as usize].type_index)
                    .filter(|&t| t != TypeIndex::INVALID)
                    .or_else(|| resolved.node_type_values.get(&children[1]).copied())
                    .or(fallback_ty)
                    .unwrap_or(TypeIndex::INVALID);
                let ty_str = if ty_idx != TypeIndex::INVALID {
                    format_type(&resolved.type_pool, effects_by_ty, ty_idx)
                } else {
                    node_source_label(ast, locate, children[1]).unwrap_or_else(|| "Any".to_owned())
                };
                let def_str = node_source_label(ast, locate, children[2]);
                let label = match def_str {
                    Some(d) if !d.is_empty() => format!(".{pname}: {ty_str} = {d}"),
                    _ => format!(".{pname}: {ty_str}"),
                };
                out.push(ParamDoc {
                    name: Some(pname),
                    label,
                    is_self: false,
                    is_optional: true,
                    is_varargs: false,
                });
            }
            NodeKind::ParamVarargs => {
                let children = ast.fixed_children(pnode);
                let name_node = children[0];
                let pname = str_interner::get(ast.node(name_node).str_id);
                let ty_idx = resolved
                    .node_symbols
                    .get(&name_node)
                    .map(|s| resolved.symbols[s.0 as usize].type_index)
                    .filter(|&t| t != TypeIndex::INVALID)
                    .or(fallback_ty)
                    .unwrap_or(TypeIndex::INVALID);
                let ty_str = if ty_idx != TypeIndex::INVALID {
                    format_type(&resolved.type_pool, effects_by_ty, ty_idx)
                } else {
                    node_source_label(ast, locate, children[1]).unwrap_or_else(|| "List".to_owned())
                };
                out.push(ParamDoc {
                    name: Some(pname.clone()),
                    label: format!("...{pname}: {ty_str}"),
                    is_self: false,
                    is_optional: false,
                    is_varargs: true,
                });
            }
            NodeKind::ParamLambda => {
                let children = ast.fixed_children(pnode);
                let pat = children[0];
                let pname = if !pat.is_null() && ast.node(pat).kind == NodeKind::Id {
                    Some(str_interner::get(ast.node(pat).str_id))
                } else {
                    node_source_label(ast, locate, pat)
                };
                let ty_idx = resolved
                    .node_symbols
                    .get(&pat)
                    .map(|s| resolved.symbols[s.0 as usize].type_index)
                    .filter(|&t| t != TypeIndex::INVALID)
                    .or(fallback_ty)
                    .unwrap_or(TypeIndex::INVALID);
                let ty_str = if ty_idx != TypeIndex::INVALID {
                    format_type(&resolved.type_pool, effects_by_ty, ty_idx)
                } else {
                    "fn(...)".to_owned()
                };
                let pat_label = pname.clone().unwrap_or_else(|| "_".to_owned());
                out.push(ParamDoc {
                    name: pname,
                    label: format!("lambda {pat_label}: {ty_str}"),
                    is_self: false,
                    is_optional: false,
                    is_varargs: false,
                });
            }
            NodeKind::ParamCatch => {
                let children = ast.fixed_children(pnode);
                let pat = children[0];
                let pname = str_interner::get(ast.node(pat).str_id);
                let ty_str = node_source_label(ast, locate, children[1])
                    .unwrap_or_else(|| "Continuation".to_owned());
                out.push(ParamDoc {
                    name: Some(pname.clone()),
                    label: format!("catch {pname}: {ty_str}"),
                    is_self: false,
                    is_optional: false,
                    is_varargs: false,
                });
            }
            _ => {}
        }
    }
    out
}

fn format_symbol_signature<'a, L>(
    resolved: &ResolvedAst,
    sym: &resolution::Symbol,
    def_node: NodeIndex,
    effects_by_ty: &HashMap<TypeIndex, (String, bool)>,
    locate: &L,
    decl_kind: Option<NodeKind>,
) -> (String, Vec<ParamDoc>, Option<TypeIndex>, bool)
where
    L: Fn(Span) -> Option<(usize, &'a CheckedSourceUnit, &'a UnitMapped, (usize, usize))>,
{
    let ast = &resolved.ast;
    let pool = &resolved.type_pool;
    let name = str_interner::get(sym.name);
    let vis = vis_prefix(sym.visibility);
    let kind = ast.node(def_node).kind;

    match kind {
        NodeKind::FunctionDef | NodeKind::TraitDefFn | NodeKind::TraitDeriveFn => {
            let fn_sig = (sym.type_index != TypeIndex::INVALID)
                .then(|| pool.canonical_type(sym.type_index))
                .flatten()
                .and_then(|cty| match &pool.get(cty).kind {
                    TypeKind::Function { params, ret } => Some((params.clone(), *ret)),
                    _ => None,
                });
            let params = format_callable_params(
                resolved,
                ast.multi_children(def_node),
                fn_sig.as_ref().map(|(p, _)| p.as_slice()),
                effects_by_ty,
                locate,
            );
            let params_str = params
                .iter()
                .map(|p| p.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let ret_ty = fn_sig.as_ref().map(|(_, r)| *r).or_else(|| {
                let ret_node = ast.fixed_children(def_node)[1];
                resolved.node_type_values.get(&ret_node).copied()
            });
            let ret_str = match ret_ty {
                Some(rt) if rt != TypeIndex::INVALID => format_type(pool, effects_by_ty, rt),
                _ => node_source_label(ast, locate, ast.fixed_children(def_node)[1])
                    .unwrap_or_else(|| "Unit".to_owned()),
            };
            let cap_node = if kind == NodeKind::TraitDefFn {
                ast.fixed_children(def_node)[2]
            } else {
                ast.fixed_children(def_node)[3]
            };
            let handles_suffix = node_source_label(ast, locate, cap_node)
                .filter(|s| !s.is_empty())
                .map(|s| format!(" handles {s}"))
                .unwrap_or_default();
            let kw = if kind == NodeKind::TraitDeriveFn {
                "derive fn"
            } else {
                "fn"
            };
            let is_async = ret_ty
                .and_then(|rt| pool.canonical_type(rt))
                .is_some_and(|crt| matches!(pool.get(crt).kind, TypeKind::Future { .. }));
            (
                format!("{vis}{kw} {name}({params_str}) -> {ret_str}{handles_suffix}"),
                params,
                ret_ty,
                is_async,
            )
        }
        NodeKind::EffectDef | NodeKind::AsyncEffectDef => {
            let eff_sig = (sym.type_index != TypeIndex::INVALID)
                .then(|| pool.canonical_type(sym.type_index))
                .flatten()
                .and_then(|cty| match &pool.get(cty).kind {
                    TypeKind::Effect {
                        params,
                        ret,
                        is_async,
                    } => Some((params.clone(), *ret, *is_async)),
                    _ => None,
                });
            let params = format_callable_params(
                resolved,
                ast.multi_children(def_node),
                eff_sig.as_ref().map(|(p, _, _)| p.as_slice()),
                effects_by_ty,
                locate,
            );
            let params_str = params
                .iter()
                .map(|p| p.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let ret_ty = eff_sig.as_ref().map(|(_, r, _)| *r);
            let ret_str = match ret_ty {
                Some(rt) if rt != TypeIndex::INVALID => format_type(pool, effects_by_ty, rt),
                _ => node_source_label(ast, locate, ast.fixed_children(def_node)[1])
                    .unwrap_or_else(|| "Unit".to_owned()),
            };
            let is_async =
                kind == NodeKind::AsyncEffectDef || eff_sig.as_ref().is_some_and(|(_, _, a)| *a);
            let kw = if is_async { "async effect" } else { "effect" };
            (
                format!("{vis}{kw} {name}({params_str}) -> {ret_str}"),
                params,
                ret_ty,
                is_async,
            )
        }
        NodeKind::StructDef => {
            let mut body_lines = Vec::new();
            if sym.type_index != TypeIndex::INVALID
                && let Some(canon) = pool.canonical_type(sym.type_index)
                && !pool.is_reserved_collection_role(canon)
                && let TypeKind::Struct { fields, .. } = &pool.get(canon).kind
            {
                for f in fields {
                    let fname = str_interner::get(f.name);
                    let fty = format_type(pool, effects_by_ty, f.ty);
                    let def_suffix = if f.has_default { " = ..." } else { "" };
                    body_lines.push(format!("    {fname}: {fty}{def_suffix},"));
                }
            }
            let detail = if body_lines.is_empty() {
                format!("{vis}struct {name} {{}}")
            } else {
                format!("{vis}struct {name} {{\n{}\n}}", body_lines.join("\n"))
            };
            (detail, Vec::new(), None, false)
        }
        NodeKind::EnumDef => {
            let mut var_lines = Vec::new();
            if sym.type_index != TypeIndex::INVALID
                && let Some(canon) = pool.canonical_type(sym.type_index)
                && let TypeKind::Enum { variants, .. } = &pool.get(canon).kind
            {
                for v in variants {
                    let vname = str_interner::get(v.name);
                    if v.fields.is_empty() {
                        var_lines.push(format!("    {vname},"));
                    } else {
                        let vfields: Vec<String> = v
                            .fields
                            .iter()
                            .map(|f| {
                                format!(
                                    "{}: {}",
                                    str_interner::get(f.name),
                                    format_type(pool, effects_by_ty, f.ty)
                                )
                            })
                            .collect();
                        var_lines.push(format!("    {vname}({}),", vfields.join(", ")));
                    }
                }
            }
            let detail = if var_lines.is_empty() {
                format!("{vis}enum {name} {{}}")
            } else {
                format!("{vis}enum {name} {{\n{}\n}}", var_lines.join("\n"))
            };
            (detail, Vec::new(), None, false)
        }
        NodeKind::EnumVariant => {
            let owner_str = if sym.type_index != TypeIndex::INVALID {
                format_type(pool, effects_by_ty, sym.type_index)
            } else {
                "Enum".to_owned()
            };
            let params = format_callable_params(
                resolved,
                ast.multi_children(def_node),
                None,
                effects_by_ty,
                locate,
            );
            if params.is_empty() {
                (
                    format!("{owner_str}.{name}"),
                    params,
                    Some(sym.type_index),
                    false,
                )
            } else {
                let pstr = params
                    .iter()
                    .map(|p| p.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    format!("{owner_str}.{name}({pstr}) -> {owner_str}"),
                    params,
                    Some(sym.type_index),
                    false,
                )
            }
        }
        NodeKind::StructField => {
            let fty = if sym.type_index != TypeIndex::INVALID {
                format_type(pool, effects_by_ty, sym.type_index)
            } else {
                node_source_label(ast, locate, ast.fixed_children(def_node)[1])
                    .unwrap_or_else(|| "Any".to_owned())
            };
            let def_val = node_source_label(ast, locate, ast.fixed_children(def_node)[2]);
            let detail = match def_val {
                Some(d) if !d.is_empty() => format!("{vis}{name}: {fty} = {d}"),
                _ => format!("{vis}{name}: {fty}"),
            };
            (detail, Vec::new(), None, false)
        }
        NodeKind::TraitDef => {
            let mut parents_str = String::new();
            if sym.type_index != TypeIndex::INVALID
                && let Some(canon) = pool.canonical_type(sym.type_index)
                && let TypeKind::Trait { parents, .. } = &pool.get(canon).kind
                && !parents.is_empty()
            {
                let pnames: Vec<String> = parents
                    .iter()
                    .map(|&p| format_type(pool, effects_by_ty, p))
                    .collect();
                parents_str = format!("({})", pnames.join(", "));
            }
            (
                format!("{vis}trait {name}{parents_str}"),
                Vec::new(),
                None,
                false,
            )
        }
        NodeKind::Typealias => {
            let target_str = if sym.type_index != TypeIndex::INVALID {
                format_type(pool, effects_by_ty, sym.type_index)
            } else {
                node_source_label(ast, locate, ast.fixed_children(def_node)[1])
                    .unwrap_or_else(|| "Type".to_owned())
            };
            (
                format!("{vis}typealias {name} = {target_str}"),
                Vec::new(),
                None,
                false,
            )
        }
        NodeKind::Newtype => {
            let inner_str = node_source_label(ast, locate, ast.fixed_children(def_node)[1])
                .unwrap_or_else(|| "Any".to_owned());
            let param = ParamDoc {
                name: Some("value".to_owned()),
                label: format!("value: {inner_str}"),
                is_self: false,
                is_optional: false,
                is_varargs: false,
            };
            (
                format!("{vis}newtype {name} = {inner_str}"),
                vec![param],
                Some(sym.type_index),
                false,
            )
        }
        NodeKind::ModuleDef => (format!("{vis}mod {name}"), Vec::new(), None, false),
        NodeKind::ParamSelf => {
            let ty_str = if sym.type_index != TypeIndex::INVALID {
                format_type(pool, effects_by_ty, sym.type_index)
            } else {
                "Self".to_owned()
            };
            (format!("self: {ty_str}"), Vec::new(), None, false)
        }
        _ => {
            let ty_idx = if sym.type_index != TypeIndex::INVALID {
                Some(sym.type_index)
            } else {
                resolved.node_types.get(&def_node).copied()
            };
            let ty_str = ty_idx
                .filter(|&t| t != TypeIndex::INVALID)
                .map(|t| format_type(pool, effects_by_ty, t))
                .unwrap_or_else(|| "Any".to_owned());
            let prefix = match (sym.kind, decl_kind) {
                (SymbolKind::Parameter, _) => "(parameter) ",
                (_, Some(NodeKind::ConstDecl)) | (SymbolKind::Constant, _) => "const ",
                (_, Some(NodeKind::VarDecl)) => "var ",
                (_, Some(NodeKind::GlobalDecl)) => "global ",
                (SymbolKind::Variable, _) => "let ",
                _ => "",
            };
            (
                format!("{vis}{prefix}{name}: {ty_str}"),
                Vec::new(),
                None,
                false,
            )
        }
    }
}

fn resolve_nominal_type_def(
    pool: &TypePool,
    type_defs: &HashMap<TypeIndex, SymbolKey>,
    ty: TypeIndex,
) -> Option<SymbolKey> {
    if ty == TypeIndex::INVALID {
        return None;
    }
    let canon = pool.canonical_type(ty)?;
    if let Some(&key) = type_defs.get(&canon) {
        return Some(key);
    }
    match &pool.get(canon).kind {
        TypeKind::Optional { inner }
        | TypeKind::Future { inner }
        | TypeKind::ErrorQualified { inner, .. }
        | TypeKind::EffectQualified { inner, .. } => {
            resolve_nominal_type_def(pool, type_defs, *inner)
        }
        TypeKind::Tuple { elements } => elements
            .iter()
            .find_map(|&el| resolve_nominal_type_def(pool, type_defs, el)),
        _ => None,
    }
}

fn build_outline_and_impls<'a, L>(resolved: &ResolvedAst, locate: &L, index: &mut SemanticIndex)
where
    L: Fn(Span) -> Option<(usize, &'a CheckedSourceUnit, &'a UnitMapped, (usize, usize))>,
{
    let ast = &resolved.ast;
    // Record `DeriveDef` trait implementations (`derive Display, Eq for Point`).
    for raw in 1..ast.nodes.len() {
        let node = NodeIndex(raw as u32);
        if ast.node(node).kind == NodeKind::DeriveDef {
            let target_node = ast.fixed_children(node)[0];
            let Some(impl_ty) = node_resolved_type_value(resolved, target_node) else {
                continue;
            };
            let Some((_, _, mapped, span)) = locate(ast.node(node).span) else {
                continue;
            };
            for &tr_expr in ast.multi_children(node) {
                if let Some(tr_ty) = node_resolved_type_value(resolved, tr_expr) {
                    index
                        .trait_impls
                        .entry(tr_ty)
                        .or_default()
                        .push(TraitImplEntry {
                            trait_type: tr_ty,
                            implementor: impl_ty,
                            file: mapped.path.clone(),
                            span,
                            methods: HashMap::new(),
                        });
                }
            }
        }
    }

    // Walk top-level items across `ast.root` (skipping `std` and detached external package roots).
    let root_items = ast.multi_children(ast.root);
    for &item in root_items {
        collect_outline_item(resolved, locate, item, None, index);
    }
}

fn node_resolved_type_value(resolved: &ResolvedAst, node: NodeIndex) -> Option<TypeIndex> {
    if node.is_null() {
        return None;
    }
    resolved
        .node_type_values
        .get(&node)
        .copied()
        .or_else(|| {
            resolved
                .node_symbols
                .get(&node)
                .map(|s| resolved.symbols[s.0 as usize].type_index)
        })
        .filter(|&t| t != TypeIndex::INVALID)
        .and_then(|t| resolved.type_pool.canonical_type(t))
}

fn collect_outline_item<'a, L>(
    resolved: &ResolvedAst,
    locate: &L,
    raw_item: NodeIndex,
    parent_key: Option<SymbolKey>,
    index: &mut SemanticIndex,
) where
    L: Fn(Span) -> Option<(usize, &'a CheckedSourceUnit, &'a UnitMapped, (usize, usize))>,
{
    let ast = &resolved.ast;
    if raw_item.is_null() {
        return;
    }
    let item = unwrap_vis(ast, raw_item);
    if item.is_null() {
        return;
    }
    let kind = ast.node(item).kind;

    match kind {
        NodeKind::ModuleDef => {
            let name_node = ast.fixed_children(item)[0];
            let sym_key = resolved
                .node_symbols
                .get(&name_node)
                .copied()
                .map(SymbolKey::Sym);
            let is_synth = name_node.is_null() || ast.node(name_node).span.is_dummy();
            if is_synth {
                // Filesystem module (`src/utils.ns` or external `std` / `regex`): recurse directly into its own file roots!
                for &child in ast.multi_children(item) {
                    collect_outline_item(resolved, locate, child, None, index);
                }
            } else if let Some(mkey) = sym_key
                && index.symbols.contains_key(&mkey)
            {
                attach_outline_child(index, parent_key, mkey);
                for &child in ast.multi_children(item) {
                    collect_outline_item(resolved, locate, child, Some(mkey), index);
                }
            }
        }
        NodeKind::StructDef | NodeKind::EnumDef | NodeKind::TraitDef => {
            let name_node = ast.fixed_children(item)[0];
            if let Some(&sym_id) = resolved.node_symbols.get(&name_node) {
                let skey = SymbolKey::Sym(sym_id);
                if index.symbols.contains_key(&skey) {
                    attach_outline_child(index, parent_key, skey);
                    for &member in ast.multi_children(item) {
                        collect_outline_item(resolved, locate, member, Some(skey), index);
                    }
                }
            }
        }
        NodeKind::ImplDef
        | NodeKind::ExtendDef
        | NodeKind::ImplTraitDef
        | NodeKind::ExtendTraitDef => {
            let Some((_, unit, mapped, full_span)) = locate(ast.node(raw_item).span) else {
                return;
            };
            let is_trait_impl = matches!(kind, NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef);
            let kw = match kind {
                NodeKind::ImplDef | NodeKind::ImplTraitDef => "impl",
                _ => "extend",
            };
            let (label, sel_node) = if is_trait_impl {
                let tr_node = ast.fixed_children(item)[0];
                let ty_node = ast.fixed_children(item)[1];
                let tr_s =
                    node_source_label(ast, locate, tr_node).unwrap_or_else(|| "Trait".into());
                let ty_s = node_source_label(ast, locate, ty_node).unwrap_or_else(|| "Type".into());
                (format!("{kw} {tr_s} for {ty_s}"), ty_node)
            } else {
                let ty_node = ast.fixed_children(item)[0];
                let ty_s = node_source_label(ast, locate, ty_node).unwrap_or_else(|| "Type".into());
                (format!("{kw} {ty_s}"), ty_node)
            };
            let sel_span = locate(ast.node(sel_node).span)
                .filter(|(_, _, m, _)| m.path == mapped.path)
                .map(|(_, _, _, r)| r)
                .unwrap_or(full_span);

            let bkey = SymbolKey::ImplBlock(item);
            index.symbols.insert(
                bkey,
                SymbolInfo {
                    key: bkey,
                    name: label.clone(),
                    kind: lsp_symbol_kind::NAMESPACE,
                    completion_kind: lsp_completion_kind::MODULE,
                    detail: label,
                    doc: extract_leading_doc(&unit.source, full_span.0),
                    container_name: None,
                    file: mapped.path.clone(),
                    full_span,
                    selection_span: sel_span,
                    children: Vec::new(),
                    params: Vec::new(),
                    type_index: None,
                    return_type: None,
                    type_def_target: None,
                    trait_owner: None,
                    is_external: mapped.is_external,
                    is_mutable: false,
                    is_async: false,
                    is_synthesized_module: false,
                    has_self_receiver: false,
                },
            );
            attach_outline_child(index, parent_key, bkey);

            let mut method_map = HashMap::new();
            for &member in ast.multi_children(item) {
                let inner_m = unwrap_vis(ast, member);
                if matches!(ast.node(inner_m).kind, NodeKind::FunctionDef) {
                    let mname_node = ast.fixed_children(inner_m)[0];
                    if let Some(&msym) = resolved.node_symbols.get(&mname_node) {
                        method_map.insert(
                            str_interner::get(ast.node(mname_node).str_id),
                            SymbolKey::Sym(msym),
                        );
                    }
                }
                collect_outline_item(resolved, locate, member, Some(bkey), index);
            }

            if is_trait_impl {
                let tr_node = ast.fixed_children(item)[0];
                let ty_node = ast.fixed_children(item)[1];
                if let (Some(tr_ty), Some(impl_ty)) = (
                    node_resolved_type_value(resolved, tr_node),
                    node_resolved_type_value(resolved, ty_node),
                ) {
                    index
                        .trait_impls
                        .entry(tr_ty)
                        .or_default()
                        .push(TraitImplEntry {
                            trait_type: tr_ty,
                            implementor: impl_ty,
                            file: mapped.path.clone(),
                            span: sel_span,
                            methods: method_map,
                        });
                }
            }
        }
        NodeKind::FunctionDef
        | NodeKind::TraitDefFn
        | NodeKind::TraitDeriveFn
        | NodeKind::EffectDef
        | NodeKind::AsyncEffectDef
        | NodeKind::StructField
        | NodeKind::EnumVariant
        | NodeKind::Typealias
        | NodeKind::Newtype => {
            let name_node = ast.fixed_children(item)[0];
            if let Some(&sym_id) = resolved.node_symbols.get(&name_node) {
                let skey = SymbolKey::Sym(sym_id);
                if index.symbols.contains_key(&skey) {
                    attach_outline_child(index, parent_key, skey);
                }
            }
        }
        NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl => {
            let pat = ast.fixed_children(item)[0];
            if !pat.is_null()
                && ast.node(pat).kind == NodeKind::Id
                && let Some(&sym_id) = resolved.node_symbols.get(&pat)
            {
                let skey = SymbolKey::Sym(sym_id);
                if index.symbols.contains_key(&skey) {
                    attach_outline_child(index, parent_key, skey);
                }
            }
        }
        NodeKind::GlobalDecl => {
            let inner = ast.fixed_children(item)[0];
            collect_outline_item(resolved, locate, inner, parent_key, index);
        }
        NodeKind::AssocDecl => {
            let inner = ast.fixed_children(item)[0];
            if !inner.is_null() && ast.node(inner).kind == NodeKind::AssocBinding {
                let name_node = ast.fixed_children(inner)[0];
                if let Some(&sym_id) = resolved.node_symbols.get(&name_node) {
                    let skey = SymbolKey::Sym(sym_id);
                    if index.symbols.contains_key(&skey) {
                        attach_outline_child(index, parent_key, skey);
                    }
                }
            } else {
                collect_outline_item(resolved, locate, inner, parent_key, index);
            }
        }
        _ => {}
    }
}

fn attach_outline_child(index: &mut SemanticIndex, parent: Option<SymbolKey>, child: SymbolKey) {
    let Some(child_info) = index.symbols.get(&child) else {
        return;
    };
    if child_info.is_external {
        return;
    }
    let file = child_info.file.clone();
    if let Some(pkey) = parent
        && let Some(pinfo) = index.symbols.get_mut(&pkey)
        && !pinfo.children.contains(&child)
    {
        pinfo.children.push(child);
        return;
    }
    let roots = index.file_roots.entry(file).or_default();
    if !roots.contains(&child) {
        roots.push(child);
    }
}

fn push_member_unique(list: &mut Vec<MemberEntry>, entry: MemberEntry) {
    if !list
        .iter()
        .any(|m| m.name == entry.name && m.is_static == entry.is_static)
    {
        list.push(entry);
    }
}

fn build_type_members(
    resolved: &ResolvedAst,
    effects_by_ty: &HashMap<TypeIndex, (String, bool)>,
    assoc_lookup: &AssocSymbolLookup,
    index: &mut SemanticIndex,
) {
    let pool = &resolved.type_pool;

    // 1. Struct fields and Enum variants from TypePool + symbol table.
    for raw in 0..pool.len() {
        let ty = TypeIndex::from_raw(raw as u32);
        let Some(canon) = pool.canonical_type(ty) else {
            continue;
        };
        if canon != ty {
            continue;
        }
        match &pool.get(canon).kind {
            TypeKind::Struct { fields, .. } if !pool.is_reserved_collection_role(canon) => {
                for f in fields {
                    let fname = str_interner::get(f.name);
                    let fkey =
                        assoc_lookup.find_assoc_symbol(resolved, canon, f.name, SymbolKind::Field);
                    let sym_info = fkey.and_then(|k| index.symbols.get(&k));
                    let detail = sym_info.map(|s| s.detail.clone()).unwrap_or_else(|| {
                        format!("{fname}: {}", format_type(pool, effects_by_ty, f.ty))
                    });
                    let doc = sym_info.and_then(|s| s.doc.clone());
                    push_member_unique(
                        index.type_members.entry(canon).or_default(),
                        MemberEntry {
                            name: fname,
                            key: fkey,
                            kind: lsp_symbol_kind::FIELD,
                            completion_kind: lsp_completion_kind::FIELD,
                            detail,
                            doc,
                            is_static: false,
                            result_type: Some(f.ty),
                        },
                    );
                }
            }
            TypeKind::Enum { name, variants } => {
                let enum_name = str_interner::get(*name);
                for v in variants {
                    let vname = str_interner::get(v.name);
                    let vkey = assoc_lookup.find_assoc_symbol(
                        resolved,
                        canon,
                        v.name,
                        SymbolKind::EnumVariant,
                    );
                    let sym_info = vkey.and_then(|k| index.symbols.get(&k));
                    let detail = sym_info.map(|s| s.detail.clone()).unwrap_or_else(|| {
                        if v.fields.is_empty() {
                            format!("{enum_name}.{vname}")
                        } else {
                            let fs: Vec<String> = v
                                .fields
                                .iter()
                                .map(|f| {
                                    format!(
                                        "{}: {}",
                                        str_interner::get(f.name),
                                        format_type(pool, effects_by_ty, f.ty)
                                    )
                                })
                                .collect();
                            format!("{enum_name}.{vname}({}) -> {enum_name}", fs.join(", "))
                        }
                    });
                    let doc = sym_info.and_then(|s| s.doc.clone());
                    push_member_unique(
                        index.type_members.entry(canon).or_default(),
                        MemberEntry {
                            name: vname,
                            key: vkey,
                            kind: lsp_symbol_kind::ENUM_MEMBER,
                            completion_kind: lsp_completion_kind::ENUM_MEMBER,
                            detail,
                            doc,
                            is_static: true,
                            result_type: Some(canon),
                        },
                    );
                }
            }
            _ => {}
        }
    }

    // 2. Associated scopes (`impl T`, `extend T`, `mod m`, `trait Tr`, `struct S`, `enum E`).
    for scope in &resolved.scopes {
        let Some(raw_assoc) = scope.assoc_type else {
            continue;
        };
        let Some(canon) = pool.canonical_type(raw_assoc) else {
            continue;
        };
        let is_module = matches!(pool.get(canon).kind, TypeKind::Module { .. });
        for (&name_id, &sym_id) in &scope.bindings {
            let sym = &resolved.symbols[sym_id.0 as usize];
            if matches!(sym.kind, SymbolKind::Field | SymbolKind::EnumVariant) {
                continue;
            }
            let skey = SymbolKey::Sym(sym_id);
            let mname = str_interner::get(name_id);
            if mname.is_empty() || mname == "__init__" {
                continue;
            }
            if let Some(sinfo) = index.symbols.get(&skey).cloned() {
                let is_static = is_module || !sinfo.has_self_receiver;
                push_member_unique(
                    index.type_members.entry(canon).or_default(),
                    MemberEntry {
                        name: mname,
                        key: Some(skey),
                        kind: sinfo.kind,
                        completion_kind: sinfo.completion_kind,
                        detail: sinfo.detail,
                        doc: sinfo.doc,
                        is_static,
                        result_type: sinfo.return_type.or(sinfo.type_index),
                    },
                );
            }
        }
    }

    // 3. Inherited trait methods on `Trait` types & implemented trait methods on concrete types!
    for record in pool.trait_impls_snapshot() {
        let (Some(impl_ty), Some(trait_ty)) = (
            pool.canonical_type(record.implementor),
            pool.canonical_type(record.trait_type),
        ) else {
            continue;
        };
        // Add trait methods (from `trait_ty` and its supertraits) to `impl_ty`.
        let mut pending = vec![trait_ty];
        let mut visited = HashSet::new();
        while let Some(cur_tr) = pending.pop() {
            if !visited.insert(cur_tr) {
                continue;
            }
            if let Some(tr_members) = index.type_members.get(&cur_tr).cloned() {
                for m in tr_members {
                    push_member_unique(index.type_members.entry(impl_ty).or_default(), m);
                }
            }
            if let TypeKind::Trait { parents, .. } = &pool.get(cur_tr).kind {
                pending.extend(parents.iter().copied());
            }
        }
    }

    // 4. Compiler-derived traits (`derive Display`, `derive Hash`, `derive Json`, `derive Eq, Ord`).
    for d in &resolved.display_derivations {
        if let Some(canon) = pool.canonical_type(d.implementor) {
            push_member_unique(
                index.type_members.entry(canon).or_default(),
                MemberEntry {
                    name: "to_string".to_owned(),
                    key: None,
                    kind: lsp_symbol_kind::METHOD,
                    completion_kind: lsp_completion_kind::METHOD,
                    detail: "pub fn to_string(self) -> String".to_owned(),
                    doc: Some("Derived `Display.to_string` implementation.".to_owned()),
                    is_static: false,
                    result_type: Some(Intrinsic::Str.type_index()),
                },
            );
        }
    }
    for d in &resolved.hash_derivations {
        if let Some(canon) = pool.canonical_type(d.implementor) {
            push_member_unique(
                index.type_members.entry(canon).or_default(),
                MemberEntry {
                    name: "hash".to_owned(),
                    key: None,
                    kind: lsp_symbol_kind::METHOD,
                    completion_kind: lsp_completion_kind::METHOD,
                    detail: "pub fn hash(self) -> u64".to_owned(),
                    doc: Some("Derived `Hash.hash` implementation.".to_owned()),
                    is_static: false,
                    result_type: Some(Intrinsic::U64.type_index()),
                },
            );
        }
    }
    for d in &resolved.json_derivations {
        if let Some(canon) = pool.canonical_type(d.implementor) {
            let tname = format_type(pool, effects_by_ty, canon);
            push_member_unique(
                index.type_members.entry(canon).or_default(),
                MemberEntry {
                    name: "to_json".to_owned(),
                    key: None,
                    kind: lsp_symbol_kind::METHOD,
                    completion_kind: lsp_completion_kind::METHOD,
                    detail: "pub fn to_json(self) -> String".to_owned(),
                    doc: Some(format!("Derived `Json.to_json` serializer for `{tname}`.")),
                    is_static: false,
                    result_type: Some(Intrinsic::Str.type_index()),
                },
            );
        }
    }
}

struct AssocSymbolLookup {
    scopes_by_type: HashMap<TypeIndex, Vec<usize>>,
    fields_by_type: HashMap<(TypeIndex, StrId), SymbolKey>,
    variants_by_type: HashMap<(TypeIndex, StrId), SymbolKey>,
    variants_by_tag: HashMap<(TypeIndex, u32), SymbolKey>,
}

impl AssocSymbolLookup {
    fn new(resolved: &ResolvedAst) -> Self {
        let pool = &resolved.type_pool;
        let mut scopes_by_type: HashMap<TypeIndex, Vec<usize>> = HashMap::new();
        for (idx, scope) in resolved.scopes.iter().enumerate() {
            if let Some(canon) = scope.assoc_type.and_then(|t| pool.canonical_type(t)) {
                scopes_by_type.entry(canon).or_default().push(idx);
            }
        }

        let mut fields_by_type = HashMap::new();
        let mut variants_by_type = HashMap::new();
        for sym in &resolved.symbols {
            if matches!(sym.kind, SymbolKind::Field | SymbolKind::EnumVariant)
                && sym.type_index != TypeIndex::INVALID
                && let Some(canon) = pool.canonical_type(sym.type_index)
            {
                let key = SymbolKey::Sym(sym.id);
                if sym.kind == SymbolKind::Field {
                    fields_by_type.entry((canon, sym.name)).or_insert(key);
                } else {
                    variants_by_type.entry((canon, sym.name)).or_insert(key);
                }
            }
        }

        let mut variants_by_tag = HashMap::new();
        for (&sym_id, &vtag) in &resolved.enum_variant_indices {
            if let Some(sym) = resolved.symbols.get(sym_id.0 as usize)
                && sym.type_index != TypeIndex::INVALID
                && let Some(canon) = pool.canonical_type(sym.type_index)
            {
                variants_by_tag
                    .entry((canon, vtag))
                    .or_insert(SymbolKey::Sym(sym_id));
            }
        }

        Self {
            scopes_by_type,
            fields_by_type,
            variants_by_type,
            variants_by_tag,
        }
    }

    fn find_assoc_symbol(
        &self,
        resolved: &ResolvedAst,
        canon_ty: TypeIndex,
        name: StrId,
        expected_kind: SymbolKind,
    ) -> Option<SymbolKey> {
        if let Some(scope_indices) = self.scopes_by_type.get(&canon_ty) {
            for &sc_idx in scope_indices {
                if let Some(&sym_id) = resolved.scopes[sc_idx].bindings.get(&name)
                    && resolved.symbols[sym_id.0 as usize].kind == expected_kind
                {
                    return Some(SymbolKey::Sym(sym_id));
                }
            }
        }
        match expected_kind {
            SymbolKind::Field => self.fields_by_type.get(&(canon_ty, name)).copied(),
            SymbolKind::EnumVariant => self.variants_by_type.get(&(canon_ty, name)).copied(),
            _ => resolved
                .symbols
                .iter()
                .find(|s| {
                    s.name == name
                        && s.kind == expected_kind
                        && resolved.type_pool.canonical_type(s.type_index) == Some(canon_ty)
                })
                .map(|s| SymbolKey::Sym(s.id)),
        }
    }

    fn find_enum_variant_by_tag(
        &self,
        resolved: &ResolvedAst,
        enum_ty: TypeIndex,
        tag: u32,
    ) -> Option<SymbolKey> {
        let canon = resolved.type_pool.canonical_type(enum_ty)?;
        self.variants_by_tag.get(&(canon, tag)).copied()
    }
}

fn collect_occurrences_and_projections<'a, L>(
    resolved: &ResolvedAst,
    locate: &L,
    effects_by_ty: &HashMap<TypeIndex, (String, bool)>,
    write_nodes: &HashSet<NodeIndex>,
    assoc_lookup: &AssocSymbolLookup,
    index: &mut SemanticIndex,
) where
    L: Fn(Span) -> Option<(usize, &'a CheckedSourceUnit, &'a UnitMapped, (usize, usize))>,
{
    let ast = &resolved.ast;
    let pool = &resolved.type_pool;
    let type_defs = index.type_defs.clone();

    let mut push_occ = |node: NodeIndex, target: SymbolKey, is_write_override: bool| {
        if node.is_null() || !index.symbols.contains_key(&target) {
            return;
        }
        let Some((_, _, mapped, span)) = locate(ast.node(node).span) else {
            return;
        };
        if mapped.is_external || span.1 <= span.0 {
            return;
        }
        let is_decl = index.symbols.get(&target).is_some_and(|s| {
            !s.is_synthesized_module && s.file == mapped.path && s.selection_span == span
        });

        let expr_ty = resolved.node_types.get(&node).copied();
        let site_type_detail = expr_ty
            .filter(|&t| t != TypeIndex::INVALID && pool.as_intrinsic(t) != Some(Intrinsic::Type))
            .map(|t| format_type(pool, effects_by_ty, t));
        let site_type_def = expr_ty.and_then(|t| resolve_nominal_type_def(pool, &type_defs, t));

        index
            .file_occurrences
            .entry(mapped.path.clone())
            .or_default()
            .push(Occurrence {
                span,
                target,
                is_decl,
                is_write: is_write_override || write_nodes.contains(&node),
                site_type_detail,
                site_type_def,
            });
    };

    for raw in 1..ast.nodes.len() {
        let node = NodeIndex(raw as u32);
        let kind = ast.node(node).kind;

        match kind {
            // Leaf identifiers directly resolved in `node_symbols`.
            NodeKind::Id | NodeKind::SelfLower | NodeKind::SelfUpper | NodeKind::ParamSelf => {
                if let Some(&sym_id) = resolved.node_symbols.get(&node) {
                    push_occ(node, SymbolKey::Sym(sym_id), false);
                }
            }
            // Dot projections: `receiver.member`
            NodeKind::Projection => {
                let children = ast.fixed_children(node);
                let recv = children[0];
                let member = children[1];

                // Record `ProjectionSite` for dot-completion (including `__nessa_lsp_probe__`!).
                if let Some((_, unit, mapped, recv_span)) = locate(ast.node(recv).span)
                    && !mapped.is_external
                {
                    let recv_sym = resolved
                        .node_symbols
                        .get(&recv)
                        .copied()
                        .map(SymbolKey::Sym);
                    let is_type_or_mod = recv_sym
                        .and_then(|k| match k {
                            SymbolKey::Sym(sid) => Some(resolved.symbols[sid.0 as usize].kind),
                            _ => None,
                        })
                        .is_some_and(|sk| {
                            matches!(
                                sk,
                                SymbolKind::Type | SymbolKind::Trait | SymbolKind::Module
                            )
                        })
                        || resolved
                            .node_types
                            .get(&recv)
                            .is_some_and(|&t| pool.as_intrinsic(t) == Some(Intrinsic::Type));

                    let recv_ty = if is_type_or_mod {
                        resolved
                            .node_type_values
                            .get(&recv)
                            .copied()
                            .or_else(|| {
                                resolved
                                    .node_symbols
                                    .get(&recv)
                                    .map(|s| resolved.symbols[s.0 as usize].type_index)
                            })
                            .filter(|&t| t != TypeIndex::INVALID)
                    } else {
                        resolved
                            .node_types
                            .get(&recv)
                            .copied()
                            .filter(|&t| t != TypeIndex::INVALID)
                    };

                    if let Some(rel_dot) = unit.source[recv_span.1..].find('.') {
                        index
                            .file_projections
                            .entry(mapped.path.clone())
                            .or_default()
                            .push(ProjectionSite {
                                receiver_span: recv_span,
                                dot_offset: recv_span.1 + rel_dot,
                                receiver_type: recv_ty,
                                receiver_sym: recv_sym,
                                is_type_or_module_receiver: is_type_or_mod,
                            });
                    }
                }

                if member.is_null() || ast.node(member).kind != NodeKind::Id {
                    continue;
                }
                if resolved.node_symbols.contains_key(&member) {
                    continue;
                }
                if let Some(&method_sym) = resolved.instance_methods.get(&node) {
                    push_occ(member, SymbolKey::Sym(method_sym), false);
                } else if let Some(ev) = resolved.enum_variants.get(&node) {
                    if let Some(vkey) = assoc_lookup.find_enum_variant_by_tag(
                        resolved,
                        ev.type_index,
                        ev.variant_tag,
                    ) {
                        push_occ(member, vkey, false);
                    }
                } else if let Some(&field_idx) = resolved.node_field_indices.get(&node) {
                    if let Some(&lhs_ty) = resolved.node_types.get(&recv)
                        && let Some(canon) = pool.canonical_type(lhs_ty)
                        && let TypeKind::Struct { fields, .. } = &pool.get(canon).kind
                        && let Some(finfo) = fields.get(field_idx as usize)
                        && let Some(fkey) = assoc_lookup.find_assoc_symbol(
                            resolved,
                            canon,
                            finfo.name,
                            SymbolKind::Field,
                        )
                    {
                        push_occ(member, fkey, false);
                    }
                } else if let Some(&proj_sym) = resolved.node_symbols.get(&node) {
                    push_occ(member, SymbolKey::Sym(proj_sym), false);
                } else if let Some(&lhs_ty) = resolved.node_types.get(&recv)
                    && let Some(canon) = pool.canonical_type(lhs_ty)
                    && let Some(members) = index.type_members.get(&canon)
                {
                    let mname = str_interner::get(ast.node(member).str_id);
                    if let Some(found) = members.iter().find(|m| !m.is_static && m.name == mname)
                        && let Some(fkey) = found.key
                    {
                        push_occ(member, fkey, false);
                    }
                }
            }
            // Enum variant pattern: `Color.red`
            NodeKind::PropertyPattern => {
                let children = ast.fixed_children(node);
                let member = children[1];
                if !member.is_null()
                    && ast.node(member).kind == NodeKind::Id
                    && !resolved.node_symbols.contains_key(&member)
                    && let Some(ev) = resolved.enum_variants.get(&node)
                    && let Some(vkey) = assoc_lookup.find_enum_variant_by_tag(
                        resolved,
                        ev.type_index,
                        ev.variant_tag,
                    )
                {
                    push_occ(member, vkey, false);
                }
            }
            // Struct construction: `Point { x: 10, y: 20 }`
            NodeKind::ExtendedCall => {
                let callee = ast.fixed_children(node)[0];
                let struct_ty = resolved
                    .struct_constructions
                    .get(&node)
                    .map(|p| p.type_index)
                    .or_else(|| node_resolved_type_value(resolved, callee));
                if let Some(sty) = struct_ty
                    && let Some(canon) = pool.canonical_type(sty)
                {
                    for &arg in ast.multi_children(node) {
                        if !arg.is_null() && ast.node(arg).kind == NodeKind::Property {
                            let field_id_node = ast.fixed_children(arg)[0];
                            if !field_id_node.is_null()
                                && let Some(fkey) = assoc_lookup.find_assoc_symbol(
                                    resolved,
                                    canon,
                                    ast.node(field_id_node).str_id,
                                    SymbolKind::Field,
                                )
                            {
                                push_occ(field_id_node, fkey, false);
                            }
                        }
                    }
                }
            }
            // Struct pattern: `Point { x: px, y }`
            NodeKind::PatternExtendedCall => {
                let callee = ast.fixed_children(node)[0];
                let struct_ty = resolved
                    .struct_patterns
                    .get(&node)
                    .map(|p| p.type_index)
                    .or_else(|| node_resolved_type_value(resolved, callee));
                if let Some(sty) = struct_ty
                    && let Some(canon) = pool.canonical_type(sty)
                {
                    for &entry in ast.multi_children(node) {
                        if !entry.is_null()
                            && matches!(
                                ast.node(entry).kind,
                                NodeKind::Property | NodeKind::PropertyPattern
                            )
                        {
                            let field_id_node = ast.fixed_children(entry)[0];
                            if !field_id_node.is_null()
                                && let Some(fkey) = assoc_lookup.find_assoc_symbol(
                                    resolved,
                                    canon,
                                    ast.node(field_id_node).str_id,
                                    SymbolKind::Field,
                                )
                            {
                                push_occ(field_id_node, fkey, false);
                            }
                        }
                    }
                }
            }
            // Call expressions: enum variant constructions & named arguments (`foo(bar = 1)`).
            NodeKind::Call => {
                let callee = ast.fixed_children(node)[0];
                if !callee.is_null()
                    && ast.node(callee).kind == NodeKind::Projection
                    && let Some(ec) = resolved.enum_constructions.get(&node)
                {
                    let member = ast.fixed_children(callee)[1];
                    if !member.is_null()
                        && let Some(vkey) = assoc_lookup.find_enum_variant_by_tag(
                            resolved,
                            ec.type_index,
                            ec.variant_tag,
                        )
                    {
                        push_occ(member, vkey, false);
                    }
                }
                if let Some(plan) = resolved.call_arguments.get(&node) {
                    let args = ast.multi_children(node);
                    for binding in &plan.parameters {
                        if let resolution::CallArgumentValue::Explicit { source_index } =
                            binding.value
                            && let Some(&arg_node) = args.get(source_index)
                            && !arg_node.is_null()
                            && ast.node(arg_node).kind == NodeKind::NamedArg
                            && let Some(&param_sym) = binding.symbols.first()
                        {
                            let param_id_node = ast.fixed_children(arg_node)[0];
                            push_occ(param_id_node, SymbolKey::Sym(param_sym), false);
                        }
                    }
                }
            }
            // Target-typed `.variant` symbol literal
            NodeKind::Symbol => {
                let id_node = ast.fixed_children(node)[0];
                if !id_node.is_null()
                    && let Some(&ety) = resolved.node_types.get(&node)
                    && let Some(canon) = pool.canonical_type(ety)
                    && matches!(pool.get(canon).kind, TypeKind::Enum { .. })
                    && let Some(vkey) = assoc_lookup.find_assoc_symbol(
                        resolved,
                        canon,
                        ast.node(id_node).str_id,
                        SymbolKind::EnumVariant,
                    )
                {
                    push_occ(id_node, vkey, false);
                }
            }
            // `.name'builtin` view
            NodeKind::View => {
                let obj = ast.fixed_children(node)[0];
                if !obj.is_null()
                    && ast.node(obj).kind == NodeKind::Symbol
                    && let Some(&sym_id) = resolved.node_symbols.get(&node)
                {
                    let id_node = ast.fixed_children(obj)[0];
                    push_occ(id_node, SymbolKey::Sym(sym_id), false);
                }
            }
            // `use ...` import paths (`PathProjection`, `PathProjectionMulti`, `PathAsBind`)
            NodeKind::UseStatement => {
                let path_root = ast.fixed_children(node)[0];
                let encl_scope = resolved.node_scopes.get(&node).copied();
                record_use_path_occurrences(
                    resolved,
                    assoc_lookup,
                    encl_scope,
                    path_root,
                    None,
                    &mut push_occ,
                );
            }
            _ => {}
        }
    }

    for occs in index.file_occurrences.values_mut() {
        occs.sort_by_key(|o| (o.span.0, o.span.1, !o.is_decl));
        occs.dedup_by(|a, b| a.span == b.span && a.target == b.target);
    }
}

fn record_use_path_occurrences<F>(
    resolved: &ResolvedAst,
    assoc_lookup: &AssocSymbolLookup,
    encl_scope: Option<ScopeId>,
    path_node: NodeIndex,
    base_module_ty: Option<TypeIndex>,
    push_occ: &mut F,
) -> Option<TypeIndex>
where
    F: FnMut(NodeIndex, SymbolKey, bool),
{
    if path_node.is_null() {
        return None;
    }
    let ast = &resolved.ast;
    let pool = &resolved.type_pool;
    match ast.node(path_node).kind {
        NodeKind::Id => {
            if let Some(&sym_id) = resolved.node_symbols.get(&path_node) {
                push_occ(path_node, SymbolKey::Sym(sym_id), false);
                let ty = resolved.symbols[sym_id.0 as usize].type_index;
                return (ty != TypeIndex::INVALID)
                    .then(|| pool.canonical_type(ty))
                    .flatten();
            }
            let name_id = ast.node(path_node).str_id;
            if let Some(mty) = base_module_ty
                && let Some(scope_indices) = assoc_lookup.scopes_by_type.get(&mty)
            {
                for &sc_idx in scope_indices {
                    if let Some(&sym_id) = resolved.scopes[sc_idx].bindings.get(&name_id) {
                        push_occ(path_node, SymbolKey::Sym(sym_id), false);
                        let ty = resolved.symbols[sym_id.0 as usize].type_index;
                        return (ty != TypeIndex::INVALID)
                            .then(|| pool.canonical_type(ty))
                            .flatten();
                    }
                }
            }
            if let Some(sc_id) = encl_scope
                && let Some(&sym_id) = resolved.scopes[sc_id.0 as usize].bindings.get(&name_id)
            {
                push_occ(path_node, SymbolKey::Sym(sym_id), false);
                let ty = resolved.symbols[sym_id.0 as usize].type_index;
                return (ty != TypeIndex::INVALID)
                    .then(|| pool.canonical_type(ty))
                    .flatten();
            }
            None
        }
        NodeKind::SuperPath | NodeKind::PackagePath | NodeKind::PathProjectionAll => {
            let inner = ast.fixed_children(path_node)[0];
            record_use_path_occurrences(
                resolved,
                assoc_lookup,
                encl_scope,
                inner,
                base_module_ty,
                push_occ,
            )
        }
        NodeKind::PathProjection => {
            let children = ast.fixed_children(path_node);
            let lhs_ty = record_use_path_occurrences(
                resolved,
                assoc_lookup,
                encl_scope,
                children[0],
                base_module_ty,
                push_occ,
            );
            record_use_path_occurrences(
                resolved,
                assoc_lookup,
                encl_scope,
                children[1],
                lhs_ty,
                push_occ,
            )
        }
        NodeKind::PathProjectionMulti => {
            let base = ast.fixed_children(path_node)[0];
            let lhs_ty = record_use_path_occurrences(
                resolved,
                assoc_lookup,
                encl_scope,
                base,
                base_module_ty,
                push_occ,
            );
            for &item in ast.multi_children(path_node) {
                record_use_path_occurrences(
                    resolved,
                    assoc_lookup,
                    encl_scope,
                    item,
                    lhs_ty,
                    push_occ,
                );
            }
            lhs_ty
        }
        NodeKind::PathAsBind => {
            let children = ast.fixed_children(path_node);
            let lhs_ty = record_use_path_occurrences(
                resolved,
                assoc_lookup,
                encl_scope,
                children[0],
                base_module_ty,
                push_occ,
            );
            if let Some(sc_id) = encl_scope {
                let alias_node = children[1];
                if !alias_node.is_null()
                    && let Some(&sym_id) = resolved.scopes[sc_id.0 as usize]
                        .bindings
                        .get(&ast.node(alias_node).str_id)
                {
                    push_occ(alias_node, SymbolKey::Sym(sym_id), false);
                }
            }
            lhs_ty
        }
        _ => None,
    }
}

fn trim_leading_trivia_offset(src: &str, lo: usize, hi: usize) -> usize {
    let Some(slice) = src.get(lo..hi) else {
        return lo;
    };
    let bytes = slice.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    while i < len {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
            }
            b'-' if i + 1 < len && bytes[i + 1] == b'-' => {
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' if i + 1 < len && bytes[i + 1] == b'-' => {
                i += 2;
                let mut depth = 1usize;
                while i < len && depth > 0 {
                    if i + 1 < len && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                        depth += 1;
                        i += 2;
                    } else if i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => return lo + i,
        }
    }
    lo
}

/// Extract leading `---`, `--`, or `{- ... -}` documentation comments immediately preceding `item_lo`.
/// Skips `--%` formatter directives and visibility/declaration modifiers.
/// Walks backward lazily via `rsplit('\n')` so symbols without doc comments allocate nothing.
pub fn extract_leading_doc(src: &str, item_lo: usize) -> Option<String> {
    if item_lo == 0 || item_lo > src.len() {
        return None;
    }
    let prefix = &src[..item_lo];
    let mut rev_iter = prefix.rsplit('\n');
    let tail_line = rev_iter.next()?.trim();
    if !tail_line.is_empty() && !is_modifier_only_line(tail_line) {
        return None;
    }
    let mut doc_lines = Vec::new();
    while let Some(raw_line) = rev_iter.next() {
        let trimmed = raw_line.trim_end_matches('\r').trim();
        if trimmed.is_empty() {
            if !doc_lines.is_empty() {
                break;
            }
            // Only attach comments directly adjacent to the declaration (no blank line gap).
            return None;
        }
        if is_modifier_only_line(trimmed) || trimmed.starts_with("--%") {
            continue;
        }
        if let Some(triple) = trimmed.strip_prefix("---") {
            // Ignore horizontal separator banners like `------` unless followed by text.
            if triple.chars().all(|c| c == '-') && !triple.is_empty() {
                break;
            }
            doc_lines.push(triple.strip_prefix(' ').unwrap_or(triple).to_owned());
            continue;
        }
        if let Some(double) = trimmed.strip_prefix("--") {
            if double.chars().all(|c| c == '-' || c == '=') && double.len() >= 4 {
                break;
            }
            doc_lines.push(double.strip_prefix(' ').unwrap_or(double).to_owned());
            continue;
        }
        if trimmed.ends_with("-}") {
            let mut block_lines = vec![trimmed];
            if !trimmed.starts_with("{-") {
                for blk_raw in rev_iter.by_ref() {
                    let cur = blk_raw.trim_end_matches('\r').trim();
                    block_lines.push(cur);
                    if cur.starts_with("{-") {
                        break;
                    }
                }
            }
            block_lines.reverse();
            let joined = block_lines.join("\n");
            if let Some(inner) = joined.strip_prefix("{-").and_then(|s| s.strip_suffix("-}")) {
                for bl in inner.lines().rev() {
                    doc_lines.push(bl.trim().to_owned());
                }
                continue;
            }
        }
        break;
    }
    if doc_lines.is_empty() {
        return None;
    }
    doc_lines.reverse();
    while doc_lines.first().is_some_and(|l| l.is_empty()) {
        doc_lines.remove(0);
    }
    while doc_lines.last().is_some_and(|l| l.is_empty()) {
        doc_lines.pop();
    }
    (!doc_lines.is_empty()).then(|| doc_lines.join("\n"))
}

fn extract_file_header_doc(src: &str) -> Option<String> {
    let mut doc_lines = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--%") {
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("---")
            .or_else(|| trimmed.strip_prefix("--"))
        {
            if rest.chars().all(|c| c == '-' || c == '=') && rest.len() >= 4 {
                continue;
            }
            doc_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        } else {
            break;
        }
    }
    (!doc_lines.is_empty()).then(|| doc_lines.join("\n"))
}

fn is_modifier_only_line(line: &str) -> bool {
    line.split_whitespace().all(|w| {
        matches!(
            w,
            "pub"
                | "private"
                | "async"
                | "derive"
                | "assoc"
                | "fn"
                | "struct"
                | "enum"
                | "trait"
                | "effect"
                | "typealias"
                | "newtype"
                | "mod"
                | "const"
                | "let"
                | "var"
                | "global"
                | "impl"
                | "extend"
        )
    })
}

/// Encode LSP 3.17 delta semantic tokens for `file` using lexer tokens + semantic index.
pub fn build_semantic_tokens(
    text: &str,
    line_idx: &LineIndex,
    file: &Path,
    index: Option<&SemanticIndex>,
) -> Vec<u32> {
    let (tokens, _) = lexer::tokenize(text);
    let mut raw_tokens: Vec<(u32, u32, u32, u32, u32)> = Vec::new(); // (line, start_u16, len_u16, ty, mods)

    for tok in &tokens {
        let lo = tok.from as usize;
        let hi = tok.to as usize;
        if hi <= lo || hi > text.len() {
            continue;
        }
        if matches!(
            tok.kind,
            TokenKind::Newline
                | TokenKind::Indent
                | TokenKind::Outdent
                | TokenKind::Sof
                | TokenKind::Eof
                | TokenKind::Invalid
        ) {
            continue;
        }

        let slice = &text[lo..hi];
        let classified: Option<(u32, u32)> = match tok.kind {
            TokenKind::Comment => {
                if slice.trim_start().starts_with("--%") {
                    Some((semantic_token_type::DECORATOR, 0))
                } else {
                    Some((semantic_token_type::COMMENT, 0))
                }
            }
            TokenKind::String
            | TokenKind::Char
            | TokenKind::FStringStart
            | TokenKind::FStringLiteral
            | TokenKind::FStringEnd => Some((semantic_token_type::STRING, 0)),
            TokenKind::Integer
            | TokenKind::IntBin
            | TokenKind::IntOct
            | TokenKind::IntHex
            | TokenKind::Real
            | TokenKind::RealSci => Some((semantic_token_type::NUMBER, 0)),
            TokenKind::KwSelfLower => Some((
                semantic_token_type::PARAMETER,
                semantic_token_modifier::READONLY,
            )),
            TokenKind::KwSelfCap => Some((semantic_token_type::TYPE, 0)),
            k if is_keyword_token(k) => Some((semantic_token_type::KEYWORD, 0)),
            TokenKind::Hash => Some((semantic_token_type::DECORATOR, 0)),
            k if is_operator_token(k) => Some((semantic_token_type::OPERATOR, 0)),
            TokenKind::Id | TokenKind::ArbitraryId => {
                classify_ident_semantic_token(slice, (lo, hi), file, index)
            }
            _ => None,
        };

        let Some((tok_ty, tok_mods)) = classified else {
            continue;
        };

        // Split multi-line tokens (e.g. `{- ... -}` comments or multi-line strings) per line.
        let mut seg_start = lo;
        for part in slice.split_inclusive('\n') {
            let clean = part.trim_end_matches(['\r', '\n']);
            if !clean.is_empty() {
                let pos = line_idx.offset_to_position(text, seg_start);
                let len_u16: usize = clean.chars().map(char::len_utf16).sum();
                if len_u16 > 0 {
                    raw_tokens.push((pos.line, pos.character, len_u16 as u32, tok_ty, tok_mods));
                }
            }
            seg_start += part.len();
        }
    }

    raw_tokens.sort_by_key(|t| (t.0, t.1));
    let mut encoded = Vec::with_capacity(raw_tokens.len() * 5);
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    for (line, start, len, ty, mods) in raw_tokens {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            start.saturating_sub(prev_start)
        } else {
            start
        };
        encoded.extend_from_slice(&[delta_line, delta_start, len, ty, mods]);
        prev_line = line;
        prev_start = start;
    }
    encoded
}

fn classify_ident_semantic_token(
    slice: &str,
    span: (usize, usize),
    file: &Path,
    index: Option<&SemanticIndex>,
) -> Option<(u32, u32)> {
    if let Some(idx) = index
        && let Some(occ) = idx.exact_occurrence_at(file, span)
        && let Some(sym) = idx.symbol(occ.target)
    {
        let mut mods = 0u32;
        if occ.is_decl {
            mods |= semantic_token_modifier::DECLARATION | semantic_token_modifier::DEFINITION;
        }
        if sym.is_mutable {
            mods |= semantic_token_modifier::MUTABLE;
        } else if matches!(
            sym.kind,
            lsp_symbol_kind::VARIABLE | lsp_symbol_kind::CONSTANT
        ) {
            mods |= semantic_token_modifier::READONLY;
        }
        if occ.is_write && !occ.is_decl {
            mods |= semantic_token_modifier::MODIFICATION | semantic_token_modifier::MUTABLE;
        }
        if sym.is_external {
            mods |= semantic_token_modifier::DEFAULT_LIBRARY;
        }
        if sym.is_async {
            mods |= semantic_token_modifier::ASYNC;
        }
        let ty = match sym.kind {
            lsp_symbol_kind::MODULE | lsp_symbol_kind::NAMESPACE | lsp_symbol_kind::PACKAGE => {
                semantic_token_type::NAMESPACE
            }
            lsp_symbol_kind::STRUCT => semantic_token_type::STRUCT,
            lsp_symbol_kind::ENUM => semantic_token_type::ENUM,
            lsp_symbol_kind::INTERFACE => semantic_token_type::INTERFACE,
            lsp_symbol_kind::CLASS => semantic_token_type::TYPE,
            lsp_symbol_kind::ENUM_MEMBER => semantic_token_type::ENUM_MEMBER,
            lsp_symbol_kind::FIELD | lsp_symbol_kind::PROPERTY => semantic_token_type::PROPERTY,
            lsp_symbol_kind::METHOD => semantic_token_type::METHOD,
            lsp_symbol_kind::FUNCTION => semantic_token_type::FUNCTION,
            lsp_symbol_kind::EVENT => semantic_token_type::DECORATOR,
            _ => {
                if sym.detail.starts_with("(parameter)") {
                    semantic_token_type::PARAMETER
                } else {
                    semantic_token_type::VARIABLE
                }
            }
        };
        return Some((ty, mods));
    }

    // Lexical fallback when buffer is mid-edit.
    let unquoted = slice.trim_matches('`');
    if unquoted
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
    {
        Some((semantic_token_type::TYPE, 0))
    } else {
        Some((semantic_token_type::VARIABLE, 0))
    }
}

fn is_keyword_token(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwAnd
            | TokenKind::KwAs
            | TokenKind::KwAssoc
            | TokenKind::KwAsync
            | TokenKind::KwAtomic
            | TokenKind::KwAwait
            | TokenKind::KwBreak
            | TokenKind::KwCase
            | TokenKind::KwCatch
            | TokenKind::KwConst
            | TokenKind::KwContinue
            | TokenKind::KwDef
            | TokenKind::KwDefer
            | TokenKind::KwDerive
            | TokenKind::KwDo
            | TokenKind::KwEffect
            | TokenKind::KwElse
            | TokenKind::KwEnum
            | TokenKind::KwError
            | TokenKind::KwExtend
            | TokenKind::KwExtern
            | TokenKind::KwFalse
            | TokenKind::KwFn
            | TokenKind::KwFor
            | TokenKind::KwGlobal
            | TokenKind::KwHandles
            | TokenKind::KwIf
            | TokenKind::KwImpl
            | TokenKind::KwIn
            | TokenKind::KwIs
            | TokenKind::KwItself
            | TokenKind::KwLambda
            | TokenKind::KwLet
            | TokenKind::KwMatch
            | TokenKind::KwMatches
            | TokenKind::KwMod
            | TokenKind::KwNewtype
            | TokenKind::KwNot
            | TokenKind::KwNull
            | TokenKind::KwOr
            | TokenKind::KwPrivate
            | TokenKind::KwPub
            | TokenKind::KwReset
            | TokenKind::KwResume
            | TokenKind::KwReturn
            | TokenKind::KwShift
            | TokenKind::KwStatic
            | TokenKind::KwStruct
            | TokenKind::KwTest
            | TokenKind::KwTrait
            | TokenKind::KwTrue
            | TokenKind::KwTypealias
            | TokenKind::KwUse
            | TokenKind::KwVar
            | TokenKind::KwWhen
            | TokenKind::KwWhile
            | TokenKind::KwWhere
    )
}

fn is_operator_token(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Bang
            | TokenKind::BangEq
            | TokenKind::Eq
            | TokenKind::EqEq
            | TokenKind::FatArrow
            | TokenKind::Plus
            | TokenKind::PlusPlus
            | TokenKind::PlusEq
            | TokenKind::Minus
            | TokenKind::MinusEq
            | TokenKind::Arrow
            | TokenKind::Star
            | TokenKind::StarEq
            | TokenKind::Slash
            | TokenKind::SlashEq
            | TokenKind::Percent
            | TokenKind::PercentEq
            | TokenKind::PipeGt
            | TokenKind::Lt
            | TokenKind::LtEq
            | TokenKind::Gt
            | TokenKind::GtEq
            | TokenKind::Question
            | TokenKind::At
            | TokenKind::Quote
    )
}
