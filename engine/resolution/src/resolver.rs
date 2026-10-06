use ast::{Ast, NodeIndex};
use diagnostic::{Diagnostic, DiagnosticContext};
use runtime::{catalog_lookup, BuiltinFnId, BuiltinKind, BUILTIN_FN_META};
use str_interner::StrId;
use type_pool::{TypeId, TypeIndex, TypeInfo, TypeKind, TypePool};

use std::collections::HashMap;

use crate::{
    EffectInfo, NodeSymbolMap, NodeTypeMap, ResolveOptions, ResolvedAst, Scope, ScopeId, Symbol,
    SymbolId, SymbolKind, TraitInfo, Visibility,
};

// ---------------------------------------------------------------------------
// Resolver — the main resolution engine
// ---------------------------------------------------------------------------

/// The resolution engine.  Consumes an AST and produces a [`ResolvedAst`].
///
/// Resolution proceeds in four phases:
///   3a. Name resolution  — build scope tree, bind all identifiers
///   3b. Type resolution  — infer types for expressions
///   3c. Effect resolution — collect effect declarations, extract operations
///   3d. Trait resolution  — collect trait info, record impl relationships
pub(crate) struct Resolver<'a> {
    pub(crate) type_pool: TypePool,
    pub(crate) scopes: Vec<Scope>,
    pub(crate) symbols: Vec<Symbol>,
    pub(crate) node_symbols: NodeSymbolMap,
    pub(crate) node_types: NodeTypeMap,
    pub(crate) effects: Vec<EffectInfo>,
    pub(crate) traits: Vec<TraitInfo>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) diag_ctx: &'a DiagnosticContext<'a>,
    pub(crate) options: ResolveOptions,
    /// Maps SymbolId → BuiltinFnId for builtin function symbols.
    pub(crate) builtin_fns: HashMap<SymbolId, BuiltinFnId>,
    /// Maps enum variant SymbolId → variant index (0-based).
    pub(crate) enum_variant_indices: HashMap<SymbolId, u32>,
    /// Maps Projection node → field index (populated in type resolution).
    pub(crate) node_field_indices: HashMap<NodeIndex, u32>,
    next_symbol_id: u32,
    pub(crate) current_scope: ScopeId,
}

impl<'a> Resolver<'a> {
    pub(crate) fn new(diag_ctx: &'a DiagnosticContext<'a>, options: ResolveOptions) -> Self {
        let root_scope = Scope {
            id: ScopeId::ROOT,
            parent: None,
            bindings: HashMap::new(),
            children: Vec::new(),
            assoc_type: None,
            node: NodeIndex::NULL,
        };
        let mut resolver = Self {
            type_pool: TypePool::with_intrinsics(),
            scopes: vec![root_scope],
            symbols: Vec::new(),
            node_symbols: HashMap::new(),
            node_types: HashMap::new(),
            effects: Vec::new(),
            traits: Vec::new(),
            diagnostics: Vec::new(),
            diag_ctx,
            options,
            builtin_fns: HashMap::new(),
            enum_variant_indices: HashMap::new(),
            node_field_indices: HashMap::new(),
            next_symbol_id: 0,
            current_scope: ScopeId::ROOT,
        };
        if resolver.options.expose_root_builtins {
            resolver.register_root_builtin_fns();
        }
        resolver.register_builtin_types();
        resolver
    }

    /// Pre-register builtin functions as root-scope symbols (script mode).
    fn register_root_builtin_fns(&mut self) {
        for meta in BUILTIN_FN_META {
            let name = str_interner::intern(meta.name);
            let sym_id = self.define_symbol(
                name,
                SymbolKind::BuiltinFunction(meta.id),
                NodeIndex::NULL,
                Visibility::Public,
            );
            self.builtin_fns.insert(sym_id, meta.id);
        }
    }

    /// Pre-register all intrinsic type names in the root scope.
    fn register_builtin_types(&mut self) {
        use type_pool::Intrinsic;
        for &intr in Intrinsic::ALL {
            if matches!(intr, Intrinsic::Closure) {
                continue;
            }
            let name = str_interner::intern(intr.name());
            let sym_id =
                self.define_symbol(name, SymbolKind::Type, NodeIndex::NULL, Visibility::Public);
            self.symbol_mut(sym_id).type_index = intr.type_index();
        }
        // Also ensure catalog has type entries for `'builtin` type views.
        runtime::catalog_register_intrinsic_types(&self.type_pool);
        self.register_well_known_traits();
    }

    /// Pre-register well-known trait names in the root scope.
    fn register_well_known_traits(&mut self) {
        use type_pool::WellKnownTraits;
        let wk = self.type_pool.well_known;
        let indices = [
            wk.display,
            wk.hash,
            wk.eq,
            wk.ord,
            wk.partial_eq,
            wk.partial_ord,
            wk.iterator,
            wk.into_iterator,
        ];
        for (i, &name) in WellKnownTraits::NAMES.iter().enumerate() {
            let name_str = str_interner::intern(name);
            let sym_id = self.define_symbol(
                name_str,
                SymbolKind::Trait,
                NodeIndex::NULL,
                Visibility::Public,
            );
            self.symbol_mut(sym_id).type_index = indices[i];
        }
    }

