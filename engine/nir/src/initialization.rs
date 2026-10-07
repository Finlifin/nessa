//! Shared module storage and dependency-ordered startup planning.
//!
//! Initializers preserve declaration order within a module. Runtime global
//! checks handle dynamic dependencies and same-module forward reads. Import
//! cycles alone do not imply a cycle in executable initialization work.

use std::collections::{BTreeSet, HashMap, HashSet};

use ast::{NodeIndex, NodeKind};
use nsbc::{GlobalId, GlobalInfo};
use resolution::{ResolvedAst, ScopeId, SymbolId, SymbolKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::LoweringError;

mod callbacks;

pub(crate) struct ScopeInitialization {
    pub node: NodeIndex,
    pub statements: Vec<NodeIndex>,
    pub hook: Option<SymbolId>,
}

pub(crate) struct InitializationPlan {
    pub globals: Vec<GlobalInfo>,
    pub slots: HashMap<SymbolId, GlobalId>,
    pub ordered: Vec<ScopeInitialization>,
}

#[derive(Clone, Copy)]
struct DefaultScan<'a> {
    body: &'a resolution::DefaultMethodPlan,
    root: &'a resolution::DefaultMethodPlan,
}

struct Planner<'a> {
    resolved: &'a ResolvedAst,
    scopes: Vec<ScopeId>,
    scope_indices: HashMap<ScopeId, usize>,
    node_owners: HashMap<NodeIndex, usize>,
    scope_nodes: HashMap<NodeIndex, usize>,
    statements: Vec<Vec<NodeIndex>>,
    hooks: Vec<Option<SymbolId>>,
    imports: Vec<BTreeSet<usize>>,
    references: Vec<BTreeSet<usize>>,
    prerequisites: Vec<BTreeSet<usize>>,
    slots: HashMap<SymbolId, GlobalId>,
    globals: Vec<GlobalInfo>,
    receiver_aliases: HashMap<SymbolId, HashSet<SymbolId>>,
    callback_sources: HashMap<Option<SymbolId>, callbacks::Sources>,
}

impl InitializationPlan {
    pub fn build(resolved: &ResolvedAst) -> Result<Self, Vec<LoweringError>> {
        Planner::new(resolved).build()
    }
}

impl<'a> Planner<'a> {
    fn new(resolved: &'a ResolvedAst) -> Self {
        let scopes: Vec<_> = resolved
            .scopes
            .iter()
            .filter(|scope| {
                !scope.node.is_null()
                    && matches!(
                        resolved.ast.node(scope.node).kind,
                        NodeKind::FileScope
                            | NodeKind::ModuleDef
                            | NodeKind::StructDef
                            | NodeKind::EnumDef
                            | NodeKind::ImplDef
                    )
            })
            .map(|scope| scope.id)
            .collect();
        let count = scopes.len();
        let scope_indices = scopes
            .iter()
            .enumerate()
            .map(|(index, &scope)| (scope, index))
            .collect();
        let scope_nodes = scopes
            .iter()
            .enumerate()
            .map(|(index, scope)| (resolved.scopes[scope.0 as usize].node, index))
            .collect();
        Self {
            resolved,
            scopes,
            scope_indices,
            scope_nodes,
            node_owners: HashMap::new(),
            statements: vec![Vec::new(); count],
            hooks: vec![None; count],
            imports: vec![BTreeSet::new(); count],
            references: vec![BTreeSet::new(); count],
            prerequisites: vec![BTreeSet::new(); count],
            slots: HashMap::new(),
            globals: Vec::new(),
            callback_sources: callbacks::sources(resolved),
            receiver_aliases: resolved
                .default_methods
                .iter()
                .map(|plan| (plan.function, receiver_aliases(resolved, plan)))
                .collect(),
        }
    }

    fn owner_of_scope(&self, mut scope: ScopeId) -> Option<usize> {
        loop {
            if let Some(&owner) = self.scope_indices.get(&scope) {
                return Some(owner);
            }
            scope = self.resolved.scopes[scope.0 as usize].parent?;
        }
    }

    fn owner_of_symbol(&self, symbol: SymbolId) -> Option<usize> {
        let symbol = &self.resolved.symbols[symbol.0 as usize];
        if matches!(symbol.kind, SymbolKind::Module | SymbolKind::Type) {
            if let Some(&owner) = self.scope_nodes.get(&symbol.def_node) {
                return Some(owner);
            }
            if symbol.kind == SymbolKind::Type {
                let target = self.resolved.type_pool.canonical_type(symbol.type_index)?;
                if let Some((owner, _)) = self.scopes.iter().enumerate().find(|(_, scope)| {
                    self.resolved.scopes[scope.0 as usize]
                        .assoc_type
                        .and_then(|ty| self.resolved.type_pool.canonical_type(ty))
                        == Some(target)
                }) {
                    return Some(owner);
                }
            }
        }
        self.owner_of_scope(symbol.scope)
    }

