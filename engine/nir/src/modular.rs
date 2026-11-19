//! Per-package & per-`mod` modular NIR lowering, external symbol compaction,
//! and `TypePool` dispatch table pruning.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ast::{NodeIndex, NodeKind};
use nsbc::{FuncId, GlobalId};
use resolution::{ResolvedAst, ScopeId, SymbolId, SymbolKind};
use str_interner::StrId;
use type_pool::{TypeIndex, TypeKind, TypePool};

use crate::builder::FunctionBuilder;
use crate::initialization::{InitializationPlan, ScopeInitialization};
use crate::lowering::NirLowering;
use crate::opt::OptLevel;
use crate::{DerivedMethod, LoweringError, NirExpr, NirModule, NirStmt, NirValue, StartupMode};

const EXT_FN_BASE: u32 = 1 << 24;
const EXT_GLOBAL_BASE: u32 = 1 << 24;

/// Origin of an imported function referenced by local NIR or retained `VTable`s.
#[derive(Debug, Clone)]
pub enum ExternalFunctionOrigin {
    Symbol(SymbolId),
    DefaultMethod {
        implementor: TypeIndex,
        trait_type: TypeIndex,
        method_name: StrId,
        signature: TypeIndex,
        self_parameters: Vec<usize>,
    },
    DefaultHelper {
        implementor: TypeIndex,
        trait_type: TypeIndex,
        method_name: StrId,
        nested_symbol: SymbolId,
        signature: TypeIndex,
        self_parameters: Vec<usize>,
    },
    DerivedMethod {
        implementor: TypeIndex,
        trait_type: TypeIndex,
        method_name: StrId,
        signature: TypeIndex,
        is_display: bool,
    },
    DerivedJsonFrom {
        implementor: TypeIndex,
        signature: TypeIndex,
    },
    DerivedJsonHelper {
        implementor: TypeIndex,
        is_write: bool,
        signature: TypeIndex,
    },
}

/// Compacted external function reference (`FuncId(local_fn_count + index)`).
#[derive(Debug, Clone)]
pub struct ModularImportedFunction {
    pub func_id: FuncId,
    pub package_root: NodeIndex,
    pub module_path: Vec<String>,
    pub origin: ExternalFunctionOrigin,
}

/// Compacted external global/constant reference (`GlobalId(local_global_count + index)`).
#[derive(Debug, Clone)]
pub struct ModularImportedGlobal {
    pub global_id: GlobalId,
    pub package_root: NodeIndex,
    pub module_path: Vec<String>,
    pub symbol: SymbolId,
    pub type_index: TypeIndex,
    pub is_mutable: bool,
}

/// Origin of a locally defined runtime symbol exported in `SectionKind::Exports`.
#[derive(Debug, Clone)]
pub enum LocalExportOrigin {
    FunctionSymbol {
        symbol: SymbolId,
        func_id: FuncId,
    },
    DefaultMethod {
        implementor: TypeIndex,
        trait_type: TypeIndex,
        method_name: StrId,
        func_id: FuncId,
    },
    DefaultHelper {
        implementor: TypeIndex,
        trait_type: TypeIndex,
        method_name: StrId,
        nested_symbol: SymbolId,
        func_id: FuncId,
    },
    DerivedMethod {
        implementor: TypeIndex,
        trait_type: TypeIndex,
        method_name: StrId,
        func_id: FuncId,
    },
    DerivedJsonFrom {
        implementor: TypeIndex,
        func_id: FuncId,
    },
    DerivedJsonHelper {
        implementor: TypeIndex,
        is_write: bool,
        func_id: FuncId,
    },
    Global {
        symbol: SymbolId,
        global_id: GlobalId,
        type_index: TypeIndex,
        is_mutable: bool,
    },
    Type {
        symbol: SymbolId,
        type_index: TypeIndex,
    },
}

/// Exportable symbol entry paired with its owning local module index.
#[derive(Debug, Clone)]
pub struct ModularLocalExport {
    pub owner_module_index: u32,
    pub origin: LocalExportOrigin,
}

/// Per-`mod` lowering unit inside the compiled target package.
#[derive(Debug, Clone)]
pub struct ModularScopeUnit {
    pub module_path: Vec<String>,
    pub scope_node: NodeIndex,
    pub scope_id: ScopeId,
    pub module_type_index: Option<TypeIndex>,
    pub owned_functions: Vec<FuncId>,
    pub owned_globals: Vec<GlobalId>,
    pub scope_initializers: Vec<FuncId>,
    pub local_module_deps: Vec<u32>,
    pub local_init_prereqs: Vec<u32>,
    pub external_module_deps: Vec<(NodeIndex, Vec<String>)>,
    pub external_init_prereqs: Vec<(NodeIndex, Vec<String>)>,
    pub main_func: Option<FuncId>,
}

/// Complete modular NIR output prior to bytecode generation.
#[derive(Debug)]
pub struct ModularNirOutput {
    pub nir_module: NirModule,
    pub modules: Vec<ModularScopeUnit>,
    pub imported_functions: Vec<ModularImportedFunction>,
    pub imported_globals: Vec<ModularImportedGlobal>,
    pub local_exports: Vec<ModularLocalExport>,
    all_derived_methods: Vec<DerivedMethod>,
    local_nominal_types: HashSet<TypeIndex>,
    local_impl_pairs: HashSet<(TypeIndex, TypeIndex)>,
    local_scope_ids: HashSet<u32>,
}

fn target_root_main(resolved: &ResolvedAst, target_root: NodeIndex) -> Option<SymbolId> {
    let scope = resolved.scopes.iter().find(|s| s.node == target_root)?;
    scope
        .bindings
        .get(&str_interner::intern("main"))
        .copied()
        .filter(|&id| {
            let symbol = &resolved.symbols[id.0 as usize];
            symbol.kind == SymbolKind::Function && symbol.scope == scope.id
        })
}

#[derive(Clone)]
enum ScopeOwner {
    Local(usize),
    External(NodeIndex, Vec<String>),
}

fn resolve_scope_owner(
    resolved: &ResolvedAst,
    scope_id: ScopeId,
    local_mod_by_scope: &HashMap<ScopeId, usize>,
) -> Option<ScopeOwner> {
    let mut segments = Vec::new();
    let mut cur = Some(scope_id);
    while let Some(sid) = cur {
        let sc = &resolved.scopes[sid.0 as usize];
        if !sc.node.is_null() && resolved.external_package_roots.contains(&sc.node) {
            segments.reverse();
            return Some(ScopeOwner::External(sc.node, segments));
        }
        if let Some(&midx) = local_mod_by_scope.get(&sid) {
            return Some(ScopeOwner::Local(midx));
        }
        if !sc.node.is_null() && resolved.ast.node(sc.node).kind == NodeKind::ModuleDef {
            let name_node = resolved.ast.fixed_children(sc.node)[0];
            if !name_node.is_null() {
                segments.push(str_interner::get(resolved.ast.node(name_node).str_id));
            }
        }
        cur = sc.parent;
    }
    None
}

fn mark_ast_scope_map(
    resolved: &ResolvedAst,
    node: NodeIndex,
    inherited: Option<ScopeId>,
    scope_by_node: &HashMap<NodeIndex, ScopeId>,
    out: &mut HashMap<NodeIndex, ScopeId>,
) {
    if node.is_null() {
        return;
    }
    let cur = scope_by_node
        .get(&node)
        .copied()
        .or_else(|| resolved.node_scopes.get(&node).copied())
        .or(inherited);
    if let Some(sid) = cur {
        out.insert(node, sid);
    }
    for &c in resolved
        .ast
        .fixed_children(node)
        .iter()
        .chain(resolved.ast.multi_children(node))
    {
        mark_ast_scope_map(resolved, c, cur, scope_by_node, out);
    }
}

