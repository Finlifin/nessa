use ast::{Ast, NodeIndex};
use diagnostic::{Diagnostic, DiagnosticContext};
use runtime::{BUILTIN_FN_META, BuiltinFnId, BuiltinKind, catalog_lookup};
use str_interner::StrId;
use type_pool::{TypeId, TypeIndex, TypeInfo, TypeKind, TypePool};

use std::collections::{HashMap, HashSet};

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
    pub(crate) node_type_values: HashMap<NodeIndex, TypeIndex>,
    pub(crate) node_coercions: HashMap<NodeIndex, crate::Coercion>,
    pub(crate) error_constructions: HashMap<NodeIndex, crate::ErrorConstructionPlan>,
    pub(crate) error_conversions: HashMap<NodeIndex, Vec<crate::ErrorConversionPlan>>,
    pub(crate) error_propagations: HashMap<NodeIndex, crate::ErrorPropagationPlan>,
    pub(crate) error_eliminations: HashMap<NodeIndex, crate::ErrorEliminationPlan>,
    pub(crate) error_patterns: HashMap<NodeIndex, crate::ErrorPatternPlan>,
    pub(crate) call_arguments: HashMap<NodeIndex, crate::CallArgumentPlan>,
    pub(crate) current_call_callee: Option<NodeIndex>,
    pub(crate) constructor_types: HashMap<NodeIndex, SymbolId>,
    pub(crate) struct_constructions: HashMap<NodeIndex, crate::StructConstructionPlan>,
    pub(crate) enum_constructions: HashMap<NodeIndex, crate::EnumConstructionPlan>,
    pub(crate) enum_variants: HashMap<NodeIndex, crate::EnumVariantRef>,
    pub(crate) for_loops: HashMap<NodeIndex, crate::ForLoopPlan>,
    pub(crate) derived_comparisons: Vec<crate::DerivedComparisonPlan>,
    pub(crate) display_derivations: Vec<crate::DerivedDisplayPlan>,
    pub(crate) default_methods: Vec<crate::DefaultMethodPlan>,
    pub(crate) default_body_context: Option<crate::default_methods::DefaultBodyContext>,
    pub(crate) concat_calls: HashMap<NodeIndex, SymbolId>,
    pub(crate) instance_methods: HashMap<NodeIndex, SymbolId>,
    pub(crate) application_calls: HashMap<NodeIndex, SymbolId>,
    pub(crate) update_calls: HashMap<NodeIndex, SymbolId>,
    pub(crate) function_inference: crate::inference::InferenceState,
    pub(crate) node_scopes: HashMap<NodeIndex, ScopeId>,
    pub(crate) return_types: Vec<Option<TypeIndex>>,
    pub(crate) inferred_returns: Vec<Vec<TypeIndex>>,
    pub(crate) effects: Vec<EffectInfo>,
    /// Resume inputs known from stable catch bindings and snapshot aliases.
    pub(crate) continuation_inputs: HashMap<SymbolId, TypeIndex>,
    /// CaseArm is shared by ordinary matches and independently called handlers.
    pub(crate) handler_arms: HashSet<NodeIndex>,
    pub(crate) traits: Vec<TraitInfo>,
    pub(crate) trait_obligations: Vec<crate::trait_typing::TraitObligation>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) diag_ctx: &'a DiagnosticContext<'a>,
    pub(crate) options: ResolveOptions,
    pub(crate) imports: crate::imports::ImportState,
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
            node_coercions: HashMap::new(),
            error_constructions: HashMap::new(),
            error_conversions: HashMap::new(),
            error_propagations: HashMap::new(),
            error_eliminations: HashMap::new(),
            error_patterns: HashMap::new(),
            call_arguments: HashMap::new(),
            current_call_callee: None,
            constructor_types: HashMap::new(),
            struct_constructions: HashMap::new(),
            enum_constructions: HashMap::new(),
            enum_variants: HashMap::new(),
            for_loops: HashMap::new(),
            derived_comparisons: Vec::new(),
            display_derivations: Vec::new(),
            default_methods: Vec::new(),
            default_body_context: None,
            concat_calls: HashMap::new(),
            instance_methods: HashMap::new(),
            application_calls: HashMap::new(),
            update_calls: HashMap::new(),
            function_inference: Default::default(),
            node_scopes: HashMap::new(),
            node_type_values: HashMap::new(),
            return_types: Vec::new(),
            inferred_returns: Vec::new(),
            effects: Vec::new(),
            continuation_inputs: HashMap::new(),
            handler_arms: HashSet::new(),
            traits: Vec::new(),
            trait_obligations: Vec::new(),
            diagnostics: Vec::new(),
            diag_ctx,
            options,
            imports: Default::default(),
            builtin_fns: HashMap::new(),
            enum_variant_indices: HashMap::new(),
            node_field_indices: HashMap::new(),
            next_symbol_id: 0,
            current_scope: ScopeId::ROOT,
        };
        crate::trait_loops::prepare_bootstrap(&mut resolver.type_pool);
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
            if !self.options.expose_root_types {
                continue;
            }
            let name = str_interner::intern(intr.name());
            let sym_id =
                self.define_symbol(name, SymbolKind::Type, NodeIndex::NULL, Visibility::Public);
            self.symbol_mut(sym_id).type_index = intr.type_index();
        }
        // Also ensure catalog has type entries for `'builtin` type views.
        runtime::catalog_register_intrinsic_types(&self.type_pool);
        if self.options.expose_root_types {
            self.define_symbol(
                str_interner::intern("IterationStep"),
                SymbolKind::TypeFactory(crate::TypeFactory::IterationStep),
                NodeIndex::NULL,
                Visibility::Public,
            );
        }
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
            runtime::catalog_register_type(name, indices[i]);
            if !self.options.expose_root_types {
                continue;
            }
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
    pub(crate) fn resolve(mut self, mut ast: Ast) -> ResolvedAst {
        crate::post_do::normalize(&mut ast);
        // Phase 3a: Name resolution — walk the AST, build scope tree, resolve names.
        crate::name::resolve_names(&mut self, &ast, ast.root);
        crate::associated::validate(&self, &ast);

        // Phase 3b: Type resolution — infer types for expressions.
        crate::typing::prepare_type_aliases_staged(&mut self, &ast, false);
        crate::traits::prepare_trait_parents(&mut self, &ast);
        crate::associated_types::prepare(&mut self, &ast);
        crate::typing::prepare_type_aliases(&mut self, &ast);
        crate::ordering::prepare(&mut self);
        crate::access::publish_scopes(&mut self);
        crate::associated_types::stage_implementations(&mut self, &ast);
        // Effect annotations can use prepared aliases and associated types.
        // Complete every header before defaults, bodies and handler contracts.
        crate::effect::resolve_effects(&mut self, &ast);
        crate::effect::resolve_handler_parameters(&mut self, &ast, ast.root);
        crate::effect_contracts::prepare_inputs(&mut self, &ast);
        crate::typing::resolve_types(&mut self, &ast, ast.root);
        crate::associated::validate_constructors(&self, &ast);
        crate::defaults::validate_default_expansions(&self, &ast);
        crate::typing::validate_native_values(&self, &ast);
        crate::initialization::validate(&self, &ast);

        // Phase 3d: Trait & method resolution.
        crate::access::publish_scopes(&mut self);
        crate::traits::resolve_traits(&mut self, &ast);
        crate::associated_types::validate_dynamic_functions(&self, &ast);
        crate::associated_types::validate_abstract_values(&self, &ast);
        crate::default_methods::bind_calls(&mut self, &ast);
        crate::trait_typing::validate(&self, &ast);
        crate::trait_loops::validate(&mut self, &ast);
        if !self.diag_ctx.has_errors()
            && !self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.level == diagnostic::Level::Error)
        {
            crate::identity::finalize(&mut self, &ast);
        }

        ResolvedAst {
            ast,
            type_pool: self.type_pool,
            scopes: self.scopes,
            symbols: self.symbols,
            node_symbols: self.node_symbols,
            node_types: self.node_types,
            node_coercions: self.node_coercions,
            error_constructions: self.error_constructions,
            error_conversions: self.error_conversions,
            error_propagations: self.error_propagations,
            error_eliminations: self.error_eliminations,
            error_patterns: self.error_patterns,
            call_arguments: self.call_arguments,
            constructor_types: self.constructor_types,
            struct_constructions: self.struct_constructions,
            enum_constructions: self.enum_constructions,
            enum_variants: self.enum_variants,
            for_loops: self.for_loops,
            derived_comparisons: self.derived_comparisons,
            display_derivations: self.display_derivations,
            default_methods: self.default_methods,
            concat_calls: self.concat_calls,
            instance_methods: self.instance_methods,
            application_calls: self.application_calls,
            update_calls: self.update_calls,
            node_scopes: self.node_scopes,
            node_type_values: self.node_type_values,
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
        self.lookup_with_visibility(name).ok().flatten()
    }

    /// Preserve inaccessible bindings as errors rather than falling through to
    /// a differently scoped declaration with the same name.
    pub(crate) fn lookup_with_visibility(&self, name: StrId) -> Result<Option<SymbolId>, SymbolId> {
        let mut scope_id = self.current_scope;
        loop {
            if let Some(&sym_id) = self.scopes[scope_id.0 as usize].bindings.get(&name) {
                return if crate::imports::binding_accessible(self, scope_id, name, sym_id) {
                    Ok(Some(sym_id))
                } else {
                    Err(sym_id)
                };
            }
            if let Some(ty) = self.scopes[scope_id.0 as usize].assoc_type
                && let Ok(symbol) = crate::associated::member(self, ty, name)
            {
                return Ok(Some(symbol));
            }
            if let Some(ty) = self.scopes[scope_id.0 as usize].assoc_type {
                let mut parents = vec![ty];
                let mut seen = std::collections::HashSet::new();
                let mut inherited = None;
                while let Some(parent) = parents.pop() {
                    if !seen.insert(parent) {
                        continue;
                    }
                    if parent != ty
                        && let Ok(symbol) = crate::associated::member(self, parent, name)
                        && self.symbols[symbol.0 as usize].kind == SymbolKind::Type
                    {
                        if self.symbols[symbol.0 as usize].def_node.is_null() {
                            continue;
                        }
                        if let Some(previous) = inherited
                            && previous != symbol
                        {
                            return Ok(None);
                        }
                        inherited = Some(symbol);
                    }
                    if let TypeKind::Trait {
                        parents: ancestors, ..
                    } = &self.type_pool.get(parent).kind
                    {
                        parents.extend(ancestors);
                    }
                }
                if inherited.is_some() {
                    return Ok(inherited);
                }
            }
            if self
                .options
                .detached_package_roots
                .contains(&self.scopes[scope_id.0 as usize].node)
            {
                return Ok(None);
            }
            let Some(parent) = self.scopes[scope_id.0 as usize].parent else {
                return Ok(None);
            };
            scope_id = parent;
        }
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
        // Declared effects use their pool index as a nominal handler identity.
        // Their current shape lacks a name; equal signatures must stay distinct.
        if matches!(kind, TypeKind::Effect { .. }) {
            return self.type_pool.register(TypeInfo {
                kind,
                type_id: TypeId::ZERO,
                size: 0,
                align: 0,
            });
        }
        self.type_pool.intern_structural(kind)
    }

    /// Resolve a `'builtin` view against the shared catalog.
    pub(crate) fn resolve_builtin_view(
        &mut self,
        view_node: NodeIndex,
        symbol_name: &str,
    ) -> Option<SymbolId> {
        if symbol_name == "IterationStep" {
            return Some(self.alloc_unbound_symbol(
                str_interner::intern(symbol_name),
                SymbolKind::TypeFactory(crate::TypeFactory::IterationStep),
                view_node,
            ));
        }
        if matches!(symbol_name, "List" | "Map") {
            let ty = if symbol_name == "List" {
                self.type_pool.list_type()?
            } else {
                self.type_pool.map_type()?
            };
            let symbol = self.alloc_unbound_symbol(
                str_interner::intern(symbol_name),
                SymbolKind::Type,
                view_node,
            );
            self.symbol_mut(symbol).type_index = ty;
            return Some(symbol);
        }
        match catalog_lookup(symbol_name) {
            Some(BuiltinKind::Fn(id)) => {
                let name = str_interner::intern(symbol_name);
                let sym_id =
                    self.alloc_unbound_symbol(name, SymbolKind::BuiltinFunction(id), view_node);
                self.builtin_fns.insert(sym_id, id);
                Some(sym_id)
            }
            Some(BuiltinKind::Type(ti)) => {
                let name = str_interner::intern(symbol_name);
                let sym_id = self.alloc_unbound_symbol(name, SymbolKind::Type, view_node);
                self.symbol_mut(sym_id).type_index = ti;
                Some(sym_id)
            }
            Some(BuiltinKind::Effect(_)) => None,
            None => None,
        }
    }
}