    fn owners_of_symbol(&self, symbol: SymbolId) -> Vec<usize> {
        let mut owners: Vec<_> = self.owner_of_symbol(symbol).into_iter().collect();
        let definition = &self.resolved.symbols[symbol.0 as usize];
        if !definition.def_node.is_null()
            && self.resolved.ast.node(definition.def_node).kind == NodeKind::Typealias
            && let Some(owner) = self.owner_of_scope(definition.scope)
            && !owners.contains(&owner)
        {
            owners.push(owner);
        }
        owners
    }

    fn mark_owners(&mut self, node: NodeIndex, inherited: Option<usize>) {
        if node.is_null() {
            return;
        }
        let owner = self.scope_nodes.get(&node).copied().or(inherited);
        if let Some(owner) = owner {
            self.node_owners.insert(node, owner);
        }
        for child in self
            .resolved
            .ast
            .fixed_children(node)
            .iter()
            .chain(self.resolved.ast.multi_children(node))
            .copied()
        {
            self.mark_owners(child, owner);
        }
    }

    fn unwrap(&self, mut node: NodeIndex) -> NodeIndex {
        while matches!(
            self.resolved.ast.node(node).kind,
            NodeKind::PubDef | NodeKind::PrivateDef | NodeKind::GlobalDecl
        ) {
            node = self.resolved.ast.fixed_children(node)[0];
        }
        node
    }

    fn collect_storage(&mut self) {
        for owner in 0..self.scopes.len() {
            let scope = &self.resolved.scopes[self.scopes[owner].0 as usize];
            let hook = scope
                .bindings
                .get(&str_interner::intern("__init__"))
                .copied();
            self.hooks[owner] = hook.filter(|symbol| {
                let symbol = &self.resolved.symbols[symbol.0 as usize];
                symbol.kind == SymbolKind::Function && symbol.scope == scope.id
            });
            for &item in self.resolved.ast.multi_children(scope.node) {
                let declaration = self.unwrap(item);
                let kind = self.resolved.ast.node(declaration).kind;
                match kind {
                    NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl => {
                        let pattern = self.resolved.ast.fixed_children(declaration)[0];
                        let Some(&symbol) = self.resolved.node_symbols.get(&pattern) else {
                            continue;
                        };
                        let definition = &self.resolved.symbols[symbol.0 as usize];
                        if definition.scope != scope.id {
                            continue;
                        }
                        if !matches!(definition.kind, SymbolKind::Variable | SymbolKind::Constant) {
                            continue;
                        }
                        let global = GlobalId(self.globals.len() as u32);
                        self.slots.insert(symbol, global);
                        self.globals.push(GlobalInfo {
                            type_index: if definition.type_index == TypeIndex::INVALID {
                                Intrinsic::Any.type_index()
                            } else {
                                definition.type_index
                            },
                            is_mutable: kind != NodeKind::ConstDecl,
                        });
                        self.statements[owner].push(declaration);
                    }
                    NodeKind::FunctionDef
                    | NodeKind::ModuleDef
                    | NodeKind::StructDef
                    | NodeKind::EnumDef
                    | NodeKind::TraitDef
                    | NodeKind::TraitDefFn
                    | NodeKind::TraitDeriveFn
                    | NodeKind::EffectDef
                    | NodeKind::AsyncEffectDef
                    | NodeKind::ImplDef
                    | NodeKind::ImplTraitDef
                    | NodeKind::ExtendDef
                    | NodeKind::ExtendTraitDef
                    | NodeKind::DeriveDef
                    | NodeKind::Typealias
                    | NodeKind::Newtype
                    | NodeKind::UseStatement
                    | NodeKind::AssocDecl
                    | NodeKind::StructField
                    | NodeKind::EnumVariant => {}
                    _ => self.statements[owner].push(declaration),
                }
            }
        }
    }