    /// Run the full resolution pipeline on the given AST.
    pub(crate) fn resolve(mut self, ast: Ast) -> ResolvedAst {
        // Phase 3a: Name resolution — walk the AST, build scope tree, resolve names.
        crate::name::resolve_names(&mut self, &ast, ast.root);

        // Phase 3b: Type resolution — infer types for expressions.
        crate::typing::resolve_types(&mut self, &ast, ast.root);

        // Phase 3c: Effect resolution.
        crate::effect::resolve_effects(&mut self, &ast);

        // Phase 3d: Trait & method resolution.
        crate::traits::resolve_traits(&mut self, &ast);

        ResolvedAst {
            ast,
            type_pool: self.type_pool,
            scopes: self.scopes,
            symbols: self.symbols,
            node_symbols: self.node_symbols,
            node_types: self.node_types,
            effects: self.effects,
            traits: self.traits,
            diagnostics: self.diagnostics,
            builtin_fns: self.builtin_fns,
            enum_variant_indices: self.enum_variant_indices,
            node_field_indices: self.node_field_indices,
        }
    }

    // ─── Scope management ──────────────────────────────────────────

    /// Push a new child scope under the current scope, optionally associated
    /// with a type.  Returns the new scope's id.
    pub(crate) fn push_scope(&mut self, node: NodeIndex, assoc_type: Option<TypeIndex>) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        let scope = Scope {
            id,
            parent: Some(self.current_scope),
            bindings: HashMap::new(),
            children: Vec::new(),
            assoc_type,
            node,
        };
        self.scopes.push(scope);
        self.scopes[self.current_scope.0 as usize].children.push(id);
        self.current_scope = id;
        id
    }

    /// Pop back to the parent scope.
    pub(crate) fn pop_scope(&mut self) {
        let parent = self.scopes[self.current_scope.0 as usize]
            .parent
            .unwrap_or(ScopeId::ROOT);
        self.current_scope = parent;
    }

    // ─── Symbol management ─────────────────────────────────────────

    /// Define a new symbol in the current scope.
    pub(crate) fn define_symbol(
        &mut self,
        name: StrId,
        kind: SymbolKind,
        def_node: NodeIndex,
        visibility: Visibility,
    ) -> SymbolId {
        let id = SymbolId(self.next_symbol_id);
        self.next_symbol_id += 1;
        let symbol = Symbol {
            id,
            name,
            kind,
            scope: self.current_scope,
            type_index: TypeIndex::INVALID,
            visibility,
            def_node,
        };
        self.symbols.push(symbol);
        self.scopes[self.current_scope.0 as usize]
            .bindings
            .insert(name, id);
        id
    }

    /// Allocate a symbol that is not inserted into any scope binding table.
    /// Used for `'builtin` view results.
    pub(crate) fn alloc_unbound_symbol(
        &mut self,
        name: StrId,
        kind: SymbolKind,
        def_node: NodeIndex,
    ) -> SymbolId {
        let id = SymbolId(self.next_symbol_id);
        self.next_symbol_id += 1;
        self.symbols.push(Symbol {
            id,
            name,
            kind,
            scope: self.current_scope,
            type_index: TypeIndex::INVALID,
            visibility: Visibility::Private,
            def_node,
        });
        id
    }

    /// Look up a name in the current scope chain (walks up to root).
    pub(crate) fn lookup(&self, name: StrId) -> Option<SymbolId> {
        let mut scope_id = self.current_scope;
        loop {
            if let Some(&sym_id) = self.scopes[scope_id.0 as usize].bindings.get(&name) {
                return Some(sym_id);
            }
            match self.scopes[scope_id.0 as usize].parent {
                Some(parent) => scope_id = parent,
                None => return None,
            }
        }
    }

    /// Look up a name in the scope associated with a given type.
    pub(crate) fn lookup_in_type_scope(
        &self,
        type_idx: TypeIndex,
        name: StrId,
    ) -> Option<SymbolId> {
        for scope in &self.scopes {
            if scope.assoc_type == Some(type_idx) {
                if let Some(&sym_id) = scope.bindings.get(&name) {
                    return Some(sym_id);
                }
            }
        }
        None
    }

    /// Look up a name only in the current scope (not walking the parent chain).
    pub(crate) fn lookup_current_scope(&self, name: StrId) -> Option<SymbolId> {
        let scope = &self.scopes[self.current_scope.0 as usize];
        scope.bindings.get(&name).copied()
    }

    pub(crate) fn symbol_mut(&mut self, id: SymbolId) -> &mut Symbol {
        &mut self.symbols[id.0 as usize]
    }

    pub(crate) fn register_type(&mut self, kind: TypeKind) -> TypeIndex {
        self.type_pool.register(TypeInfo {
            kind,
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        })
    }

    /// Resolve a `'builtin` view against the shared catalog.
    pub(crate) fn resolve_builtin_view(
        &mut self,
        view_node: NodeIndex,
        symbol_name: &str,
    ) -> Option<SymbolId> {
        match catalog_lookup(symbol_name) {
            Some(BuiltinKind::Fn(id)) => {
                let name = str_interner::intern(symbol_name);
                let sym_id = self.alloc_unbound_symbol(
                    name,
                    SymbolKind::BuiltinFunction(id),
                    view_node,
                );
                self.builtin_fns.insert(sym_id, id);
                Some(sym_id)
            }
            Some(BuiltinKind::Type(ti)) => {
                let name = str_interner::intern(symbol_name);
                let sym_id =
                    self.alloc_unbound_symbol(name, SymbolKind::Type, view_node);
                self.symbol_mut(sym_id).type_index = ti;
                Some(sym_id)
            }
            Some(BuiltinKind::Effect(_)) => None,
            None => None,
        }
    }
}