struct FunctionPartitionCollector<'a> {
    resolved: &'a ResolvedAst,
    ast_scopes: &'a HashMap<NodeIndex, ScopeId>,
    local_mod_by_scope: &'a HashMap<ScopeId, usize>,
    next_local_fid: &'a mut u32,
    next_ext_fid: &'a mut u32,
    func_map: &'a mut HashMap<SymbolId, FuncId>,
    local_pending: &'a mut Vec<(usize, NodeIndex, FuncId, Option<SymbolId>)>,
    ext_functions: &'a mut Vec<ModularImportedFunction>,
}

impl FunctionPartitionCollector<'_> {
    fn visit(&mut self, node_idx: NodeIndex) {
        if node_idx.is_null() {
            return;
        }
        let ast = &self.resolved.ast;
        match ast.node(node_idx).kind {
            NodeKind::TraitDeriveFn => {}
            NodeKind::FunctionDef => {
                if self.resolved.iterator_template_nodes.contains(&node_idx) {
                    return;
                }
                let name_node = ast.fixed_children(node_idx)[0];
                let sym_opt = (!name_node.is_null())
                    .then(|| self.resolved.node_symbols.get(&name_node).copied())
                    .flatten();

                let owner = sym_opt
                    .map(|s| self.resolved.symbols[s.0 as usize].scope)
                    .or_else(|| self.ast_scopes.get(&node_idx).copied())
                    .and_then(|sid| {
                        resolve_scope_owner(self.resolved, sid, self.local_mod_by_scope)
                    });

                match owner {
                    Some(ScopeOwner::Local(midx)) => {
                        let fid = FuncId(*self.next_local_fid);
                        *self.next_local_fid += 1;
                        if let Some(sym_id) = sym_opt {
                            self.func_map.insert(sym_id, fid);
                        }
                        self.local_pending.push((midx, node_idx, fid, sym_opt));
                        for &child in ast
                            .fixed_children(node_idx)
                            .iter()
                            .chain(ast.multi_children(node_idx))
                        {
                            self.visit(child);
                        }
                    }
                    Some(ScopeOwner::External(pkg_root, module_path)) => {
                        if let Some(sym_id) = sym_opt {
                            let fid = FuncId(*self.next_ext_fid);
                            *self.next_ext_fid += 1;
                            self.func_map.insert(sym_id, fid);
                            self.ext_functions.push(ModularImportedFunction {
                                func_id: fid,
                                package_root: pkg_root,
                                module_path,
                                origin: ExternalFunctionOrigin::Symbol(sym_id),
                            });
                        }
                    }
                    None => {}
                }
            }
            _ => {
                for &child in ast
                    .fixed_children(node_idx)
                    .iter()
                    .chain(ast.multi_children(node_idx))
                {
                    self.visit(child);
                }
            }
        }
    }
}