    fn validate_shared_initializers(&self) -> Vec<LoweringError> {
        let mut errors = Vec::new();
        for owner in 0..self.scopes.len() {
            let mut pending: Vec<_> = self.statements[owner]
                .iter()
                .map(|&node| (node, None))
                .collect();
            let mut visited_helpers = HashSet::new();
            if let Some(hook) = self.hooks[owner] {
                pending.push((
                    self.resolved
                        .ast
                        .fixed_children(self.resolved.symbols[hook.0 as usize].def_node)[2],
                    None,
                ));
            }
            while let Some((node, helper_scope)) = pending.pop() {
                if node.is_null() {
                    continue;
                }
                if let Some(&symbol) = self.resolved.node_symbols.get(&node) {
                    let definition = &self.resolved.symbols[symbol.0 as usize];
                    if matches!(
                        definition.kind,
                        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant
                    ) && !self.slots.contains_key(&symbol)
                    {
                        let mut scope = Some(definition.scope);
                        let mut local = false;
                        while let Some(current) = scope {
                            if current == self.scopes[owner] || Some(current) == helper_scope {
                                local = true;
                                break;
                            }
                            scope = self.resolved.scopes[current.0 as usize].parent;
                        }
                        if !local {
                            errors.push(LoweringError {
                                node,
                                message: "shared scope initialization cannot capture function-local values".to_owned(),
                            });
                        }
                    }
                }
                for (declaration, expression) in
                    crate::defaults::selected_defaults(self.resolved, node)
                {
                    let declaration_scope = self
                        .resolved
                        .scopes
                        .iter()
                        .find(|scope| scope.node == declaration)
                        .map(|scope| scope.id);
                    pending.push((expression, declaration_scope));
                }
                // A helper executes during startup too. Its parameters and own
                // locals are available, but surrounding function captures are
                // not part of shared storage and cannot be read at startup.
                if let Some(symbol) = self.called_symbol(node, None) {
                    let definition = &self.resolved.symbols[symbol.0 as usize];
                    if definition.kind == SymbolKind::Function && visited_helpers.insert(symbol) {
                        let scope = self
                            .resolved
                            .scopes
                            .iter()
                            .find(|scope| scope.node == definition.def_node)
                            .map(|scope| scope.id);
                        pending.push((
                            self.resolved.ast.fixed_children(definition.def_node)[2],
                            scope,
                        ));
                    }
                }
                if self.resolved.ast.node(node).kind == NodeKind::NamedArg {
                    pending.push((
                        resolution::argument_value_node(&self.resolved.ast, node),
                        helper_scope,
                    ));
                    continue;
                }
                pending.extend(
                    self.resolved
                        .ast
                        .fixed_children(node)
                        .iter()
                        .chain(self.resolved.ast.multi_children(node))
                        .map(|&child| (child, helper_scope)),
                );
            }
        }
        errors
    }

    fn collect_load_dependencies(&mut self) {
        for owner in 0..self.scopes.len() {
            let scope = &self.resolved.scopes[self.scopes[owner].0 as usize];
            for &symbol in scope.bindings.values() {
                if self.resolved.symbols[symbol.0 as usize].scope != scope.id {
                    for target in self.owners_of_symbol(symbol) {
                        if target != owner {
                            self.imports[owner].insert(target);
                        }
                    }
                }
            }
        }
        for (&node, &symbol) in &self.resolved.node_symbols {
            let Some(&owner) = self.node_owners.get(&node) else {
                continue;
            };
            // Declaration names do not load their own declared namespace.
            if matches!(
                self.resolved.symbols[symbol.0 as usize].kind,
                SymbolKind::Module | SymbolKind::Type
            ) {
                let definition = self.resolved.symbols[symbol.0 as usize].def_node;
                if !definition.is_null() && self.resolved.ast.fixed_children(definition)[0] == node
                {
                    continue;
                }
            }
            for target in self.owners_of_symbol(symbol) {
                if owner != target {
                    self.references[owner].insert(target);
                }
            }
        }
        for (&call, &symbol) in &self.resolved.constructor_types {
            if let Some(&owner) = self.node_owners.get(&call) {
                for target in self.owners_of_symbol(symbol) {
                    if owner != target {
                        self.references[owner].insert(target);
                    }
                }
            }
        }
        for (&node, &symbol) in self
            .resolved
            .concat_calls
            .iter()
            .chain(&self.resolved.instance_methods)
            .chain(&self.resolved.application_calls)
            .chain(&self.resolved.update_calls)
        {
            if let Some(&owner) = self.node_owners.get(&node) {
                for target in self.owners_of_symbol(symbol) {
                    if owner != target {
                        self.references[owner].insert(target);
                    }
                }
            }
        }
        // Generated Display bodies contain calls absent from the source AST.
        // Load each selected field implementation's namespace before startup can call it.
        for plan in &self.resolved.display_derivations {
            if plan.mode != resolution::DisplayDerivationMode::FieldCalls {
                continue;
            }
            let Some(&owner) = self.node_owners.get(&plan.node) else {
                continue;
            };
            let mut functions = HashSet::new();
            self.display_component_functions(plan.implementor, &mut HashSet::new(), &mut functions);
            for function in functions {
                for target in self.owners_of_symbol(function) {
                    if owner != target {
                        self.references[owner].insert(target);
                    }
                }
            }
        }
    }

    fn display_component_functions(
        &self,
        ty: TypeIndex,
        visited: &mut HashSet<TypeIndex>,
        functions: &mut HashSet<SymbolId>,
    ) {
        let Some(ty) = self.resolved.type_pool.canonical_type(ty) else {
            return;
        };
        if !visited.insert(ty) {
            return;
        }
        if let Some(method) = self.resolved.type_pool.find_trait_method(
            ty,
            self.resolved.type_pool.well_known.display,
            str_interner::intern("to_string"),
        ) {
            if method.func_id != type_pool::DERIVE_FUNC_ID {
                functions.insert(SymbolId(method.func_id));
                return;
            }
            if !self.resolved.display_derivations.iter().any(|plan| {
                plan.implementor == ty && plan.mode == resolution::DisplayDerivationMode::FieldCalls
            }) {
                return;
            }
        }
        match &self.resolved.type_pool.get(ty).kind {
            TypeKind::Optional { inner } => {
                self.display_component_functions(*inner, visited, functions)
            }
            TypeKind::Tuple { elements } => {
                for &element in elements {
                    self.display_component_functions(element, visited, functions);
                }
            }
            TypeKind::Enum { variants, .. } => {
                for field in variants.iter().flat_map(|variant| &variant.fields) {
                    self.display_component_functions(field.ty, visited, functions);
                }
            }
            _ => {}
        }
    }

    fn static_boolean(&self, node: NodeIndex, visited: &mut HashSet<SymbolId>) -> Option<bool> {
        if node.is_null() {
            return None;
        }
        let ast = &self.resolved.ast;
        let children = ast.fixed_children(node);
        match ast.node(node).kind {
            NodeKind::Bool => Some(str_interner::get(ast.node(node).str_id) == "true"),
            NodeKind::BoolNot => self
                .static_boolean(children[0], visited)
                .map(|value| !value),
            NodeKind::BoolAnd => {
                let left = self.static_boolean(children[0], visited)?;
                if left {
                    self.static_boolean(children[1], visited)
                } else {
                    Some(false)
                }
            }
            NodeKind::BoolOr => {
                let left = self.static_boolean(children[0], visited)?;
                if left {
                    Some(true)
                } else {
                    self.static_boolean(children[1], visited)
                }
            }
            NodeKind::Id | NodeKind::Projection => {
                let symbol = *self.resolved.node_symbols.get(&node)?;
                let definition = &self.resolved.symbols[symbol.0 as usize];
                if definition.kind != SymbolKind::Constant || !visited.insert(symbol) {
                    return None;
                }
                let result =
                    self.static_boolean(ast.fixed_children(definition.def_node)[2], visited);
                visited.remove(&symbol);
                result
            }
            _ => None,
        }
    }