fn collect_all_impl_owners(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    ast_scopes: &HashMap<NodeIndex, ScopeId>,
    local_mod_by_scope: &HashMap<ScopeId, usize>,
    impl_owners: &mut HashMap<(TypeIndex, TypeIndex), ScopeOwner>,
) {
    if node_idx.is_null() {
        return;
    }
    let ast = &resolved.ast;
    let denotes = |n: NodeIndex| -> Option<TypeIndex> {
        if n.is_null() {
            return None;
        }
        resolved
            .node_symbols
            .get(&n)
            .map(|&s| resolved.symbols[s.0 as usize].type_index)
            .or_else(|| resolved.node_types.get(&n).copied())
            .and_then(|ty| resolved.type_pool.canonical_type(ty))
    };
    let owner_opt = ast_scopes
        .get(&node_idx)
        .copied()
        .and_then(|sid| resolve_scope_owner(resolved, sid, local_mod_by_scope));
    if let Some(owner) = owner_opt {
        let mut register_pair = |imp: TypeIndex, tr: TypeIndex| {
            impl_owners
                .entry((imp, tr))
                .or_insert_with(|| owner.clone());
            for anc in trait_ancestors(&resolved.type_pool, tr) {
                impl_owners
                    .entry((imp, anc))
                    .or_insert_with(|| owner.clone());
            }
        };
        match ast.node(node_idx).kind {
            NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef => {
                let children = ast.fixed_children(node_idx);
                if let (Some(tr), Some(imp)) = (denotes(children[0]), denotes(children[1])) {
                    register_pair(imp, tr);
                }
            }
            NodeKind::DeriveDef => {
                if let Some(imp) = denotes(ast.fixed_children(node_idx)[0]) {
                    for &tr_node in ast.multi_children(node_idx) {
                        if let Some(tr) = denotes(tr_node) {
                            register_pair(imp, tr);
                            let wk = resolved.type_pool.well_known;
                            if tr == wk.eq {
                                register_pair(imp, wk.partial_eq);
                            } else if tr == wk.ord {
                                register_pair(imp, wk.partial_ord);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for &c in ast
        .fixed_children(node_idx)
        .iter()
        .chain(ast.multi_children(node_idx))
    {
        collect_all_impl_owners(resolved, c, ast_scopes, local_mod_by_scope, impl_owners);
    }
}

fn collect_local_impl_pairs(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    local_pairs: &mut HashSet<(TypeIndex, TypeIndex)>,
) {
    if node_idx.is_null() || resolved.external_package_roots.contains(&node_idx) {
        return;
    }
    let ast = &resolved.ast;
    let denotes = |n: NodeIndex| -> Option<TypeIndex> {
        if n.is_null() {
            return None;
        }
        resolved
            .node_symbols
            .get(&n)
            .map(|&s| resolved.symbols[s.0 as usize].type_index)
            .or_else(|| resolved.node_types.get(&n).copied())
            .and_then(|ty| resolved.type_pool.canonical_type(ty))
    };
    match ast.node(node_idx).kind {
        NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef => {
            let children = ast.fixed_children(node_idx);
            if let (Some(tr), Some(imp)) = (denotes(children[0]), denotes(children[1])) {
                local_pairs.insert((imp, tr));
            }
        }
        NodeKind::DeriveDef => {
            let imp_opt = denotes(ast.fixed_children(node_idx)[0]);
            if let Some(imp) = imp_opt {
                for &tr_node in ast.multi_children(node_idx) {
                    if let Some(tr) = denotes(tr_node) {
                        local_pairs.insert((imp, tr));
                        let wk = resolved.type_pool.well_known;
                        if tr == wk.eq {
                            local_pairs.insert((imp, wk.partial_eq));
                        } else if tr == wk.ord {
                            local_pairs.insert((imp, wk.partial_ord));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    for &c in ast
        .fixed_children(node_idx)
        .iter()
        .chain(ast.multi_children(node_idx))
    {
        collect_local_impl_pairs(resolved, c, local_pairs);
    }
}

fn lower_scope_initializers_for_module(
    resolved: &ResolvedAst,
    lowering: &mut NirLowering,
    initializers: Vec<ScopeInitialization>,
    owned_functions: &mut Vec<FuncId>,
) -> Vec<FuncId> {
    let mut init_fids = Vec::with_capacity(initializers.len());
    for initializer in initializers {
        let function = FuncId(lowering.next_func_id);
        lowering.next_func_id += 1;
        let mut builder = FunctionBuilder::new(
            function,
            str_interner::intern(&format!("__module_init_{}", initializer.node.0)),
        );
        builder.func_map = lowering.func_map.clone();
        builder.derived_functions = lowering.derived_functions.clone();
        builder.global_map = lowering.global_map.clone();
        builder.next_func_id = lowering.next_func_id;
        builder.return_type = type_pool::Intrinsic::Unit.type_index();
        let mut block = builder.new_block();
        builder.entry_block = block;
        for statement in initializer.statements {
            crate::stmt::lower_stmt(resolved, statement, &mut builder, &mut block, false);
        }
        if let Some(hook) = initializer.hook {
            let result = builder.alloc_local();
            builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                result,
                NirExpr::Call(lowering.func_map[&hook], Vec::new()),
            ));
        }
        crate::stmt::emit_return(resolved, NirValue::Unit, &mut builder, &mut block);
        lowering.next_func_id = builder.next_func_id;
        for lf in &builder.lambda_functions {
            owned_functions.push(lf.func_id);
        }
        lowering.functions.append(&mut builder.lambda_functions);
        owned_functions.push(function);
        lowering.functions.push(builder.build());
        init_fids.push(function);
    }
    init_fids
}

/// Lower a resolved AST into per-`mod` [`ModularNirOutput`] for `target_root`.
pub fn try_lower_modular(
    resolved: &ResolvedAst,
    target_root: NodeIndex,
    startup: StartupMode,
) -> Result<ModularNirOutput, Vec<LoweringError>> {
    if startup == StartupMode::RequiredMain {
        let main = target_root_main(resolved, target_root).ok_or_else(|| {
            vec![LoweringError {
                node: target_root,
                message: "exe package must define a root main function".into(),
                code: Some(5001),
                note_node: None,
                help: Some("define `fn main() { ... }` at the root of `main.ns`".into()),
            }]
        })?;
        let node = resolved.symbols[main.0 as usize].def_node;
        if !resolved.ast.multi_children(node).is_empty() {
            return Err(vec![LoweringError {
                node,
                message: "exe main must have zero parameters".into(),
                code: Some(5001),
                note_node: None,
                help: Some(
                    "remove parameters from `fn main()` and read CLI arguments via `std.env.args()`"
                        .into(),
                ),
            }]);
        }
    }

    let init_plan = InitializationPlan::build_modular(resolved, target_root, EXT_GLOBAL_BASE)?;
    crate::named_functions::validate(resolved, &init_plan.slots)?;

    let local_mod_by_scope: HashMap<ScopeId, usize> = init_plan
        .modules
        .iter()
        .enumerate()
        .map(|(idx, m)| (m.scope_id, idx))
        .collect();

    let target_pkg_id = resolved
        .scopes
        .iter()
        .find(|s| s.node == target_root)
        .and_then(|s| resolved.type_pool.scope_context(s.id.0))
        .map(|c| c.package);
    let local_scope_ids: HashSet<u32> = resolved
        .scopes
        .iter()
        .filter(|s| resolved.type_pool.scope_context(s.id.0).map(|c| c.package) == target_pkg_id)
        .map(|s| s.id.0)
        .collect();

    let scope_by_node: HashMap<NodeIndex, ScopeId> = resolved
        .scopes
        .iter()
        .filter(|s| !s.node.is_null())
        .map(|s| (s.node, s.id))
        .collect();
    let mut ast_scopes = HashMap::new();
    mark_ast_scope_map(
        resolved,
        resolved.ast.root,
        None,
        &scope_by_node,
        &mut ast_scopes,
    );

    let mut lowering = NirLowering::new();
    lowering.global_map = init_plan.slots;
    let mut next_ext_fid = EXT_FN_BASE;
    let mut local_pending = Vec::new();
    let mut ext_functions = Vec::new();

    FunctionPartitionCollector {
        resolved,
        ast_scopes: &ast_scopes,
        local_mod_by_scope: &local_mod_by_scope,
        next_local_fid: &mut lowering.next_func_id,
        next_ext_fid: &mut next_ext_fid,
        func_map: &mut lowering.func_map,
        local_pending: &mut local_pending,
        ext_functions: &mut ext_functions,
    }
    .visit(resolved.ast.root);

    let mut impl_owners = HashMap::new();
    collect_all_impl_owners(
        resolved,
        resolved.ast.root,
        &ast_scopes,
        &local_mod_by_scope,
        &mut impl_owners,
    );

    let mut local_default_plans: Vec<(usize, &resolution::DefaultMethodPlan)> = Vec::new();
    let wk = resolved.type_pool.well_known;
    let next_str = str_interner::intern("next");
    let into_iter_str = str_interner::intern("into_iter");
    for plan in &resolved.default_methods {
        let tr_c = resolved
            .type_pool
            .canonical_type(plan.trait_type)
            .unwrap_or(plan.trait_type);
        let is_on_demand_iter_adapter = (tr_c == wk.iterator && plan.method_name != next_str)
            || (tr_c == wk.into_iterator && plan.method_name != into_iter_str);
        let owner = if is_on_demand_iter_adapter {
            Some(ScopeOwner::Local(0))
        } else if let Some(vis_sc) = plan.visible_scope {
            resolve_scope_owner(resolved, ScopeId(vis_sc), &local_mod_by_scope)
        } else {
            let imp_c = resolved
                .type_pool
                .canonical_type(plan.implementor)
                .unwrap_or(plan.implementor);
            let impl_tr_c = resolved
                .type_pool
                .canonical_type(plan.implementation_trait)
                .unwrap_or(plan.implementation_trait);
            impl_owners
                .get(&(imp_c, impl_tr_c))
                .or_else(|| impl_owners.get(&(imp_c, tr_c)))
                .cloned()
                .or_else(|| {
                    resolve_scope_owner(resolved, ScopeId(plan.provider_scope), &local_mod_by_scope)
                })
        };
        match owner {
            Some(ScopeOwner::Local(midx)) => {
                let fid = FuncId(lowering.next_func_id);
                lowering.next_func_id += 1;
                lowering.func_map.insert(plan.function, fid);
                let mut nested = HashMap::new();
                let mut nodes: Vec<_> = plan.self_function_types.keys().copied().collect();
                nodes.sort_by_key(|n| n.0);
                for node in nodes {
                    if resolved.ast.node(node).kind == NodeKind::FunctionDef
                        && node != resolved.symbols[plan.declaration.0 as usize].def_node
                        && let Some(&sym) = resolved
                            .node_symbols
                            .get(&resolved.ast.fixed_children(node)[0])
                    {
                        nested.insert(sym, FuncId(lowering.next_func_id));
                        lowering.next_func_id += 1;
                    }
                }
                lowering.default_function_maps.insert(plan.function, nested);
                local_default_plans.push((midx, plan));
            }
            Some(ScopeOwner::External(pkg_root, module_path)) => {
                let fid = FuncId(next_ext_fid);
                next_ext_fid += 1;
                lowering.func_map.insert(plan.function, fid);
                let decl_node = resolved.symbols[plan.declaration.0 as usize].def_node;
                let sig = plan
                    .self_function_types
                    .get(&decl_node)
                    .copied()
                    .unwrap_or(plan.signature);
                let self_params = plan
                    .self_function_parameters
                    .get(&decl_node)
                    .cloned()
                    .unwrap_or_else(|| plan.self_parameters.clone());
                ext_functions.push(ModularImportedFunction {
                    func_id: fid,
                    package_root: pkg_root,
                    module_path: module_path.clone(),
                    origin: ExternalFunctionOrigin::DefaultMethod {
                        implementor: plan.implementor,
                        trait_type: plan.trait_type,
                        method_name: plan.method_name,
                        signature: sig,
                        self_parameters: self_params,
                    },
                });
                let mut nested = HashMap::new();
                let mut nodes: Vec<_> = plan.self_function_types.keys().copied().collect();
                nodes.sort_by_key(|n| n.0);
                for node in nodes {
                    if resolved.ast.node(node).kind == NodeKind::FunctionDef
                        && node != decl_node
                        && let Some(&sym) = resolved
                            .node_symbols
                            .get(&resolved.ast.fixed_children(node)[0])
                    {
                        let nfid = FuncId(next_ext_fid);
                        next_ext_fid += 1;
                        nested.insert(sym, nfid);
                        let nsig = plan.self_function_types[&node];
                        let nself = plan
                            .self_function_parameters
                            .get(&node)
                            .cloned()
                            .unwrap_or_default();
                        ext_functions.push(ModularImportedFunction {
                            func_id: nfid,
                            package_root: pkg_root,
                            module_path: module_path.clone(),
                            origin: ExternalFunctionOrigin::DefaultHelper {
                                implementor: plan.implementor,
                                trait_type: plan.trait_type,
                                method_name: plan.method_name,
                                nested_symbol: sym,
                                signature: nsig,
                                self_parameters: nself,
                            },
                        });
                    }
                }
                lowering.default_function_maps.insert(plan.function, nested);
            }
            None => {}
        }
    }

    let node_owner = |node: NodeIndex| -> Option<ScopeOwner> {
        ast_scopes
            .get(&node)
            .copied()
            .and_then(|sid| resolve_scope_owner(resolved, sid, &local_mod_by_scope))
    };

    let mut all_derived_methods = Vec::new();
    let mut local_cmp_plans = Vec::new();
    for plan in &resolved.derived_comparisons {
        match node_owner(plan.node) {
            Some(ScopeOwner::Local(midx)) => {
                let fid = FuncId(lowering.next_func_id);
                lowering.next_func_id += 1;
                lowering
                    .derived_functions
                    .insert((plan.implementor, plan.trait_type, plan.method_name), fid);
                local_cmp_plans.push((midx, plan, fid));
            }
            Some(ScopeOwner::External(pkg_root, module_path)) => {
                let fid = FuncId(next_ext_fid);
                next_ext_fid += 1;
                lowering
                    .derived_functions
                    .insert((plan.implementor, plan.trait_type, plan.method_name), fid);
                all_derived_methods.push(DerivedMethod {
                    implementor: plan.implementor,
                    trait_type: plan.trait_type,
                    method_name: plan.method_name,
                    func_id: fid,
                });
                ext_functions.push(ModularImportedFunction {
                    func_id: fid,
                    package_root: pkg_root,
                    module_path,
                    origin: ExternalFunctionOrigin::DerivedMethod {
                        implementor: plan.implementor,
                        trait_type: plan.trait_type,
                        method_name: plan.method_name,
                        signature: plan.signature,
                        is_display: false,
                    },
                });
            }
            None => {}
        }
    }

    let mut local_disp_plans = Vec::new();
    for plan in &resolved.display_derivations {
        match node_owner(plan.node) {
            Some(ScopeOwner::Local(midx)) => {
                let fid = FuncId(lowering.next_func_id);
                lowering.next_func_id += 1;
                lowering
                    .derived_functions
                    .insert((plan.implementor, plan.trait_type, plan.method_name), fid);
                local_disp_plans.push((midx, plan, fid));
            }
            Some(ScopeOwner::External(pkg_root, module_path)) => {
                let fid = FuncId(next_ext_fid);
                next_ext_fid += 1;
                lowering
                    .derived_functions
                    .insert((plan.implementor, plan.trait_type, plan.method_name), fid);
                all_derived_methods.push(DerivedMethod {
                    implementor: plan.implementor,
                    trait_type: plan.trait_type,
                    method_name: plan.method_name,
                    func_id: fid,
                });
                ext_functions.push(ModularImportedFunction {
                    func_id: fid,
                    package_root: pkg_root,
                    module_path,
                    origin: ExternalFunctionOrigin::DerivedMethod {
                        implementor: plan.implementor,
                        trait_type: plan.trait_type,
                        method_name: plan.method_name,
                        signature: plan.signature,
                        is_display: true,
                    },
                });
            }
            None => {}
        }
    }

    let mut local_hash_plans = Vec::new();
    for plan in &resolved.hash_derivations {
        match node_owner(plan.node) {
            Some(ScopeOwner::Local(midx)) => {
                let fid = FuncId(lowering.next_func_id);
                lowering.next_func_id += 1;
                lowering
                    .derived_functions
                    .insert((plan.implementor, plan.trait_type, plan.method_name), fid);
                local_hash_plans.push((midx, plan, fid));
            }
            Some(ScopeOwner::External(pkg_root, module_path)) => {
                let fid = FuncId(next_ext_fid);
                next_ext_fid += 1;
                lowering
                    .derived_functions
                    .insert((plan.implementor, plan.trait_type, plan.method_name), fid);
                all_derived_methods.push(DerivedMethod {
                    implementor: plan.implementor,
                    trait_type: plan.trait_type,
                    method_name: plan.method_name,
                    func_id: fid,
                });
                ext_functions.push(ModularImportedFunction {
                    func_id: fid,
                    package_root: pkg_root,
                    module_path,
                    origin: ExternalFunctionOrigin::DerivedMethod {
                        implementor: plan.implementor,
                        trait_type: plan.trait_type,
                        method_name: plan.method_name,
                        signature: plan.signature,
                        is_display: false,
                    },
                });
            }
            None => {}
        }
    }

    let mut json_write_helpers = HashMap::new();
    let mut json_read_helpers = HashMap::new();
    let mut local_json_plans = Vec::new();
    for plan in &resolved.json_derivations {
        match node_owner(plan.node) {
            Some(ScopeOwner::Local(midx)) => {
                let to_json_fn = FuncId(lowering.next_func_id);
                lowering.next_func_id += 1;
                lowering.derived_functions.insert(
                    (plan.implementor, plan.trait_type, plan.method_name),
                    to_json_fn,
                );
                let write_fn = FuncId(lowering.next_func_id);
                lowering.next_func_id += 1;
                json_write_helpers.insert(plan.implementor, write_fn);
                let read_fn = FuncId(lowering.next_func_id);
                lowering.next_func_id += 1;
                json_read_helpers.insert(plan.implementor, read_fn);
                let from_json_fn = plan.from_json_symbol.map(|sym| {
                    let fid = FuncId(lowering.next_func_id);
                    lowering.next_func_id += 1;
                    lowering.func_map.insert(sym, fid);
                    fid
                });
                local_json_plans.push((midx, plan, to_json_fn, write_fn, read_fn, from_json_fn));
            }
            Some(ScopeOwner::External(pkg_root, module_path)) => {
                let to_json_fn = FuncId(next_ext_fid);
                next_ext_fid += 1;
                lowering.derived_functions.insert(
                    (plan.implementor, plan.trait_type, plan.method_name),
                    to_json_fn,
                );
                all_derived_methods.push(DerivedMethod {
                    implementor: plan.implementor,
                    trait_type: plan.trait_type,
                    method_name: plan.method_name,
                    func_id: to_json_fn,
                });
                ext_functions.push(ModularImportedFunction {
                    func_id: to_json_fn,
                    package_root: pkg_root,
                    module_path: module_path.clone(),
                    origin: ExternalFunctionOrigin::DerivedMethod {
                        implementor: plan.implementor,
                        trait_type: plan.trait_type,
                        method_name: plan.method_name,
                        signature: plan.signature,
                        is_display: false,
                    },
                });

                let write_fn = FuncId(next_ext_fid);
                next_ext_fid += 1;
                json_write_helpers.insert(plan.implementor, write_fn);
                ext_functions.push(ModularImportedFunction {
                    func_id: write_fn,
                    package_root: pkg_root,
                    module_path: module_path.clone(),
                    origin: ExternalFunctionOrigin::DerivedJsonHelper {
                        implementor: plan.implementor,
                        is_write: true,
                        signature: plan.nir_types.write_helper_fn_type,
                    },
                });

                let read_fn = FuncId(next_ext_fid);
                next_ext_fid += 1;
                json_read_helpers.insert(plan.implementor, read_fn);
                ext_functions.push(ModularImportedFunction {
                    func_id: read_fn,
                    package_root: pkg_root,
                    module_path: module_path.clone(),
                    origin: ExternalFunctionOrigin::DerivedJsonHelper {
                        implementor: plan.implementor,
                        is_write: false,
                        signature: plan.nir_types.read_helper_fn_type,
                    },
                });

                if let Some(sym) = plan.from_json_symbol {
                    let from_json_fn = FuncId(next_ext_fid);
                    next_ext_fid += 1;
                    lowering.func_map.insert(sym, from_json_fn);
                    ext_functions.push(ModularImportedFunction {
                        func_id: from_json_fn,
                        package_root: pkg_root,
                        module_path,
                        origin: ExternalFunctionOrigin::DerivedJsonFrom {
                            implementor: plan.implementor,
                            signature: plan.nir_types.from_json_fn_type,
                        },
                    });
                }
            }
            None => {}
        }
    }

    let mut owned_fns_per_mod: Vec<Vec<FuncId>> = vec![Vec::new(); init_plan.modules.len()];
    let mut local_exports: Vec<ModularLocalExport> = Vec::new();

    // Pass 2: lower ONLY local function definitions and record per-module ownership.
    for (midx, node_idx, fid, sym_opt) in local_pending {
        let before = lowering.functions.len();
        lowering.lower_function_def(resolved, node_idx, fid, None);
        for f in &lowering.functions[before..] {
            owned_fns_per_mod[midx].push(f.func_id);
        }
        if let Some(sym) = sym_opt {
            local_exports.push(ModularLocalExport {
                owner_module_index: midx as u32,
                origin: LocalExportOrigin::FunctionSymbol {
                    symbol: sym,
                    func_id: fid,
                },
            });
        }
    }

    for (midx, plan) in local_default_plans {
        let main_fid = lowering.func_map[&plan.function];
        let decl_node = resolved.symbols[plan.declaration.0 as usize].def_node;
        let before = lowering.functions.len();
        lowering.lower_function_def(resolved, decl_node, main_fid, Some(plan));
        for f in &lowering.functions[before..] {
            owned_fns_per_mod[midx].push(f.func_id);
        }
        local_exports.push(ModularLocalExport {
            owner_module_index: midx as u32,
            origin: LocalExportOrigin::DefaultMethod {
                implementor: plan.implementor,
                trait_type: plan.trait_type,
                method_name: plan.method_name,
                func_id: main_fid,
            },
        });
        let mut nodes: Vec<_> = plan.self_function_types.keys().copied().collect();
        nodes.sort_by_key(|n| n.0);
        for node in nodes {
            if resolved.ast.node(node).kind == NodeKind::FunctionDef && node != decl_node {
                let sym = resolved.node_symbols[&resolved.ast.fixed_children(node)[0]];
                let nfid = lowering.default_function_maps[&plan.function][&sym];
                let nbefore = lowering.functions.len();
                lowering.lower_function_def(resolved, node, nfid, Some(plan));
                for f in &lowering.functions[nbefore..] {
                    owned_fns_per_mod[midx].push(f.func_id);
                }
                local_exports.push(ModularLocalExport {
                    owner_module_index: midx as u32,
                    origin: LocalExportOrigin::DefaultHelper {
                        implementor: plan.implementor,
                        trait_type: plan.trait_type,
                        method_name: plan.method_name,
                        nested_symbol: sym,
                        func_id: nfid,
                    },
                });
            }
        }
    }

    let mut local_derived_methods = Vec::new();
    for (midx, plan, fid) in local_cmp_plans {
        lowering.functions.push(crate::derivation::lower(
            resolved,
            plan,
            fid,
            &lowering.func_map,
            &lowering.derived_functions,
        ));
        owned_fns_per_mod[midx].push(fid);
        let dm = DerivedMethod {
            implementor: plan.implementor,
            trait_type: plan.trait_type,
            method_name: plan.method_name,
            func_id: fid,
        };
        local_derived_methods.push(dm);
        all_derived_methods.push(dm);
        local_exports.push(ModularLocalExport {
            owner_module_index: midx as u32,
            origin: LocalExportOrigin::DerivedMethod {
                implementor: plan.implementor,
                trait_type: plan.trait_type,
                method_name: plan.method_name,
                func_id: fid,
            },
        });
    }
    for (midx, plan, fid) in local_disp_plans {
        lowering.functions.push(crate::display_derivation::lower(
            resolved,
            plan,
            fid,
            &lowering.func_map,
            &lowering.derived_functions,
        ));
        owned_fns_per_mod[midx].push(fid);
        let dm = DerivedMethod {
            implementor: plan.implementor,
            trait_type: plan.trait_type,
            method_name: plan.method_name,
            func_id: fid,
        };
        local_derived_methods.push(dm);
        all_derived_methods.push(dm);
        local_exports.push(ModularLocalExport {
            owner_module_index: midx as u32,
            origin: LocalExportOrigin::DerivedMethod {
                implementor: plan.implementor,
                trait_type: plan.trait_type,
                method_name: plan.method_name,
                func_id: fid,
            },
        });
    }
    for (midx, plan, fid) in local_hash_plans {
        lowering.functions.push(crate::hash_derivation::lower(
            resolved,
            plan,
            fid,
            &lowering.func_map,
            &lowering.derived_functions,
        ));
        owned_fns_per_mod[midx].push(fid);
        let dm = DerivedMethod {
            implementor: plan.implementor,
            trait_type: plan.trait_type,
            method_name: plan.method_name,
            func_id: fid,
        };
        local_derived_methods.push(dm);
        all_derived_methods.push(dm);
        local_exports.push(ModularLocalExport {
            owner_module_index: midx as u32,
            origin: LocalExportOrigin::DerivedMethod {
                implementor: plan.implementor,
                trait_type: plan.trait_type,
                method_name: plan.method_name,
                func_id: fid,
            },
        });
    }

    for (midx, plan, to_json_fn, write_fn, read_fn, from_json_fn) in local_json_plans {
        let before = lowering.functions.len();
        let mut json_dms = Vec::new();
        crate::json_derivation::lower_plan(
            resolved,
            plan,
            &mut lowering.next_func_id,
            &lowering.func_map,
            &lowering.derived_functions,
            &lowering.global_map,
            &json_write_helpers,
            &json_read_helpers,
            &mut lowering.functions,
            &mut json_dms,
        );
        for f in &lowering.functions[before..] {
            owned_fns_per_mod[midx].push(f.func_id);
        }
        local_derived_methods.extend(json_dms.iter().copied());
        all_derived_methods.extend(json_dms);
        local_exports.push(ModularLocalExport {
            owner_module_index: midx as u32,
            origin: LocalExportOrigin::DerivedMethod {
                implementor: plan.implementor,
                trait_type: plan.trait_type,
                method_name: plan.method_name,
                func_id: to_json_fn,
            },
        });
        local_exports.push(ModularLocalExport {
            owner_module_index: midx as u32,
            origin: LocalExportOrigin::DerivedJsonHelper {
                implementor: plan.implementor,
                is_write: true,
                func_id: write_fn,
            },
        });
        local_exports.push(ModularLocalExport {
            owner_module_index: midx as u32,
            origin: LocalExportOrigin::DerivedJsonHelper {
                implementor: plan.implementor,
                is_write: false,
                func_id: read_fn,
            },
        });
        if let Some(fj_fid) = from_json_fn {
            local_exports.push(ModularLocalExport {
                owner_module_index: midx as u32,
                origin: LocalExportOrigin::DerivedJsonFrom {
                    implementor: plan.implementor,
                    func_id: fj_fid,
                },
            });
        }
    }

    let root_main_fid = match startup {
        StartupMode::Main | StartupMode::RequiredMain => target_root_main(resolved, target_root)
            .and_then(|sym| lowering.func_map.get(&sym))
            .copied(),
        StartupMode::InitializationOnly => None,
    };

    let mut local_nominal_types = HashSet::new();
    for sym in &resolved.symbols {
        if matches!(
            sym.kind,
            SymbolKind::Type | SymbolKind::Trait | SymbolKind::Module | SymbolKind::Effect
        ) && !sym.def_node.is_null()
            && !resolved.type_pool.is_reserved_type(sym.type_index)
            && let Some(ScopeOwner::Local(midx)) =
                resolve_scope_owner(resolved, sym.scope, &local_mod_by_scope)
        {
            if let Some(canon) = resolved.type_pool.canonical_type(sym.type_index) {
                local_nominal_types.insert(canon);
            }
            if sym.type_index != TypeIndex::INVALID {
                local_exports.push(ModularLocalExport {
                    owner_module_index: midx as u32,
                    origin: LocalExportOrigin::Type {
                        symbol: sym.id,
                        type_index: sym.type_index,
                    },
                });
            }
        }
    }

    let mut local_impl_pairs = HashSet::new();
    collect_local_impl_pairs(resolved, target_root, &mut local_impl_pairs);

    let mut modules = Vec::with_capacity(init_plan.modules.len());
    for (midx, mplan) in init_plan.modules.into_iter().enumerate() {
        let mut owned_fns = std::mem::take(&mut owned_fns_per_mod[midx]);
        let scope_initializers = lower_scope_initializers_for_module(
            resolved,
            &mut lowering,
            mplan.ordered_initializers,
            &mut owned_fns,
        );
        owned_fns.sort_by_key(|f| f.0);

        let mut owned_globals = Vec::with_capacity(mplan.owned_globals.len());
        for (gid, sym) in mplan.owned_globals {
            owned_globals.push(gid);
            let info = init_plan.local_globals[gid.0 as usize];
            local_exports.push(ModularLocalExport {
                owner_module_index: midx as u32,
                origin: LocalExportOrigin::Global {
                    symbol: sym,
                    global_id: gid,
                    type_index: info.type_index,
                    is_mutable: info.is_mutable,
                },
            });
        }

        let module_type_index = if resolved.ast.node(mplan.scope_node).kind == NodeKind::ModuleDef {
            let name_node = resolved.ast.fixed_children(mplan.scope_node)[0];
            (!name_node.is_null())
                .then(|| resolved.node_symbols.get(&name_node).copied())
                .flatten()
                .map(|s| resolved.symbols[s.0 as usize].type_index)
                .filter(|&ty| ty != TypeIndex::INVALID)
        } else {
            None
        };

        modules.push(ModularScopeUnit {
            module_path: mplan.module_path,
            scope_node: mplan.scope_node,
            scope_id: mplan.scope_id,
            module_type_index,
            owned_functions: owned_fns,
            owned_globals,
            scope_initializers,
            local_module_deps: mplan
                .local_module_deps
                .into_iter()
                .map(|i| i as u32)
                .collect(),
            local_init_prereqs: mplan
                .local_init_prereqs
                .into_iter()
                .map(|i| i as u32)
                .collect(),
            external_module_deps: mplan.external_module_deps.into_iter().collect(),
            external_init_prereqs: mplan.external_init_prereqs.into_iter().collect(),
            main_func: (midx == 0).then_some(root_main_fid).flatten(),
        });
    }

    lowering.functions.sort_by_key(|f| f.func_id.0);
    let nir_module = NirModule {
        derived_methods: local_derived_methods,
        function_symbols: lowering.func_map,
        exported_roots: Vec::new(),
        functions: lowering.functions,
        globals: init_plan.local_globals,
        entry: None,
    };

    let imported_globals = init_plan
        .external_globals
        .into_iter()
        .map(|g| ModularImportedGlobal {
            global_id: g.global_id,
            package_root: g.package_root,
            module_path: g.module_path,
            symbol: g.symbol,
            type_index: g.info.type_index,
            is_mutable: g.info.is_mutable,
        })
        .collect();

    Ok(ModularNirOutput {
        nir_module,
        modules,
        imported_functions: ext_functions,
        imported_globals,
        local_exports,
        all_derived_methods,
        local_nominal_types,
        local_impl_pairs,
        local_scope_ids,
    })
}

fn expr_refs(expr: &NirExpr, out_fns: &mut BTreeSet<u32>, out_globals: &mut BTreeSet<u32>) {
    match expr {
        NirExpr::ScopedCall { call, .. } => expr_refs(call, out_fns, out_globals),
        NirExpr::Call(fid, _) | NirExpr::CallWithProof(fid, _) | NirExpr::NewClosure(fid, _) => {
            out_fns.insert(fid.0);
        }
        NirExpr::LoadGlobal(gid) => {
            out_globals.insert(gid.0);
        }
        _ => {}
    }
}

fn remap_expr_ext_ids(
    expr: &mut NirExpr,
    fn_remap: &HashMap<u32, u32>,
    global_remap: &HashMap<u32, u32>,
) {
    match expr {
        NirExpr::ScopedCall { call, .. } => remap_expr_ext_ids(call, fn_remap, global_remap),
        NirExpr::Call(fid, _) | NirExpr::CallWithProof(fid, _) | NirExpr::NewClosure(fid, _) => {
            if let Some(&new_id) = fn_remap.get(&fid.0) {
                *fid = FuncId(new_id);
            }
        }
        NirExpr::LoadGlobal(gid) => {
            if let Some(&new_gid) = global_remap.get(&gid.0) {
                *gid = GlobalId(new_gid);
            }
        }
        _ => {}
    }
}

fn remap_stmt_ext_ids(
    stmt: &mut NirStmt,
    fn_remap: &HashMap<u32, u32>,
    global_remap: &HashMap<u32, u32>,
) {
    match stmt {
        NirStmt::Scoped { statement, .. } => remap_stmt_ext_ids(statement, fn_remap, global_remap),
        NirStmt::Assign(_, expr) => remap_expr_ext_ids(expr, fn_remap, global_remap),
        NirStmt::StoreGlobal(gid, _) => {
            if let Some(&new_gid) = global_remap.get(&gid.0) {
                *gid = GlobalId(new_gid);
            }
        }
        _ => {}
    }
}

fn trait_ancestors(pool: &TypePool, trait_ty: TypeIndex) -> HashSet<TypeIndex> {
    let mut out = HashSet::new();
    let Some(canon) = pool.canonical_type(trait_ty) else {
        return out;
    };
    let mut stack = vec![canon];
    while let Some(cur) = stack.pop() {
        if !out.insert(cur) {
            continue;
        }
        if let TypeKind::Trait { parents, .. } = &pool.get(cur).kind {
            for &p in parents {
                if let Some(cp) = pool.canonical_type(p) {
                    stack.push(cp);
                }
            }
        }
    }
    out
}

fn ext_fn_self_trait_reqs(
    pool: &TypePool,
    origin: &ExternalFunctionOrigin,
) -> Vec<(TypeIndex, TypeIndex)> {
    match origin {
        ExternalFunctionOrigin::DefaultMethod {
            trait_type,
            signature,
            self_parameters,
            ..
        }
        | ExternalFunctionOrigin::DefaultHelper {
            trait_type,
            signature,
            self_parameters,
            ..
        } => {
            let Some(csig) = pool.canonical_type(*signature) else {
                return Vec::new();
            };
            let TypeKind::Function { params, .. } = &pool.get(csig).kind else {
                return Vec::new();
            };
            let Some(view) = pool.canonical_type(*trait_type) else {
                return Vec::new();
            };
            self_parameters
                .iter()
                .filter_map(|&pos| {
                    params
                        .get(pos)
                        .and_then(|&pty| pool.canonical_type(pty))
                        .map(|concrete| (concrete, view))
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Relocate `TypePool` dispatch IDs, run NIR optimization passes, prune unreferenced external
/// dispatch tables, and compact referenced external `FuncId`s & `GlobalId`s densely after local tables.
pub fn finalize_modular_nir(
    output: &mut ModularNirOutput,
    type_pool: &mut TypePool,
    opt_level: OptLevel,
) -> Result<(), String> {
    type_pool
        .remap_function_ids(|symbol| {
            output
                .nir_module
                .function_symbols
                .get(&SymbolId(symbol))
                .map(|f| f.0)
        })
        .map_err(|sym| format!("method symbol {sym} has no generated or imported function ID"))?;

    type_pool
        .resolve_derived_methods(
            output
                .all_derived_methods
                .iter()
                .map(|dm| (dm.implementor, dm.trait_type, dm.method_name, dm.func_id.0)),
        )
        .map_err(|e| format!("cannot publish derived method in modular TypePool: {e}"))?;

    type_pool
        .validate_type_identities()
        .map_err(|e| format!("invalid finalized source type identities: {e}"))?;

    crate::optimize_module(&mut output.nir_module, type_pool, opt_level);

    let local_fn_count = output.nir_module.functions.len() as u32;
    let local_global_count = output.nir_module.globals.len() as u32;

    let ext_fn_by_id: HashMap<u32, &ModularImportedFunction> = output
        .imported_functions
        .iter()
        .map(|f| (f.func_id.0, f))
        .collect();
    let ext_glob_by_id: HashMap<u32, &ModularImportedGlobal> = output
        .imported_globals
        .iter()
        .map(|g| (g.global_id.0, g))
        .collect();

    let mut all_ref_fns = BTreeSet::new();
    let mut all_ref_globals = BTreeSet::new();
    let mut needed_vtables: HashSet<(TypeIndex, TypeIndex)> = HashSet::new();

    for func in &output.nir_module.functions {
        if let Some(csig) = type_pool.canonical_type(func.function_type)
            && let TypeKind::Function { params, .. } = &type_pool.get(csig).kind
        {
            let abi = func.entry_abi();
            for (pabi, &pty) in abi.parameters.iter().zip(params) {
                if let nsbc::ParameterAbi::TraitSelf { view } = pabi
                    && let (Some(concrete), Some(cview)) = (
                        type_pool.canonical_type(pty),
                        type_pool.canonical_type(*view),
                    )
                {
                    needed_vtables.insert((concrete, cview));
                }
            }
        }
        for block in &func.blocks {
            for stmt in &block.stmts {
                match stmt.unscoped() {
                    NirStmt::Assign(_, expr) => {
                        expr_refs(expr, &mut all_ref_fns, &mut all_ref_globals)
                    }
                    NirStmt::StoreGlobal(gid, _) => {
                        all_ref_globals.insert(gid.0);
                    }
                    _ => {}
                }
            }
        }
    }

    let is_local_vtable = |table: &type_pool::VTable| -> bool {
        let Some(imp) = type_pool.canonical_type(table.implementor) else {
            return false;
        };
        let Some(tr) = type_pool.canonical_type(table.trait_type) else {
            return false;
        };
        output.local_nominal_types.contains(&imp)
            || output.local_nominal_types.contains(&tr)
            || output.local_impl_pairs.contains(&(imp, tr))
            || table
                .visible_scope
                .is_some_and(|sc| output.local_scope_ids.contains(&sc))
    };

    // Fixed-point closure over local VTables + TraitSelf-required external VTables + backing TraitImplRecords.
    loop {
        let mut changed = false;
        for &fid in &all_ref_fns.clone() {
            if fid >= EXT_FN_BASE
                && let Some(imp_fn) = ext_fn_by_id.get(&fid)
            {
                for pair in ext_fn_self_trait_reqs(type_pool, &imp_fn.origin) {
                    if needed_vtables.insert(pair) {
                        changed = true;
                    }
                }
            }
        }
        let mut required_impl_ancestors: HashSet<(TypeIndex, TypeIndex)> = HashSet::new();
        for table in type_pool.vtables_snapshot() {
            let Some(imp) = type_pool.canonical_type(table.implementor) else {
                continue;
            };
            let Some(tr) = type_pool.canonical_type(table.trait_type) else {
                continue;
            };
            if is_local_vtable(table) || needed_vtables.contains(&(imp, tr)) {
                for &entry_fid in &table.entries {
                    if all_ref_fns.insert(entry_fid) {
                        changed = true;
                    }
                }
                for anc in trait_ancestors(type_pool, tr) {
                    required_impl_ancestors.insert((imp, anc));
                }
            }
        }
        for rec in type_pool.trait_impls_snapshot() {
            let Some(imp) = type_pool.canonical_type(rec.implementor) else {
                continue;
            };
            let Some(tr) = type_pool.canonical_type(rec.trait_type) else {
                continue;
            };
            let is_local_rec = output.local_nominal_types.contains(&imp)
                || output.local_nominal_types.contains(&tr)
                || output.local_impl_pairs.contains(&(imp, tr))
                || rec
                    .visible_scope
                    .is_some_and(|sc| output.local_scope_ids.contains(&sc))
                || rec.methods.iter().any(|m| m.func_id < EXT_FN_BASE);
            if is_local_rec || required_impl_ancestors.contains(&(imp, tr)) {
                for m in &rec.methods {
                    if all_ref_fns.insert(m.func_id) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut required_impl_ancestors: HashSet<(TypeIndex, TypeIndex)> = HashSet::new();
    for table in type_pool.vtables_snapshot() {
        if let (Some(imp), Some(tr)) = (
            type_pool.canonical_type(table.implementor),
            type_pool.canonical_type(table.trait_type),
        ) && (is_local_vtable(table) || needed_vtables.contains(&(imp, tr)))
        {
            for anc in trait_ancestors(type_pool, tr) {
                required_impl_ancestors.insert((imp, anc));
            }
        }
    }

    let keep_recs: Vec<bool> = type_pool
        .trait_impls_snapshot()
        .iter()
        .map(|rec| {
            let (Some(imp), Some(tr)) = (
                type_pool.canonical_type(rec.implementor),
                type_pool.canonical_type(rec.trait_type),
            ) else {
                return false;
            };
            output.local_nominal_types.contains(&imp)
                || output.local_nominal_types.contains(&tr)
                || output.local_impl_pairs.contains(&(imp, tr))
                || rec
                    .visible_scope
                    .is_some_and(|sc| output.local_scope_ids.contains(&sc))
                || rec.methods.iter().any(|m| m.func_id < EXT_FN_BASE)
                || required_impl_ancestors.contains(&(imp, tr))
        })
        .collect();
    let keep_vtables: Vec<bool> = type_pool
        .vtables_snapshot()
        .iter()
        .map(|table| {
            let (Some(imp), Some(tr)) = (
                type_pool.canonical_type(table.implementor),
                type_pool.canonical_type(table.trait_type),
            ) else {
                return false;
            };
            is_local_vtable(table) || needed_vtables.contains(&(imp, tr))
        })
        .collect();
    let mut rec_iter = keep_recs.into_iter();
    let mut vt_iter = keep_vtables.into_iter();
    type_pool.retain_dispatch_tables(
        |_owner, slot| slot.func_id < EXT_FN_BASE,
        |_rec| rec_iter.next().unwrap_or(false),
        |_table| vt_iter.next().unwrap_or(false),
    );

    let used_ext_fn_ids: Vec<u32> = all_ref_fns
        .iter()
        .copied()
        .filter(|&fid| fid >= EXT_FN_BASE)
        .collect();
    let mut fn_remap: HashMap<u32, u32> = HashMap::with_capacity(used_ext_fn_ids.len());
    let mut compacted_fns = Vec::with_capacity(used_ext_fn_ids.len());
    for (idx, old_fid) in used_ext_fn_ids.into_iter().enumerate() {
        let new_fid = local_fn_count + idx as u32;
        fn_remap.insert(old_fid, new_fid);
        let mut entry = (*ext_fn_by_id
            .get(&old_fid)
            .ok_or_else(|| format!("missing imported function descriptor for ID {old_fid}"))?)
        .clone();
        entry.func_id = FuncId(new_fid);
        compacted_fns.push(entry);
    }

    let used_ext_glob_ids: Vec<u32> = all_ref_globals
        .iter()
        .copied()
        .filter(|&gid| gid >= EXT_GLOBAL_BASE)
        .collect();
    let mut glob_remap: HashMap<u32, u32> = HashMap::with_capacity(used_ext_glob_ids.len());
    let mut compacted_globals = Vec::with_capacity(used_ext_glob_ids.len());
    for (idx, old_gid) in used_ext_glob_ids.into_iter().enumerate() {
        let new_gid = local_global_count + idx as u32;
        glob_remap.insert(old_gid, new_gid);
        let mut entry = (*ext_glob_by_id
            .get(&old_gid)
            .ok_or_else(|| format!("missing imported global descriptor for ID {old_gid}"))?)
        .clone();
        entry.global_id = GlobalId(new_gid);
        compacted_globals.push(entry);
    }

    for func in &mut output.nir_module.functions {
        for block in &mut func.blocks {
            for stmt in &mut block.stmts {
                remap_stmt_ext_ids(stmt, &fn_remap, &glob_remap);
            }
        }
    }

    output
        .nir_module
        .function_symbols
        .retain(|_, fid| fid.0 < EXT_FN_BASE || fn_remap.contains_key(&fid.0));
    for fid in output.nir_module.function_symbols.values_mut() {
        if let Some(&new_id) = fn_remap.get(&fid.0) {
            *fid = FuncId(new_id);
        }
    }

    type_pool
        .remap_function_ids(|fid| {
            if fid < EXT_FN_BASE {
                Some(fid)
            } else {
                fn_remap.get(&fid).copied()
            }
        })
        .map_err(|fid| format!("unmapped external function ID {fid} in pruned TypePool"))?;

    // Post-optimization scan: ensure every module's `local_module_deps` and `external_module_deps`
    // cover any cross-module targets introduced by inlining or retained supertrait VTables.
    let mut local_fn_owner: HashMap<u32, u32> = HashMap::with_capacity(local_fn_count as usize);
    for (midx, m) in output.modules.iter().enumerate() {
        for &fid in &m.owned_functions {
            local_fn_owner.insert(fid.0, midx as u32);
        }
    }
    let fn_by_id: BTreeMap<u32, &crate::NirFunction> = output
        .nir_module
        .functions
        .iter()
        .map(|f| (f.func_id.0, f))
        .collect();
    let comp_fn_by_id: HashMap<u32, (NodeIndex, Vec<String>)> = compacted_fns
        .iter()
        .map(|f| (f.func_id.0, (f.package_root, f.module_path.clone())))
        .collect();
    let comp_glob_by_id: HashMap<u32, (NodeIndex, Vec<String>)> = compacted_globals
        .iter()
        .map(|g| (g.global_id.0, (g.package_root, g.module_path.clone())))
        .collect();

    // Any imported function referenced from a retained VTable / TraitImplRecord is required
    // whenever the root module (or the module owning that local type) loads.
    let dispatch_ext_deps: BTreeSet<(NodeIndex, Vec<String>)> = type_pool
        .referenced_function_ids()
        .into_iter()
        .filter(|&fid| fid >= local_fn_count)
        .filter_map(|fid| comp_fn_by_id.get(&fid).cloned())
        .collect();

    for (midx, m) in output.modules.iter_mut().enumerate() {
        let mut ldeps: BTreeSet<u32> = m.local_module_deps.iter().copied().collect();
        let mut edeps: BTreeSet<(NodeIndex, Vec<String>)> =
            m.external_module_deps.iter().cloned().collect();
        if midx == 0 {
            edeps.extend(dispatch_ext_deps.iter().cloned());
        }
        for &fid in &m.owned_functions {
            if let Some(func) = fn_by_id.get(&fid.0) {
                let mut fns = BTreeSet::new();
                let mut globs = BTreeSet::new();
                for block in &func.blocks {
                    for stmt in &block.stmts {
                        match stmt.unscoped() {
                            NirStmt::Assign(_, expr) => expr_refs(expr, &mut fns, &mut globs),
                            NirStmt::StoreGlobal(gid, _) => {
                                globs.insert(gid.0);
                            }
                            _ => {}
                        }
                    }
                }
                for target_fid in fns {
                    if target_fid < local_fn_count {
                        if let Some(&owner_m) = local_fn_owner.get(&target_fid)
                            && owner_m != midx as u32
                        {
                            ldeps.insert(owner_m);
                        }
                    } else if let Some(ext_mod) = comp_fn_by_id.get(&target_fid) {
                        edeps.insert(ext_mod.clone());
                    }
                }
                for target_gid in globs {
                    if target_gid >= local_global_count
                        && let Some(ext_mod) = comp_glob_by_id.get(&target_gid)
                    {
                        edeps.insert(ext_mod.clone());
                    }
                }
            }
        }
        m.local_module_deps = ldeps.into_iter().collect();
        m.external_module_deps = edeps.into_iter().collect();
    }

    output.imported_functions = compacted_fns;
    output.imported_globals = compacted_globals;
    Ok(())
}