    fn body_facts<'b>(
        &self,
        context: Option<DefaultScan<'b>>,
        node: NodeIndex,
    ) -> Option<&'b resolution::DefaultBodyFacts> {
        context
            .and_then(|context| context.body.body_facts.as_ref())
            .filter(|facts| facts.nodes.contains(&node))
    }

    fn selected_defaults(
        &self,
        node: NodeIndex,
        context: Option<DefaultScan<'_>>,
    ) -> Vec<(NodeIndex, NodeIndex)> {
        let facts = self.body_facts(context, node);
        let parameters = facts
            .map(|facts| &facts.call_arguments)
            .unwrap_or(&self.resolved.call_arguments)
            .get(&node)
            .into_iter()
            .flat_map(|plan| {
                plan.parameters
                    .iter()
                    .filter_map(|binding| match binding.value {
                        resolution::CallArgumentValue::Default { expression, .. } => {
                            Some((plan.declaration, expression))
                        }
                        _ => None,
                    })
            });
        let fields = facts
            .map(|facts| &facts.struct_constructions)
            .unwrap_or(&self.resolved.struct_constructions)
            .get(&node)
            .into_iter()
            .flat_map(|plan| {
                plan.fields
                    .iter()
                    .filter_map(|binding| match binding.value {
                        resolution::StructFieldValue::Default { expression } => {
                            Some((plan.declaration, expression))
                        }
                        _ => None,
                    })
            });
        parameters.chain(fields).collect()
    }

    /// Resolve interface declarations against this adapter's frozen root table.
    /// Fresh adapter symbols already identify an exact implementation.
    fn frozen_target(&self, symbol: SymbolId, context: Option<DefaultScan<'_>>) -> SymbolId {
        let Some(context) = context else {
            return symbol;
        };
        if self
            .resolved
            .default_methods
            .iter()
            .any(|plan| plan.function == symbol)
        {
            return symbol;
        }
        let Some(definition) = self.resolved.symbols.get(symbol.0 as usize) else {
            return symbol;
        };
        if !matches!(
            self.resolved.ast.node(definition.def_node).kind,
            NodeKind::TraitDefFn | NodeKind::TraitDeriveFn
        ) {
            return symbol;
        }
        let pool = &self.resolved.type_pool;
        let Some(owner) = self
            .resolved
            .scopes
            .get(definition.scope.0 as usize)
            .and_then(|scope| scope.assoc_type)
            .and_then(|ty| pool.canonical_type(ty))
        else {
            return symbol;
        };
        let plan = context.root;
        let mut pending = vec![plan.implementation_trait];
        let mut ancestors = HashSet::new();
        while let Some(ty) = pending.pop() {
            if !ancestors.insert(ty) {
                continue;
            }
            if let TypeKind::Trait { parents, .. } = &pool.get(ty).kind {
                pending.extend(
                    parents
                        .iter()
                        .filter_map(|parent| pool.canonical_type(*parent)),
                );
            }
        }
        if !ancestors.contains(&owner) {
            return symbol;
        }
        let Some(declared) = pool
            .trait_schema(owner)
            .and_then(|schema| schema.slots.iter().find(|key| key.name == definition.name))
        else {
            return symbol;
        };
        let Some(table) = pool.vtables_snapshot().iter().find(|table| {
            table.implementor == plan.implementor
                && table.trait_type == plan.implementation_trait
                && table.visible_scope == plan.visible_scope
        }) else {
            return symbol;
        };
        let Some(schema) = pool.trait_schema(table.trait_type) else {
            return symbol;
        };
        schema
            .slots
            .iter()
            .position(|key| {
                key.name == declared.name
                    && pool.check_trait_method_redeclaration(key, declared).is_ok()
            })
            .and_then(|slot| table.entries.get(slot))
            .map(|id| SymbolId(*id))
            .unwrap_or(symbol)
    }

    fn scan_initializer(
        &self,
        node: NodeIndex,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
    ) {
        self.scan_initializer_in(node, dependencies, visited, None);
    }

    fn scan_called_symbol(
        &self,
        symbol: SymbolId,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
    ) {
        self.scan_called_symbol_in(symbol, dependencies, visited, None);
    }

    /// Only checked constructors and explicit success lifts determine a branch.
    /// A payload's nominal type never determines whether it is Ok or Err.
    fn known_error_branch(
        &self,
        node: NodeIndex,
        context: Option<DefaultScan<'_>>,
    ) -> Option<resolution::ErrorPatternBranch> {
        let constructions = self
            .body_facts(context, node)
            .map(|facts| &facts.error_constructions)
            .unwrap_or(&self.resolved.error_constructions);
        let conversions = self
            .body_facts(context, node)
            .map(|facts| &facts.error_conversions)
            .unwrap_or(&self.resolved.error_conversions);
        if conversions.get(&node).is_some_and(|plans| {
            plans
                .iter()
                .any(|plan| plan.kind == resolution::ErrorConversionKind::LiftOk)
        }) {
            return Some(resolution::ErrorPatternBranch::Ok);
        }
        constructions
            .contains_key(&node)
            .then_some(resolution::ErrorPatternBranch::Error)
    }

    fn selected_error_arms(
        &self,
        node: NodeIndex,
        context: Option<DefaultScan<'_>>,
    ) -> Option<Vec<NodeIndex>> {
        let plans = self
            .body_facts(context, node)
            .map(|facts| &facts.error_eliminations)
            .unwrap_or(&self.resolved.error_eliminations);
        let plan = plans.get(&node)?;
        let branch = self.known_error_branch(plan.operand, context)?;
        Some(
            plan.arms
                .iter()
                .copied()
                .filter(|&arm| {
                    let pattern = self.resolved.ast.fixed_children(arm)[0];
                    let patterns = self
                        .body_facts(context, pattern)
                        .map(|facts| &facts.error_patterns)
                        .unwrap_or(&self.resolved.error_patterns);
                    patterns
                        .get(&pattern)
                        .is_none_or(|plan| plan.branch == branch)
                })
                .collect(),
        )
    }

    fn scan_initializer_in(
        &self,
        node: NodeIndex,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
        context: Option<DefaultScan<'_>>,
    ) {
        if node.is_null() {
            return;
        }
        let ast = &self.resolved.ast;
        // Constructing a closure does not execute its body or capture globals.
        if matches!(
            ast.node(node).kind,
            NodeKind::Lambda | NodeKind::FunctionDef | NodeKind::ModuleDef
        ) {
            return;
        }
        if let Some(&symbol) = self
            .body_facts(context, node)
            .map(|facts| &facts.node_symbols)
            .unwrap_or(&self.resolved.node_symbols)
            .get(&node)
            && self.slots.contains_key(&symbol)
            && let Some(owner) = self.owner_of_symbol(symbol)
        {
            dependencies.insert(owner);
        }
        if let Some(plan) = self
            .body_facts(context, node)
            .map(|facts| &facts.for_loops)
            .unwrap_or(&self.resolved.for_loops)
            .get(&node)
        {
            // Loop calls are implicit in the AST. Their exact source targets
            // were frozen by resolution, including each default adapter's facts.
            for call in plan.into_iter.iter().chain(std::iter::once(&plan.next)) {
                let function = call
                    .function
                    .expect("resolution froze the loop target before lowering");
                self.scan_called_symbol_in(function, dependencies, visited, None);
            }
        }
        match ast.node(node).kind {
            NodeKind::ErrorElimination => {
                if let Some(arms) = self.selected_error_arms(node, context) {
                    self.scan_initializer_in(
                        ast.fixed_children(node)[0],
                        dependencies,
                        visited,
                        context,
                    );
                    for arm in arms {
                        self.scan_initializer_in(arm, dependencies, visited, context);
                    }
                    return;
                }
            }
            NodeKind::BoolAnd | NodeKind::BoolOr => {
                let children = ast.fixed_children(node);
                self.scan_initializer_in(children[0], dependencies, visited, context);
                let left = self.static_boolean(children[0], &mut HashSet::new());
                if matches!(
                    (ast.node(node).kind, left),
                    (NodeKind::BoolAnd, Some(false)) | (NodeKind::BoolOr, Some(true))
                ) {
                    return;
                }
                self.scan_initializer_in(children[1], dependencies, visited, context);
                return;
            }
            NodeKind::IfStatement => {
                let children = ast.fixed_children(node);
                self.scan_initializer_in(children[0], dependencies, visited, context);
                if let Some(condition) = self.static_boolean(children[0], &mut HashSet::new()) {
                    self.scan_initializer_in(
                        children[if condition { 1 } else { 2 }],
                        dependencies,
                        visited,
                        context,
                    );
                    return;
                }
            }
            NodeKind::WhileLoop => {
                let children = ast.fixed_children(node);
                self.scan_initializer_in(children[1], dependencies, visited, context);
                if self.static_boolean(children[1], &mut HashSet::new()) == Some(false) {
                    return;
                }
            }
            _ => {}
        }
        for (_, expression) in self.selected_defaults(node, context) {
            self.scan_initializer_in(expression, dependencies, visited, context);
        }
        if matches!(ast.node(node).kind, NodeKind::Call | NodeKind::ExtendedCall) {
            self.scan_callback_call(node, dependencies, visited, context);
        }
        if let Some(symbol) = self.called_symbol(node, context) {
            if !self.scan_independent_self_call(node, symbol, dependencies, visited, context) {
                self.scan_called_symbol_in(symbol, dependencies, visited, context);
            }
        } else if matches!(ast.node(node).kind, NodeKind::Call | NodeKind::ExtendedCall) {
            let callee = ast.fixed_children(node)[0];
            if ast.node(callee).kind == NodeKind::Lambda {
                self.scan_initializer_in(
                    ast.fixed_children(callee)[0],
                    dependencies,
                    visited,
                    context,
                );
            }
        }
        for &child in ast
            .fixed_children(node)
            .iter()
            .chain(ast.multi_children(node))
        {
            self.scan_initializer_in(child, dependencies, visited, context);
        }
    }

    /// A distinct Self argument can carry a different frozen implementation.
    /// Without a statically tracked argument proof, include compatible tables
    /// conservatively instead of assigning it the current receiver's proof.
    fn scan_independent_self_call(
        &self,
        node: NodeIndex,
        symbol: SymbolId,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
        context: Option<DefaultScan<'_>>,
    ) -> bool {
        let Some(context) = context else {
            return false;
        };
        let ast = &self.resolved.ast;
        if !matches!(ast.node(node).kind, NodeKind::Call | NodeKind::ExtendedCall) {
            return false;
        }
        let callee = ast.fixed_children(node)[0];
        if ast.node(callee).kind != NodeKind::Projection {
            return false;
        }
        let receiver = ast.fixed_children(callee)[0];
        let receiver_symbol = self
            .body_facts(Some(context), receiver)
            .map(|facts| &facts.node_symbols)
            .unwrap_or(&self.resolved.node_symbols)
            .get(&receiver);
        if receiver_symbol.is_some_and(|symbol| {
            self.receiver_aliases
                .get(&context.body.function)
                .is_some_and(|aliases| aliases.contains(symbol))
        }) {
            return false;
        }
        let Some(definition) = self.resolved.symbols.get(symbol.0 as usize) else {
            return false;
        };
        if !matches!(
            ast.node(definition.def_node).kind,
            NodeKind::TraitDefFn | NodeKind::TraitDeriveFn
        ) {
            return false;
        }
        let pool = &self.resolved.type_pool;
        let Some(owner) = self
            .resolved
            .scopes
            .get(definition.scope.0 as usize)
            .and_then(|scope| scope.assoc_type)
            .and_then(|ty| pool.canonical_type(ty))
        else {
            return false;
        };
        let Some(key) = pool
            .trait_schema(owner)
            .and_then(|schema| schema.slots.iter().find(|key| key.name == definition.name))
        else {
            return false;
        };
        for table in pool
            .vtables_snapshot()
            .iter()
            .filter(|table| table.implementor == context.root.implementor)
        {
            // Presence of this declaration's owner in the schema proves the
            // relationship; an unrelated same-named method is never a target.
            let Some(schema) = pool.trait_schema(table.trait_type) else {
                continue;
            };
            if !schema
                .slots
                .iter()
                .any(|slot| slot.trait_owner == key.trait_owner)
            {
                continue;
            }
            if let Some(slot) = schema.slots.iter().position(|slot| {
                slot.name == key.name && pool.check_trait_method_redeclaration(slot, key).is_ok()
            }) && let Some(&target) = table.entries.get(slot)
            {
                self.scan_called_symbol_in(SymbolId(target), dependencies, visited, None);
            }
        }
        true
    }

    fn called_symbol(&self, node: NodeIndex, context: Option<DefaultScan<'_>>) -> Option<SymbolId> {
        let facts = self.body_facts(context, node);
        if let Some(&symbol) = facts
            .map(|facts| &facts.concat_calls)
            .unwrap_or(&self.resolved.concat_calls)
            .get(&node)
            .or_else(|| {
                facts
                    .map(|facts| &facts.application_calls)
                    .unwrap_or(&self.resolved.application_calls)
                    .get(&node)
            })
            .or_else(|| {
                facts
                    .map(|facts| &facts.update_calls)
                    .unwrap_or(&self.resolved.update_calls)
                    .get(&node)
            })
        {
            return Some(symbol);
        }
        if !matches!(
            self.resolved.ast.node(node).kind,
            NodeKind::Call | NodeKind::ExtendedCall
        ) {
            return None;
        }
        let callee = self.resolved.ast.fixed_children(node)[0];
        let facts = self.body_facts(context, callee);
        facts
            .map(|facts| &facts.instance_methods)
            .unwrap_or(&self.resolved.instance_methods)
            .get(&callee)
            .or_else(|| {
                facts
                    .map(|facts| &facts.node_symbols)
                    .unwrap_or(&self.resolved.node_symbols)
                    .get(&callee)
            })
            .copied()
    }

    fn scan_called_symbol_in(
        &self,
        symbol: SymbolId,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
        context: Option<DefaultScan<'_>>,
    ) {
        let original = symbol;
        let symbol = self.frozen_target(symbol, context);
        let default = self
            .resolved
            .default_methods
            .iter()
            .find(|plan| plan.function == symbol);
        let context = default
            .map(|body| DefaultScan {
                body,
                root: context
                    .filter(|_| original != symbol)
                    .map(|context| context.root)
                    .unwrap_or(body),
            })
            .or_else(|| {
                context.filter(|context| {
                    context.body.body_facts.as_ref().is_none_or(|facts| {
                        self.resolved
                            .symbols
                            .get(symbol.0 as usize)
                            .is_some_and(|definition| facts.nodes.contains(&definition.def_node))
                    })
                })
            });
        if !visited.insert((symbol, context.map(|context| context.root.function))) {
            return;
        }
        let Some(definition) = self.resolved.symbols.get(symbol.0 as usize) else {
            return;
        };
        let ast = &self.resolved.ast;
        if definition.kind == SymbolKind::Function {
            if let Some(owner) = self.owner_of_symbol(symbol) {
                dependencies.insert(owner);
            }
            self.scan_initializer_in(
                ast.fixed_children(definition.def_node)[2],
                dependencies,
                visited,
                context,
            );
        } else if definition.kind == SymbolKind::Type {
            if let Some(owner) = self.owner_of_symbol(symbol) {
                dependencies.insert(owner);
                let scope = &self.resolved.scopes[self.scopes[owner].0 as usize];
                if let Some(&constructor) = scope.bindings.get(&str_interner::intern("new")) {
                    self.scan_called_symbol_in(constructor, dependencies, visited, context);
                }
            }
        } else if self.slots.contains_key(&symbol) {
            let value = ast.fixed_children(definition.def_node)[2];
            match ast.node(value).kind {
                NodeKind::Lambda => self.scan_initializer_in(
                    ast.fixed_children(value)[0],
                    dependencies,
                    visited,
                    context,
                ),
                NodeKind::Id | NodeKind::Projection => {
                    if let Some(&target) = self.resolved.node_symbols.get(&value) {
                        self.scan_called_symbol_in(target, dependencies, visited, context);
                    }
                }
                _ => {}
            }
        }
    }

    fn build(mut self) -> Result<InitializationPlan, Vec<LoweringError>> {
        self.mark_owners(self.resolved.ast.root, None);
        self.collect_storage();
        let errors = self.validate_shared_initializers();
        if !errors.is_empty() {
            return Err(errors);
        }
        self.collect_load_dependencies();
        let Some(&root) = self.scope_nodes.get(&self.resolved.ast.root) else {
            return Ok(InitializationPlan {
                globals: self.globals,
                slots: self.slots,
                ordered: Vec::new(),
            });
        };
        let mut loaded = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(owner) = pending.pop() {
            if !loaded.insert(owner) {
                continue;
            }
            let mut dependencies = BTreeSet::new();
            let mut visited = HashSet::new();
            for &statement in &self.statements[owner] {
                self.scan_initializer(statement, &mut dependencies, &mut visited);
            }
            if let Some(hook) = self.hooks[owner] {
                self.scan_called_symbol(hook, &mut dependencies, &mut visited);
            }
            dependencies.remove(&owner);
            pending.extend(
                self.imports[owner]
                    .iter()
                    .chain(&self.references[owner])
                    .chain(&dependencies)
                    .copied(),
            );
            self.prerequisites[owner] = dependencies;
        }
        // Preserve dependencies through modules without runtime work. A lexical
        // reference to the containing file does not inherit that file's main
        // dependencies; actual initializer reads of its globals are hard edges.
        let mut preferred = vec![BTreeSet::new(); self.scopes.len()];
        for &owner in &loaded {
            let mut pending: Vec<_> = self.imports[owner]
                .iter()
                .chain(&self.references[owner])
                .copied()
                .collect();
            while let Some(target) = pending.pop() {
                if target == owner || !preferred[owner].insert(target) {
                    continue;
                }
                let scope_node = self.resolved.scopes[self.scopes[target].0 as usize].node;
                if self.resolved.ast.node(scope_node).kind != NodeKind::FileScope {
                    pending.extend(
                        self.imports[target]
                            .iter()
                            .chain(&self.references[target])
                            .copied(),
                    );
                }
            }
        }
        self.imports = preferred;
        // Empty scopes have no executable initialization to order. References
        // inside called helpers have already propagated their actual reads.
        let work: BTreeSet<_> = loaded
            .iter()
            .copied()
            .filter(|&owner| !self.statements[owner].is_empty() || self.hooks[owner].is_some())
            .collect();
        let mut remaining = work;
        let mut order = Vec::new();
        while !remaining.is_empty() {
            let ready: Vec<_> = remaining
                .iter()
                .copied()
                .filter(|&owner| {
                    !self.prerequisites[owner]
                        .iter()
                        .any(|target| remaining.contains(target))
                })
                .collect();
            if ready.is_empty() {
                return Err(remaining
                    .iter()
                    .map(|&owner| LoweringError {
                        node: self.resolved.scopes[self.scopes[owner].0 as usize].node,
                        message: "cyclic module value initialization dependency".to_owned(),
                    })
                    .collect());
            }
            // Prefer import order among otherwise ready scopes. Soft import
            // cycles choose a stable order; checked reads never use defaults.
            let next = ready
                .iter()
                .copied()
                .find(|&owner| {
                    !self.imports[owner]
                        .iter()
                        .any(|target| remaining.contains(target))
                })
                .unwrap_or(ready[0]);
            remaining.remove(&next);
            order.push(next);
        }
        let ordered = order
            .into_iter()
            .map(|owner| ScopeInitialization {
                node: self.resolved.scopes[self.scopes[owner].0 as usize].node,
                statements: std::mem::take(&mut self.statements[owner]),
                hook: self.hooks[owner],
            })
            .collect();
        Ok(InitializationPlan {
            globals: self.globals,
            slots: self.slots,
            ordered,
        })
    }
}

/// A local copied from the receiver retains its proof. Restrict this analysis
/// to simple bindings with no writes anywhere in the template (including
/// closures); an independently supplied Self value must keep its own proof.
fn receiver_aliases(
    resolved: &ResolvedAst,
    plan: &resolution::DefaultMethodPlan,
) -> HashSet<SymbolId> {
    let ast = &resolved.ast;
    let symbols = plan
        .body_facts
        .as_ref()
        .map(|facts| &facts.node_symbols)
        .unwrap_or(&resolved.node_symbols);
    let mut pending = vec![resolved.symbols[plan.declaration.0 as usize].def_node];
    let mut bindings = Vec::new();
    let mut receivers = HashSet::new();
    let mut writes = HashSet::new();
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        let children = ast.fixed_children(node);
        match ast.node(node).kind {
            NodeKind::SelfLower => {
                if let Some(&symbol) = symbols.get(&node) {
                    receivers.insert(symbol);
                }
            }
            NodeKind::LetDecl | NodeKind::ConstDecl
                if ast.node(children[0]).kind == NodeKind::Id =>
            {
                if let Some(&symbol) = symbols.get(&children[0]) {
                    bindings.push((symbol, children[2]));
                }
            }
            NodeKind::Assign
            | NodeKind::AddAssign
            | NodeKind::SubAssign
            | NodeKind::MulAssign
            | NodeKind::DivAssign
            | NodeKind::ModAssign => {
                if let Some(&symbol) = symbols.get(&children[0]) {
                    writes.insert(symbol);
                }
            }
            _ => {}
        }
        pending.extend(children.iter().chain(ast.multi_children(node)).copied());
    }
    let mut copies: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
    for (symbol, value) in bindings {
        if writes.contains(&symbol) || value.is_null() {
            continue;
        }
        if matches!(ast.node(value).kind, NodeKind::SelfLower | NodeKind::Id)
            && let Some(&source) = symbols.get(&value)
        {
            copies.entry(source).or_default().push(symbol);
        }
    }
    let mut pending: Vec<_> = receivers
        .into_iter()
        .filter(|receiver| !writes.contains(receiver))
        .collect();
    let mut aliases = HashSet::new();
    while let Some(symbol) = pending.pop() {
        if aliases.insert(symbol)
            && let Some(dependents) = copies.get(&symbol)
        {
            pending.extend(dependents.iter().copied());
        }
    }
    aliases
}
