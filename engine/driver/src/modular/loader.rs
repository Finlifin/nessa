//! On-demand per-`mod` dynamic VM loader, symbol linker, in-place bytecode relocator,
//! and prerequisite-ordered `__init__` executor (`ModuleLoader`).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use interpreter::{Vm, VmError, VmResult};
use nsbc::{
    AddrMode, ArtifactError, CaptureAbi, Constant, ExportTarget, FuncId, FunctionAbi, GlobalId,
    GlobalInfo, Instruction, InstructionData, MethodCallScope, ModularArtifact, Opcode,
    ParameterAbi, Reg, SymbolLinkSelector, WasmBindingDef, WasmModuleDef,
};
use runtime::FunctionCode;
use type_pool::{
    IdentityPathSegment, MergedDispatchState, MergedPoolMapping, TypeId, TypeIndex, TypeKind,
    TypePool,
};

use crate::execution::{
    ArtifactLoadError, builtin_requires_bytes_roles, builtin_requires_list_roles,
    builtin_requires_map_roles,
};

const NARROW12_LIMIT: u32 = 4096;
const NARROW14_BASE: u32 = 4096;
const NARROW14_LIMIT: u32 = 12288;
const WIDE17_BASE: u32 = 12288;
const WIDE17_LIMIT: u32 = 1 << 17;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConstDedupKey {
    Int(i64),
    UInt(u64),
    Int128(i128),
    UInt128(u128),
    FloatBits(u64),
    Str(String),
    Char(char),
    Type(u32),
    Enum(u32, u32),
}

impl ConstDedupKey {
    fn from_constant(c: &Constant) -> Self {
        match c {
            Constant::Int(v) => Self::Int(*v),
            Constant::UInt(v) => Self::UInt(*v),
            Constant::Int128(v) => Self::Int128(*v),
            Constant::UInt128(v) => Self::UInt128(*v),
            Constant::Float(v) => Self::FloatBits(v.to_bits()),
            Constant::Str(s) => Self::Str(s.clone()),
            Constant::Char(c) => Self::Char(*c),
            Constant::Type(ty) => Self::Type(ty.as_u32()),
            Constant::Enum {
                type_index,
                variant,
            } => Self::Enum(type_index.as_u32(), *variant),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleLoadState {
    Unloaded,
    LoadedUninitialized,
    Initialized,
}

struct RegisteredPackage {
    artifact: Arc<ModularArtifact>,
    pool_mapping: Option<MergedPoolMapping>,
    dispatch_state: MergedDispatchState,
    wasm_binding_base: u32,
    local_fn_to_vm: Vec<Option<FuncId>>,
    local_fn_owner_mod: Vec<usize>,
    local_global_to_vm: Vec<Option<GlobalId>>,
    imported_fn_to_vm: Vec<Option<FuncId>>,
    imported_global_to_vm: Vec<Option<GlobalId>>,
    module_states: Vec<ModuleLoadState>,
    mod_by_path: HashMap<Vec<String>, usize>,
    sym_by_selector: HashMap<SymbolLinkSelector, usize>,
    local_fn_by_id: HashMap<u32, usize>,
    scopes_by_fn: HashMap<u32, Vec<(u32, u32)>>,
}

/// Prepared result of loading a module and its transitive dependencies into a [`Vm`]
/// prior to executing pending module initializers.
#[derive(Debug, Clone)]
pub struct PreparedModuleLoad {
    /// Relocated VM `FuncId`s of pending `__module_init_*` functions in topological order.
    pub pending_initializers: Vec<FuncId>,
    /// Modules `(qualified_name, module_path)` whose initializers are included in `pending_initializers`.
    pub pending_modules: Vec<(String, Vec<String>)>,
    /// Modules `(qualified_name, module_path)` newly loaded from bytecode during this call.
    pub newly_loaded_modules: Vec<(String, Vec<String>)>,
    /// Relocated `main` `FuncId` of the target module, if declared.
    pub main_func: Option<FuncId>,
}

struct ModuleExternalRefs {
    imported_functions: BTreeSet<usize>,
    imported_globals: BTreeSet<usize>,
    active_traits: HashSet<TypeId>,
    active_methods: HashSet<str_interner::StrId>,
}

/// Dynamic per-`mod` linker, relocator, and initializer manager for a [`Vm`].
pub struct ModuleLoader {
    packages: Vec<RegisteredPackage>,
    pkg_by_qname: HashMap<String, usize>,
    nominal_type_owners: HashMap<TypeId, (usize, usize)>,
    active_traits: HashSet<TypeId>,
    active_methods: HashSet<str_interner::StrId>,
    next_narrow12: u32,
    narrow12_dedup: HashMap<ConstDedupKey, u32>,
    next_narrow14: u32,
    narrow14_dedup: HashMap<ConstDedupKey, u32>,
    next_wide17: u32,
    wide17_dedup: HashMap<ConstDedupKey, u32>,
    pool_initialized: bool,
}

impl ModuleLoader {
    pub fn new() -> Self {
        Self {
            packages: Vec::new(),
            pkg_by_qname: HashMap::new(),
            nominal_type_owners: HashMap::new(),
            active_traits: HashSet::new(),
            active_methods: HashSet::new(),
            next_narrow12: 0,
            narrow12_dedup: HashMap::new(),
            next_narrow14: NARROW14_BASE,
            narrow14_dedup: HashMap::new(),
            next_wide17: WIDE17_BASE,
            wide17_dedup: HashMap::new(),
            pool_initialized: false,
        }
    }

    /// Register a precompiled [`ModularArtifact`] without loading any of its modules yet.
    pub fn register_artifact(
        &mut self,
        artifact: Arc<ModularArtifact>,
    ) -> Result<(), ArtifactLoadError> {
        if self
            .pkg_by_qname
            .contains_key(&artifact.exports.qualified_name)
        {
            return Ok(());
        }
        if artifact.builtin_abi_version != runtime::BUILTIN_ABI_VERSION {
            return Err(ArtifactLoadError::IncompatibleBuiltin(format!(
                "revision {}, expected {}",
                artifact.builtin_abi_version,
                runtime::BUILTIN_ABI_VERSION
            )));
        }
        for import in &artifact.builtins {
            if runtime::lookup_builtin_fn_meta(import.id).map(|m| m.name)
                != Some(import.name.as_str())
            {
                return Err(ArtifactLoadError::IncompatibleBuiltin(format!(
                    "unknown ID/name pair {} ({})",
                    import.id, import.name
                )));
            }
        }

        let local_fn_count = artifact.codegen_output.functions.len();
        let local_glob_count = artifact.codegen_output.globals.len();
        let imp_fn_count = artifact.imports.functions.len();
        let imp_glob_count = artifact.imports.globals.len();
        let mod_count = artifact.exports.modules.len();

        let mod_by_path: HashMap<Vec<String>, usize> = artifact
            .exports
            .modules
            .iter()
            .enumerate()
            .map(|(idx, m)| (m.module_path.clone(), idx))
            .collect();
        let sym_by_selector: HashMap<SymbolLinkSelector, usize> = artifact
            .exports
            .symbols
            .iter()
            .enumerate()
            .map(|(idx, s)| (s.selector.clone(), idx))
            .collect();
        let local_fn_by_id: HashMap<u32, usize> = artifact
            .codegen_output
            .functions
            .iter()
            .enumerate()
            .map(|(idx, f)| (f.func_id.0, idx))
            .collect();
        let mut scopes_by_fn: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
        for sc in &artifact.codegen_output.method_call_scopes {
            scopes_by_fn
                .entry(sc.func_id.0)
                .or_default()
                .push((sc.pc, sc.scope));
        }

        let mut local_fn_owner_mod = vec![0usize; local_fn_count];
        for (midx, mdesc) in artifact.exports.modules.iter().enumerate() {
            for &fid in &mdesc.owned_functions {
                if let Some(slot) = local_fn_owner_mod.get_mut(fid.0 as usize) {
                    *slot = midx;
                }
            }
        }

        let pkg_idx = self.packages.len();
        let qname = artifact.exports.qualified_name.clone();
        if let Some(id_input) = artifact.type_pool.identity_input() {
            for decl in &id_input.declarations {
                let Some(pkg_ctx) = id_input.packages.get(decl.package as usize) else {
                    continue;
                };
                if pkg_ctx.qualified_name != qname || pkg_ctx.identity != artifact.exports.identity
                {
                    continue;
                }
                let tid = artifact.type_pool.get(decl.type_index).type_id;
                if tid == TypeId::ZERO {
                    continue;
                }
                let mut named_prefix = Vec::new();
                for seg in &decl.path {
                    if let IdentityPathSegment::Named(s) = seg {
                        named_prefix.push(s.clone());
                    } else {
                        break;
                    }
                }
                if artifact.exports.is_privileged_std
                    && named_prefix.first().map(String::as_str) == Some("std")
                {
                    named_prefix.remove(0);
                }
                let mut owner_midx = 0usize;
                for len in (0..=named_prefix.len()).rev() {
                    if let Some(&m) = mod_by_path.get(&named_prefix[..len]) {
                        owner_midx = m;
                        break;
                    }
                }
                self.nominal_type_owners.insert(tid, (pkg_idx, owner_midx));
            }
        }

        self.packages.push(RegisteredPackage {
            artifact,
            pool_mapping: None,
            dispatch_state: MergedDispatchState::default(),
            wasm_binding_base: 0,
            local_fn_to_vm: vec![None; local_fn_count],
            local_fn_owner_mod,
            local_global_to_vm: vec![None; local_glob_count],
            imported_fn_to_vm: vec![None; imp_fn_count],
            imported_global_to_vm: vec![None; imp_glob_count],
            module_states: vec![ModuleLoadState::Unloaded; mod_count],
            mod_by_path,
            sym_by_selector,
            local_fn_by_id,
            scopes_by_fn,
        });
        self.pkg_by_qname.insert(qname, pkg_idx);
        Ok(())
    }

    /// Register a slice of [`ModularArtifact`]s.
    pub fn register_artifacts(
        &mut self,
        artifacts: &[Arc<ModularArtifact>],
    ) -> Result<(), ArtifactLoadError> {
        for art in artifacts {
            self.register_artifact(Arc::clone(art))?;
        }
        Ok(())
    }

    /// Query the current [`ModuleLoadState`] of `(pkg_qname, module_path)`.
    pub fn module_state(&self, pkg_qname: &str, module_path: &[String]) -> Option<ModuleLoadState> {
        let &pidx = self.pkg_by_qname.get(pkg_qname)?;
        let &midx = self.packages[pidx].mod_by_path.get(module_path)?;
        Some(self.packages[pidx].module_states[midx])
    }

    /// Return all `(qualified_name, module_path)` pairs currently loaded into the VM.
    pub fn loaded_modules(&self) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        for pkg in &self.packages {
            for (midx, state) in pkg.module_states.iter().enumerate() {
                if *state != ModuleLoadState::Unloaded {
                    out.push((
                        pkg.artifact.exports.qualified_name.clone(),
                        pkg.artifact.exports.modules[midx].module_path.clone(),
                    ));
                }
            }
        }
        out
    }

    /// Resolve a relocated VM [`FuncId`] for an exported function in a loaded module.
    pub fn lookup_function(
        &self,
        pkg_qname: &str,
        selector: &SymbolLinkSelector,
    ) -> Option<FuncId> {
        let &pidx = self.pkg_by_qname.get(pkg_qname)?;
        let pkg = &self.packages[pidx];
        let &sidx = pkg.sym_by_selector.get(selector)?;
        match &pkg.artifact.exports.symbols[sidx].target {
            ExportTarget::Function { func_id, .. } => pkg
                .local_fn_to_vm
                .get(func_id.0 as usize)
                .copied()
                .flatten(),
            _ => None,
        }
    }

    fn alloc_narrow12(
        &mut self,
        vm: &mut Vm,
        constant: Constant,
    ) -> Result<u16, ArtifactLoadError> {
        let key = ConstDedupKey::from_constant(&constant);
        if let Some(&idx) = self.narrow12_dedup.get(&key) {
            return Ok(idx as u16);
        }
        if self.next_narrow12 >= NARROW12_LIMIT {
            return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "constant relocation",
                "12-bit narrow constant descriptor region (4096) exhausted",
            )));
        }
        let idx = self.next_narrow12;
        self.next_narrow12 += 1;
        vm.ensure_constant_len((idx + 1) as usize);
        vm.set_constant_at(idx, &constant)
            .map_err(ArtifactLoadError::Runtime)?;
        self.narrow12_dedup.insert(key, idx);
        Ok(idx as u16)
    }

    fn alloc_narrow14(
        &mut self,
        vm: &mut Vm,
        constant: Constant,
    ) -> Result<u16, ArtifactLoadError> {
        let key = ConstDedupKey::from_constant(&constant);
        if let Some(&idx) = self.narrow14_dedup.get(&key) {
            return Ok(idx as u16);
        }
        if let Some(&idx) = self.narrow12_dedup.get(&key) {
            return Ok(idx as u16);
        }
        if self.next_narrow14 >= NARROW14_LIMIT {
            return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "constant relocation",
                "14-bit CallFar constant descriptor region exhausted",
            )));
        }
        let idx = self.next_narrow14;
        self.next_narrow14 += 1;
        vm.ensure_constant_len((idx + 1) as usize);
        vm.set_constant_at(idx, &constant)
            .map_err(ArtifactLoadError::Runtime)?;
        self.narrow14_dedup.insert(key, idx);
        Ok(idx as u16)
    }

    fn alloc_wide17(&mut self, vm: &mut Vm, constant: Constant) -> Result<u32, ArtifactLoadError> {
        let key = ConstDedupKey::from_constant(&constant);
        if let Some(&idx) = self.wide17_dedup.get(&key) {
            return Ok(idx);
        }
        if let Some(&idx) = self.narrow12_dedup.get(&key) {
            return Ok(idx);
        }
        if let Some(&idx) = self.narrow14_dedup.get(&key) {
            return Ok(idx);
        }
        if self.next_wide17 >= WIDE17_LIMIT {
            return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "constant relocation",
                "17-bit constant pool capacity (131072) exhausted",
            )));
        }
        let idx = self.next_wide17;
        self.next_wide17 += 1;
        vm.ensure_constant_len((idx + 1) as usize);
        vm.set_constant_at(idx, &constant)
            .map_err(ArtifactLoadError::Runtime)?;
        self.wide17_dedup.insert(key, idx);
        Ok(idx)
    }

    fn resolve_dep_pkg_idx(
        &self,
        pkg_idx: usize,
        imp_pkg_index: u32,
    ) -> Result<usize, ArtifactLoadError> {
        let imp_ref = self.packages[pkg_idx]
            .artifact
            .imports
            .packages
            .get(imp_pkg_index as usize)
            .ok_or_else(|| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "module linker",
                    "imported package index out of range",
                ))
            })?;
        let &target_idx = self
            .pkg_by_qname
            .get(&imp_ref.qualified_name)
            .ok_or_else(|| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "module linker",
                    format!(
                        "missing registered package `{}` required by `{}`",
                        imp_ref.qualified_name,
                        self.packages[pkg_idx].artifact.exports.qualified_name
                    ),
                ))
            })?;
        let target_exp = &self.packages[target_idx].artifact.exports;
        if target_exp.identity != imp_ref.identity
            || target_exp.interface_hash != imp_ref.interface_hash
        {
            return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "module linker",
                format!(
                    "interface hash mismatch linking `{}` against `{}`",
                    self.packages[pkg_idx].artifact.exports.qualified_name, imp_ref.qualified_name
                ),
            )));
        }
        Ok(target_idx)
    }

    /// Load `(pkg_qname, module_path)` and ONLY its reachable local & external `mod` dependencies
    /// into `vm`, returning [`PreparedModuleLoad`] with relocated pending `__module_init_*` IDs.
    pub fn prepare_module(
        &mut self,
        vm: &mut Vm,
        pkg_qname: &str,
        module_path: &[String],
    ) -> Result<PreparedModuleLoad, ArtifactLoadError> {
        if !self.pool_initialized {
            vm.ensure_constant_len(WIDE17_BASE as usize);
            self.pool_initialized = true;
        }

        let &root_pidx = self.pkg_by_qname.get(pkg_qname).ok_or_else(|| {
            ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "module loader",
                format!("package `{pkg_qname}` is not registered in ModuleLoader"),
            ))
        })?;
        let &root_midx = self.packages[root_pidx]
            .mod_by_path
            .get(module_path)
            .ok_or_else(|| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "module loader",
                    format!(
                        "module `{}` does not exist in package `{pkg_qname}`",
                        module_path.join(".")
                    ),
                ))
            })?;

        // 1. Discover transitive `(pkg_idx, mod_idx)` reachability from `(root_pidx, root_midx)`.
        let mut reachable_set: BTreeSet<(usize, usize)> = BTreeSet::new();
        let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
        reachable_set.insert((root_pidx, root_midx));
        queue.push_back((root_pidx, root_midx));

        // If `std` is registered and `std.prelude` / `std.collections` / `std.traits` exist,
        // ensure `std`'s core collection/trait roles are merged if any builtin requires them.
        if let Some(std_pidx) = self.pkg_by_qname.get("org.nessa/std").copied().or_else(|| {
            self.packages
                .iter()
                .position(|p| p.artifact.exports.is_privileged_std)
        }) {
            // Only merge `std`'s TypePool if needed for collection roles; do NOT eagerly load all `std` modules!
            if self.packages[std_pidx].pool_mapping.is_none() {
                let mapping = vm
                    .merge_type_pool(&self.packages[std_pidx].artifact.type_pool)
                    .map_err(|e| {
                        ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                            "TypePool merge (std)",
                            e.to_string(),
                        ))
                    })?;
                self.packages[std_pidx].pool_mapping = Some(mapping);
            }
        }

        loop {
            while let Some((pidx, midx)) = queue.pop_front() {
                let mdesc = self.packages[pidx].artifact.exports.modules[midx].clone();
                for &ldep in mdesc
                    .local_module_deps
                    .iter()
                    .chain(&mdesc.local_init_prereqs)
                {
                    let pair = (pidx, ldep as usize);
                    if reachable_set.insert(pair) {
                        queue.push_back(pair);
                    }
                }
                for &edep in mdesc
                    .external_module_deps
                    .iter()
                    .chain(&mdesc.external_init_prereqs)
                {
                    let imp_mod = self.packages[pidx].artifact.imports.module_dependencies
                        [edep as usize]
                        .clone();
                    let target_pidx = self.resolve_dep_pkg_idx(pidx, imp_mod.package_index)?;
                    let &target_midx = self.packages[target_pidx]
                        .mod_by_path
                        .get(&imp_mod.module_path)
                        .ok_or_else(|| {
                            ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                                "module linker",
                                format!(
                                    "missing module `{}` in `{}`",
                                    imp_mod.module_path.join("."),
                                    self.packages[target_pidx].artifact.exports.qualified_name
                                ),
                            ))
                        })?;
                    let pair = (target_pidx, target_midx);
                    if reachable_set.insert(pair) {
                        queue.push_back(pair);
                    }
                }

                let ext_refs = self.scan_module_external_refs(pidx, midx)?;
                self.active_traits.extend(ext_refs.active_traits);
                self.active_methods.extend(ext_refs.active_methods);

                for imp_fidx in ext_refs.imported_functions {
                    let imp_fn = self.packages[pidx].artifact.imports.functions[imp_fidx].clone();
                    let target_pidx = self.resolve_dep_pkg_idx(pidx, imp_fn.package_index)?;
                    let &sym_idx = self.packages[target_pidx]
                        .sym_by_selector
                        .get(&imp_fn.selector)
                        .ok_or_else(|| {
                            ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                                "module linker",
                                format!("missing exported function {:?}", imp_fn.selector),
                            ))
                        })?;
                    let owner_midx = self.packages[target_pidx].artifact.exports.symbols[sym_idx]
                        .owner_module_index as usize;
                    if reachable_set.insert((target_pidx, owner_midx)) {
                        queue.push_back((target_pidx, owner_midx));
                    }
                }
                for imp_gidx in ext_refs.imported_globals {
                    let imp_glob = self.packages[pidx].artifact.imports.globals[imp_gidx].clone();
                    let target_pidx = self.resolve_dep_pkg_idx(pidx, imp_glob.package_index)?;
                    let sel = SymbolLinkSelector::ScopedSymbol {
                        module_path: imp_glob.module_path.clone(),
                        owner_type_id: imp_glob.owner_type_id,
                        name: imp_glob.name.clone(),
                    };
                    let &sym_idx = self.packages[target_pidx]
                        .sym_by_selector
                        .get(&sel)
                        .ok_or_else(|| {
                            ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                                "module linker",
                                format!("missing exported global {sel:?}"),
                            ))
                        })?;
                    let owner_midx = self.packages[target_pidx].artifact.exports.symbols[sym_idx]
                        .owner_module_index as usize;
                    if reachable_set.insert((target_pidx, owner_midx)) {
                        queue.push_back((target_pidx, owner_midx));
                    }
                }
            }

            // Demand-driven dispatch closure: pull in modules owning `MethodSlot`s or `VTable`s
            // whose receiver/implementor type is live AND whose method name or trait is active!
            let mut newly_dispatched = Vec::new();
            for cand_pidx in 0..self.packages.len() {
                let pkg_active = self.packages[cand_pidx].artifact.exports.is_privileged_std
                    || self.packages[cand_pidx].pool_mapping.is_some()
                    || reachable_set.iter().any(|&(p, _)| p == cand_pidx);
                if !pkg_active {
                    continue;
                }
                let mut demanded_fids = Vec::new();
                {
                    let pool = &self.packages[cand_pidx].artifact.type_pool;
                    for vtable in pool.vtables_snapshot() {
                        let Some(canon_tr) = pool.canonical_type(vtable.trait_type) else {
                            continue;
                        };
                        let tr_id = pool.get(canon_tr).type_id;
                        if self.active_traits.contains(&tr_id)
                            && self.is_type_live(pool, vtable.implementor, &reachable_set)
                        {
                            demanded_fids.extend(vtable.entries.iter().copied());
                        }
                    }
                    for rec in pool.trait_impls_snapshot() {
                        let Some(canon_tr) = pool.canonical_type(rec.trait_type) else {
                            continue;
                        };
                        let tr_id = pool.get(canon_tr).type_id;
                        if self.active_traits.contains(&tr_id)
                            && self.is_type_live(pool, rec.implementor, &reachable_set)
                        {
                            demanded_fids.extend(rec.methods.iter().map(|m| m.func_id));
                        }
                    }
                    for raw_ty in 0..pool.len() {
                        let ty = TypeIndex::from_raw(raw_ty as u32);
                        let slots = pool.methods_of(ty);
                        if slots.is_empty() || !self.is_type_live(pool, ty, &reachable_set) {
                            continue;
                        }
                        for slot in slots {
                            let trait_active = slot
                                .trait_impl
                                .and_then(|tr| pool.canonical_type(tr))
                                .is_some_and(|ctr| {
                                    self.active_traits.contains(&pool.get(ctr).type_id)
                                });
                            if self.active_methods.contains(&slot.name) || trait_active {
                                demanded_fids.push(slot.func_id);
                            }
                        }
                    }
                }
                for fid in demanded_fids {
                    let owner = self.resolve_pool_func_owner(cand_pidx, fid)?;
                    if !reachable_set.contains(&owner)
                        && self.packages[owner.0].module_states[owner.1]
                            != ModuleLoadState::Initialized
                    {
                        newly_dispatched.push(owner);
                    }
                }
            }
            for pair in newly_dispatched {
                if reachable_set.insert(pair) {
                    queue.push_back(pair);
                }
            }
            if queue.is_empty() {
                break;
            }
        }

        let newly_loading: Vec<(usize, usize)> = reachable_set
            .iter()
            .copied()
            .filter(|&(p, m)| self.packages[p].module_states[m] == ModuleLoadState::Unloaded)
            .collect();

        // 2. Merge `TypePool` & append WASM modules for any package touched for the first time.
        let mut touched_pkgs: BTreeSet<usize> = BTreeSet::new();
        for &(p, _) in &newly_loading {
            touched_pkgs.insert(p);
        }
        for &pidx in &touched_pkgs {
            if self.packages[pidx].pool_mapping.is_none() {
                let mapping = vm
                    .merge_type_pool(&self.packages[pidx].artifact.type_pool)
                    .map_err(|e| {
                        ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                            "TypePool merge",
                            e.to_string(),
                        ))
                    })?;
                if !self.packages[pidx].artifact.wasm_modules.is_empty() {
                    let remapped_wasm: Vec<WasmModuleDef> = self.packages[pidx]
                        .artifact
                        .wasm_modules
                        .iter()
                        .map(|wm| WasmModuleDef {
                            name: wm.name.clone(),
                            wasm_bytes: wm.wasm_bytes.clone(),
                            bindings: wm
                                .bindings
                                .iter()
                                .map(|b| WasmBindingDef {
                                    nessa_name: b.nessa_name.clone(),
                                    wasm_export: b.wasm_export.clone(),
                                    signature: mapping.map_type(b.signature).unwrap_or(b.signature),
                                })
                                .collect(),
                        })
                        .collect();
                    let wbase = vm
                        .append_wasm_modules(&remapped_wasm)
                        .map_err(ArtifactLoadError::Runtime)?;
                    self.packages[pidx].wasm_binding_base = wbase;
                }
                self.packages[pidx].pool_mapping = Some(mapping);
            }
            self.verify_collection_roles(vm, pidx)?;
        }

        // 3. Pre-allocate VM `GlobalId`s & VM `FuncId`s for all newly loading modules.
        let batch_start_fid = vm.function_count() as u32;
        let mut next_vm_fid = batch_start_fid;
        for &(pidx, midx) in &newly_loading {
            let mapping = self.packages[pidx].pool_mapping.as_ref().unwrap();
            let owned_globals = self.packages[pidx].artifact.exports.modules[midx]
                .owned_globals
                .clone();
            if !owned_globals.is_empty() {
                let mut schema = Vec::with_capacity(owned_globals.len());
                for &gid in &owned_globals {
                    let ginfo = self.packages[pidx].artifact.codegen_output.globals[gid.0 as usize];
                    let mapped_ty = mapping.map_type(ginfo.type_index).ok_or_else(|| {
                        ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                            "global relocation",
                            "global TypeIndex out of range",
                        ))
                    })?;
                    schema.push(GlobalInfo {
                        type_index: mapped_ty,
                        is_mutable: ginfo.is_mutable,
                    });
                }
                let gbase = vm
                    .extend_globals(&schema)
                    .map_err(ArtifactLoadError::Runtime)?;
                for (offset, &gid) in owned_globals.iter().enumerate() {
                    self.packages[pidx].local_global_to_vm[gid.0 as usize] =
                        Some(GlobalId(gbase + offset as u32));
                }
            }

            let owned_fns = self.packages[pidx].artifact.exports.modules[midx]
                .owned_functions
                .clone();
            for &fid in &owned_fns {
                self.packages[pidx].local_fn_to_vm[fid.0 as usize] = Some(FuncId(next_vm_fid));
                next_vm_fid += 1;
            }
        }

        // 4. Resolve imported `FuncId`s and `GlobalId`s across all merged packages.
        for pidx in 0..self.packages.len() {
            if self.packages[pidx].pool_mapping.is_none() {
                continue;
            }
            let imp_fn_len = self.packages[pidx].artifact.imports.functions.len();
            for i in 0..imp_fn_len {
                if self.packages[pidx].imported_fn_to_vm[i].is_some() {
                    continue;
                }
                let imp_fn = self.packages[pidx].artifact.imports.functions[i].clone();
                let Ok(target_pidx) = self.resolve_dep_pkg_idx(pidx, imp_fn.package_index) else {
                    continue;
                };
                if let Some(&sym_idx) = self.packages[target_pidx]
                    .sym_by_selector
                    .get(&imp_fn.selector)
                    && let ExportTarget::Function { func_id, .. } =
                        &self.packages[target_pidx].artifact.exports.symbols[sym_idx].target
                    && let Some(vm_fid) =
                        self.packages[target_pidx].local_fn_to_vm[func_id.0 as usize]
                {
                    self.packages[pidx].imported_fn_to_vm[i] = Some(vm_fid);
                }
            }

            let imp_glob_len = self.packages[pidx].artifact.imports.globals.len();
            for i in 0..imp_glob_len {
                if self.packages[pidx].imported_global_to_vm[i].is_some() {
                    continue;
                }
                let imp_glob = self.packages[pidx].artifact.imports.globals[i].clone();
                let Ok(target_pidx) = self.resolve_dep_pkg_idx(pidx, imp_glob.package_index) else {
                    continue;
                };
                let sel = SymbolLinkSelector::ScopedSymbol {
                    module_path: imp_glob.module_path.clone(),
                    owner_type_id: imp_glob.owner_type_id,
                    name: imp_glob.name.clone(),
                };
                if let Some(&sym_idx) = self.packages[target_pidx].sym_by_selector.get(&sel)
                    && let ExportTarget::Global { global_id, .. } =
                        &self.packages[target_pidx].artifact.exports.symbols[sym_idx].target
                    && let Some(vm_gid) =
                        self.packages[target_pidx].local_global_to_vm[global_id.0 as usize]
                {
                    self.packages[pidx].imported_global_to_vm[i] = Some(vm_gid);
                }
            }
        }

        // 5. Publish newly loaded `MethodSlot`s, `TraitImplRecord`s, and `VTable`s into `vm`'s `TypePool`.
        for pidx in 0..self.packages.len() {
            let pkg = &mut self.packages[pidx];
            let Some(mapping) = pkg.pool_mapping.as_ref() else {
                continue;
            };
            let local_fn_count = pkg.local_fn_to_vm.len();
            let mut func_map_snapshot: HashMap<u32, u32> = HashMap::new();
            for (idx, opt_fid) in pkg.local_fn_to_vm.iter().enumerate() {
                if let Some(fid) = opt_fid {
                    func_map_snapshot.insert(idx as u32, fid.0);
                }
            }
            for (idx, opt_fid) in pkg.imported_fn_to_vm.iter().enumerate() {
                if let Some(fid) = opt_fid {
                    func_map_snapshot.insert((local_fn_count + idx) as u32, fid.0);
                }
            }
            vm.install_merged_type_dispatch(
                &pkg.artifact.type_pool,
                mapping,
                &func_map_snapshot,
                &mut pkg.dispatch_state,
            )
            .map_err(|e| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "TypePool dispatch merge",
                    e.to_string(),
                ))
            })?;
        }

        // 6. Relocate bytecode instructions, ABIs, constants, and `MethodCallScope`s & install into `vm`.
        let mut all_new_scopes: Vec<MethodCallScope> = Vec::new();
        for &(pidx, midx) in &newly_loading {
            let owned_fns = self.packages[pidx].artifact.exports.modules[midx]
                .owned_functions
                .clone();
            for arch_fid in owned_fns {
                let (fn_code, mut fn_scopes) = self.relocate_function(vm, pidx, arch_fid)?;
                let installed_id = vm.add_function(fn_code);
                debug_assert_eq!(
                    Some(installed_id),
                    self.packages[pidx].local_fn_to_vm[arch_fid.0 as usize]
                );
                all_new_scopes.append(&mut fn_scopes);
            }
            self.packages[pidx].module_states[midx] = ModuleLoadState::LoadedUninitialized;
        }
        if batch_start_fid < next_vm_fid {
            vm.append_method_call_scopes(all_new_scopes, batch_start_fid..next_vm_fid)
                .map_err(ArtifactLoadError::Runtime)?;
        }

        // 7. Compute prerequisite-topological initialization order over all uninitialized reachable modules.
        let uninit_mods: BTreeSet<(usize, usize)> = reachable_set
            .iter()
            .copied()
            .filter(|&(p, m)| self.packages[p].module_states[m] != ModuleLoadState::Initialized)
            .collect();

        let ordered_mods = self.order_module_initializers(&uninit_mods)?;
        let mut pending_initializers = Vec::new();
        let mut pending_modules = Vec::new();
        for (pidx, midx) in ordered_mods {
            let mdesc = &self.packages[pidx].artifact.exports.modules[midx];
            for &arch_init_fid in &mdesc.scope_initializers {
                let vm_fid = self.packages[pidx].local_fn_to_vm[arch_init_fid.0 as usize]
                    .expect("loaded module scope_initializer has VM FuncId");
                pending_initializers.push(vm_fid);
            }
            pending_modules.push((
                self.packages[pidx].artifact.exports.qualified_name.clone(),
                mdesc.module_path.clone(),
            ));
        }

        let newly_loaded_modules = newly_loading
            .iter()
            .map(|&(p, m)| {
                (
                    self.packages[p].artifact.exports.qualified_name.clone(),
                    self.packages[p].artifact.exports.modules[m]
                        .module_path
                        .clone(),
                )
            })
            .collect();

        let main_func = self.packages[root_pidx].artifact.exports.modules[root_midx]
            .main_func
            .and_then(|arch_fid| self.packages[root_pidx].local_fn_to_vm[arch_fid.0 as usize]);

        Ok(PreparedModuleLoad {
            pending_initializers,
            pending_modules,
            newly_loaded_modules,
            main_func,
        })
    }

    /// Mark `modules` `(qualified_name, module_path)` as [`ModuleLoadState::Initialized`].
    pub fn mark_modules_initialized(&mut self, modules: &[(String, Vec<String>)]) {
        for (qname, mpath) in modules {
            if let Some(&pidx) = self.pkg_by_qname.get(qname)
                && let Some(&midx) = self.packages[pidx].mod_by_path.get(mpath)
            {
                self.packages[pidx].module_states[midx] = ModuleLoadState::Initialized;
            }
        }
    }

    /// Load `(pkg_qname, module_path)` and run any pending `__init__` functions synchronously on `vm`.
    pub fn load_module(
        &mut self,
        vm: &mut Vm,
        pkg_qname: &str,
        module_path: &[String],
    ) -> Result<PreparedModuleLoad, ArtifactLoadError> {
        let prepared = self.prepare_module(vm, pkg_qname, module_path)?;
        for &init_fid in &prepared.pending_initializers {
            vm.spawn_root(init_fid);
            if let VmResult::Error(err) = vm.run() {
                return Err(ArtifactLoadError::Runtime(err));
            }
        }
        self.mark_modules_initialized(&prepared.pending_modules);
        Ok(prepared)
    }

    /// Synthesize a zero-parameter entry trampoline in `vm` that executes `pending_initializers`
    /// in order followed by `main_func` (ideal for host event loops like `drive_vm_loop`).
    pub fn synthesize_entry_sequence(
        &mut self,
        vm: &mut Vm,
        prepared: &PreparedModuleLoad,
    ) -> Result<Option<FuncId>, ArtifactLoadError> {
        if prepared.pending_initializers.is_empty() {
            self.mark_modules_initialized(&prepared.pending_modules);
            return Ok(prepared.main_func);
        }
        let mut instrs = Vec::with_capacity(prepared.pending_initializers.len() + 2);
        for &fid in &prepared.pending_initializers {
            if fid.0 < (1 << 14) {
                instrs.push(Instruction::call(fid.0, 0).encode());
            } else {
                let cidx = self.alloc_narrow14(vm, Constant::UInt(u64::from(fid.0)))?;
                instrs.push(Instruction::call_far(cidx, 0).encode());
            }
        }
        if let Some(main_fid) = prepared.main_func {
            if main_fid.0 < (1 << 14) {
                instrs.push(Instruction::call(main_fid.0, 0).encode());
            } else {
                let cidx = self.alloc_narrow14(vm, Constant::UInt(u64::from(main_fid.0)))?;
                instrs.push(Instruction::call_far(cidx, 0).encode());
            }
            instrs.push(Instruction::ret(Reg(0)).encode());
        } else {
            instrs.push(Instruction::return_unit().encode());
        }

        let entry_fid = FuncId(vm.function_count() as u32);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: entry_fid,
            instructions: instrs,
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: TypeIndex::INVALID,
        });
        self.mark_modules_initialized(&prepared.pending_modules);
        Ok(Some(entry_fid))
    }

    fn verify_collection_roles(&self, vm: &Vm, pidx: usize) -> Result<(), ArtifactLoadError> {
        let builtins = &self.packages[pidx].artifact.builtins;
        let pool = vm.type_pool();
        if builtins.iter().any(|b| builtin_requires_list_roles(b.id))
            && (pool.list_type().is_none() || pool.list_buffer_type().is_none())
        {
            return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "native imports",
                "List imports require List and Buffer roles",
            )));
        }
        if builtins.iter().any(|b| builtin_requires_map_roles(b.id))
            && (pool.map_type().is_none() || pool.map_buffer_type().is_none())
        {
            return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "native imports",
                "Map imports require Map and MapBuffer roles",
            )));
        }
        if builtins.iter().any(|b| builtin_requires_bytes_roles(b.id))
            && (pool.bytes_type().is_none() || pool.bytes_buffer_type().is_none())
        {
            return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                "native imports",
                "Bytes imports require Bytes and BytesBuffer roles",
            )));
        }
        Ok(())
    }

    fn resolve_pool_func_owner(
        &self,
        pidx: usize,
        fid: u32,
    ) -> Result<(usize, usize), ArtifactLoadError> {
        let pkg = &self.packages[pidx];
        let local_count = pkg.local_fn_owner_mod.len() as u32;
        if fid < local_count {
            Ok((pidx, pkg.local_fn_owner_mod[fid as usize]))
        } else {
            let imp_idx = (fid - local_count) as usize;
            let imp_fn = pkg.artifact.imports.functions.get(imp_idx).ok_or_else(|| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "module linker",
                    format!("out-of-range dispatch function ID #{fid}"),
                ))
            })?;
            let target_pidx = self.resolve_dep_pkg_idx(pidx, imp_fn.package_index)?;
            let &sym_idx = self.packages[target_pidx]
                .sym_by_selector
                .get(&imp_fn.selector)
                .ok_or_else(|| {
                    ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                        "module linker",
                        format!("missing exported dispatch target {:?}", imp_fn.selector),
                    ))
                })?;
            let owner_midx = self.packages[target_pidx].artifact.exports.symbols[sym_idx]
                .owner_module_index as usize;
            Ok((target_pidx, owner_midx))
        }
    }

    fn is_type_live(
        &self,
        pool: &TypePool,
        ty: TypeIndex,
        reachable_set: &BTreeSet<(usize, usize)>,
    ) -> bool {
        let Some(canon) = pool.canonical_type(ty) else {
            return false;
        };
        let info = pool.get(canon);
        if let Some(&(op, om)) = self.nominal_type_owners.get(&info.type_id) {
            return self.packages[op].module_states[om] != ModuleLoadState::Unloaded
                || reachable_set.contains(&(op, om));
        }
        match &info.kind {
            TypeKind::Optional { inner } | TypeKind::Future { inner } => {
                self.is_type_live(pool, *inner, reachable_set)
            }
            TypeKind::Tuple { elements } => elements
                .iter()
                .all(|&el| self.is_type_live(pool, el, reachable_set)),
            TypeKind::ErrorQualified { errors, inner } => {
                self.is_type_live(pool, *inner, reachable_set)
                    && errors
                        .iter()
                        .all(|&e| self.is_type_live(pool, e, reachable_set))
            }
            TypeKind::EffectQualified { inner, .. } => {
                self.is_type_live(pool, *inner, reachable_set)
            }
            _ => true,
        }
    }

    fn collect_trait_and_ancestors(
        pool: &TypePool,
        trait_ty: TypeIndex,
        out: &mut HashSet<TypeId>,
    ) {
        let Some(start) = pool.canonical_type(trait_ty) else {
            return;
        };
        let mut stack = vec![start];
        let mut visited = HashSet::new();
        while let Some(cur) = stack.pop() {
            if !visited.insert(cur) {
                continue;
            }
            let info = pool.get(cur);
            if info.type_id != TypeId::ZERO {
                out.insert(info.type_id);
            }
            if let TypeKind::Trait { parents, .. } = &info.kind {
                for &p in parents {
                    if let Some(cp) = pool.canonical_type(p) {
                        stack.push(cp);
                    }
                }
            }
        }
    }

    fn scan_module_external_refs(
        &self,
        pidx: usize,
        midx: usize,
    ) -> Result<ModuleExternalRefs, ArtifactLoadError> {
        let pkg = &self.packages[pidx];
        let pool = &pkg.artifact.type_pool;
        let local_fn_count = pkg.artifact.codegen_output.functions.len() as u32;
        let local_glob_count = pkg.artifact.codegen_output.globals.len() as u32;
        let constants = &pkg.artifact.codegen_output.constants;
        let mut imp_fns = BTreeSet::new();
        let mut imp_globs = BTreeSet::new();
        let mut traits = HashSet::new();
        let mut methods = HashSet::new();

        for &arch_fid in &pkg.artifact.exports.modules[midx].owned_functions {
            let fidx = pkg.local_fn_by_id[&arch_fid.0];
            let cf = &pkg.artifact.codegen_output.functions[fidx];
            if cf.display_owner.is_some() {
                Self::collect_trait_and_ancestors(pool, pool.well_known.display, &mut traits);
            }
            for cap in &cf.abi.captures {
                if let CaptureAbi::TraitProof { view } = *cap {
                    Self::collect_trait_and_ancestors(pool, view, &mut traits);
                }
            }
            for param in &cf.abi.parameters {
                match *param {
                    ParameterAbi::Value => {}
                    ParameterAbi::Trait { view } | ParameterAbi::TraitSelf { view } => {
                        Self::collect_trait_and_ancestors(pool, view, &mut traits);
                    }
                }
            }
            for &word in &cf.instructions {
                let Some(instr) = Instruction::decode(word) else {
                    continue;
                };
                match (instr.opcode, instr.data) {
                    (
                        Opcode::TraitProof | Opcode::TraitAssert | Opcode::TraitProject,
                        InstructionData::A { imm12, .. },
                    ) => {
                        let view_opt = match instr.amode {
                            AddrMode::Imm => Some(TypeIndex::from_raw(u32::from(imm12))),
                            AddrMode::Const => match constants.get(usize::from(imm12)) {
                                Some(Constant::Type(ty)) => Some(*ty),
                                _ => None,
                            },
                        };
                        if let Some(view) = view_opt {
                            Self::collect_trait_and_ancestors(pool, view, &mut traits);
                        }
                    }
                    (Opcode::CallMethod, InstructionData::C { payload }) => {
                        let (_, _, mraw) = Instruction::c_call_method(payload);
                        methods.insert(str_interner::StrId::from_raw(mraw));
                    }
                    (Opcode::CallMethodFar, InstructionData::C { payload }) => {
                        let (_, _, cidx) = Instruction::c_call_method(payload);
                        if let Some(Constant::UInt(v)) = constants.get(cidx as usize) {
                            methods.insert(str_interner::StrId::from_raw(*v as u32));
                        }
                    }
                    (Opcode::Call, InstructionData::C { payload }) => {
                        let target = payload & 0x3FFF;
                        if target >= local_fn_count {
                            imp_fns.insert((target - local_fn_count) as usize);
                        }
                    }
                    (Opcode::CallFar, InstructionData::C { payload }) => {
                        let cidx = (payload & 0x3FFF) as usize;
                        if let Some(Constant::UInt(v)) = constants.get(cidx)
                            && (*v as u32) >= local_fn_count
                        {
                            imp_fns.insert((*v as u32 - local_fn_count) as usize);
                        }
                    }
                    (Opcode::NewClosure, InstructionData::A { imm12, .. }) => {
                        let target = u32::from(imm12);
                        if target >= local_fn_count {
                            imp_fns.insert((target - local_fn_count) as usize);
                        }
                    }
                    (Opcode::NewClosureWide, InstructionData::A { imm12, .. }) => {
                        let cidx = usize::from(imm12);
                        if let Some(Constant::UInt(v)) = constants.get(cidx)
                            && (*v as u32) >= local_fn_count
                        {
                            imp_fns.insert((*v as u32 - local_fn_count) as usize);
                        }
                    }
                    (
                        Opcode::LoadGlobal | Opcode::StoreGlobal,
                        InstructionData::A { imm12, .. },
                    ) => {
                        let gid = u32::from(imm12);
                        if gid >= local_glob_count {
                            imp_globs.insert((gid - local_glob_count) as usize);
                        }
                    }
                    (
                        Opcode::LoadGlobalWide | Opcode::StoreGlobalWide,
                        InstructionData::A { base, imm12, .. },
                    ) => {
                        let gid = (u32::from(base.0) << 12) | u32::from(imm12);
                        if gid >= local_glob_count {
                            imp_globs.insert((gid - local_glob_count) as usize);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(ModuleExternalRefs {
            imported_functions: imp_fns,
            imported_globals: imp_globs,
            active_traits: traits,
            active_methods: methods,
        })
    }

    fn relocate_function(
        &mut self,
        vm: &mut Vm,
        pidx: usize,
        arch_fid: FuncId,
    ) -> Result<(FunctionCode, Vec<MethodCallScope>), ArtifactLoadError> {
        let (
            cf_instructions,
            cf_display_owner,
            cf_function_type,
            cf_abi,
            cf_register_count,
            cf_param_count,
            cf_is_closure,
        ) = {
            let pkg = &self.packages[pidx];
            let fidx = pkg.local_fn_by_id[&arch_fid.0];
            let src = &pkg.artifact.codegen_output.functions[fidx];
            (
                src.instructions.clone(),
                src.display_owner,
                src.function_type,
                src.abi.clone(),
                src.register_count,
                src.param_count,
                src.is_closure,
            )
        };
        let vm_fid = self.packages[pidx].local_fn_to_vm[arch_fid.0 as usize]
            .expect("pre-allocated VM FuncId");

        let map_type = |loader: &Self, ty: TypeIndex| -> Result<TypeIndex, ArtifactLoadError> {
            if ty == TypeIndex::INVALID {
                return Ok(TypeIndex::INVALID);
            }
            loader.packages[pidx]
                .pool_mapping
                .as_ref()
                .unwrap()
                .map_type(ty)
                .ok_or_else(|| {
                    ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                        "bytecode relocation",
                        format!("TypeIndex {} out of range", ty.as_u32()),
                    ))
                })
        };

        let map_func = |loader: &Self, fid: u32| -> Result<u32, ArtifactLoadError> {
            let pkg = &loader.packages[pidx];
            let lcount = pkg.local_fn_to_vm.len();
            let idx = fid as usize;
            let resolved = if idx < lcount {
                pkg.local_fn_to_vm[idx]
            } else {
                pkg.imported_fn_to_vm.get(idx - lcount).copied().flatten()
            };
            resolved.map(|f| f.0).ok_or_else(|| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "bytecode relocation",
                    format!(
                        "unlinked function ID #{fid} in `{}`",
                        pkg.artifact.exports.qualified_name
                    ),
                ))
            })
        };

        let map_global = |loader: &Self, gid: u32| -> Result<u32, ArtifactLoadError> {
            let pkg = &loader.packages[pidx];
            let lcount = pkg.local_global_to_vm.len();
            let idx = gid as usize;
            let resolved = if idx < lcount {
                pkg.local_global_to_vm[idx]
            } else {
                pkg.imported_global_to_vm
                    .get(idx - lcount)
                    .copied()
                    .flatten()
            };
            resolved.map(|g| g.0).ok_or_else(|| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "bytecode relocation",
                    format!("unlinked global ID #{gid}"),
                ))
            })
        };

        let map_const_val = |loader: &Self, cidx: usize| -> Result<Constant, ArtifactLoadError> {
            let raw = loader.packages[pidx]
                .artifact
                .codegen_output
                .constants
                .get(cidx)
                .ok_or_else(|| {
                    ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                        "bytecode relocation",
                        "constant index out of range",
                    ))
                })?
                .clone();
            Ok(match raw {
                Constant::Type(ty) => Constant::Type(map_type(loader, ty)?),
                Constant::Enum {
                    type_index,
                    variant,
                } => Constant::Enum {
                    type_index: map_type(loader, type_index)?,
                    variant,
                },
                other => other,
            })
        };

        let mut relocated_instrs = Vec::with_capacity(cf_instructions.len());
        for &word in &cf_instructions {
            let instr = Instruction::decode(word).ok_or_else(|| {
                ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                    "bytecode relocation",
                    "failed to decode instruction word",
                ))
            })?;
            let new_word = match (instr.opcode, instr.amode, instr.data) {
                (Opcode::Load, AddrMode::Const, InstructionData::A { dst, imm12, .. }) => {
                    let cval = map_const_val(self, usize::from(imm12))?;
                    let vm_cidx = self.alloc_wide17(vm, cval)?;
                    if vm_cidx < 4096 {
                        Instruction::load_const(dst, vm_cidx as u16).encode()
                    } else {
                        Instruction::load_const_wide(dst, vm_cidx).encode()
                    }
                }
                (Opcode::LoadConstWide, _, InstructionData::A { dst, base, imm12 }) => {
                    let src_cidx = (usize::from(base.0) << 12) | usize::from(imm12);
                    let cval = map_const_val(self, src_cidx)?;
                    let vm_cidx = self.alloc_wide17(vm, cval)?;
                    Instruction::load_const_wide(dst, vm_cidx).encode()
                }
                (
                    op @ (Opcode::TypeCheck
                    | Opcode::TypeCast
                    | Opcode::TypeCastSafe
                    | Opcode::TypeAssert
                    | Opcode::NewObject
                    | Opcode::TraitProof
                    | Opcode::TraitAssert
                    | Opcode::TraitProject),
                    am,
                    InstructionData::A { dst, base, imm12 },
                ) => {
                    let src_ty = match am {
                        AddrMode::Imm => TypeIndex::from_raw(u32::from(imm12)),
                        AddrMode::Const => match map_const_val(self, usize::from(imm12))? {
                            Constant::Type(ty) => ty,
                            _ => {
                                return Err(ArtifactLoadError::InvalidMetadata(
                                    ArtifactError::new(
                                        "bytecode relocation",
                                        "Const-mode type operand is not Constant::Type",
                                    ),
                                ));
                            }
                        },
                    };
                    let vm_ty = map_type(self, src_ty)?;
                    if vm_ty.as_u32() < 4096 {
                        Instruction::a_type(op, AddrMode::Imm, dst, base, vm_ty.as_u32() as u16)
                            .encode()
                    } else {
                        let cidx = self.alloc_narrow12(vm, Constant::Type(vm_ty))?;
                        Instruction::a_type(op, AddrMode::Const, dst, base, cidx).encode()
                    }
                }
                (
                    op @ (Opcode::NewEnum | Opcode::EnumIs | Opcode::ErrorOk | Opcode::ErrorErr),
                    am,
                    InstructionData::A { dst, base, imm12 },
                ) => {
                    let cval = map_const_val(self, usize::from(imm12))?;
                    let narrow_idx = self.alloc_narrow12(vm, cval)?;
                    Instruction::a_type(op, am, dst, base, narrow_idx).encode()
                }
                (Opcode::LoadGlobal, _, InstructionData::A { dst, imm12, .. }) => {
                    let vm_gid = map_global(self, u32::from(imm12))?;
                    if vm_gid < 4096 {
                        Instruction::load_global(dst, vm_gid as u16).encode()
                    } else {
                        Instruction::load_global_wide(dst, vm_gid).encode()
                    }
                }
                (Opcode::LoadGlobalWide, _, InstructionData::A { dst, base, imm12 }) => {
                    let src_gid = (u32::from(base.0) << 12) | u32::from(imm12);
                    let vm_gid = map_global(self, src_gid)?;
                    Instruction::load_global_wide(dst, vm_gid).encode()
                }
                (Opcode::StoreGlobal, _, InstructionData::A { dst, imm12, .. }) => {
                    let vm_gid = map_global(self, u32::from(imm12))?;
                    if vm_gid < 4096 {
                        Instruction::store_global(vm_gid as u16, dst).encode()
                    } else {
                        Instruction::store_global_wide(vm_gid, dst).encode()
                    }
                }
                (Opcode::StoreGlobalWide, _, InstructionData::A { dst, base, imm12 }) => {
                    let src_gid = (u32::from(base.0) << 12) | u32::from(imm12);
                    let vm_gid = map_global(self, src_gid)?;
                    Instruction::store_global_wide(vm_gid, dst).encode()
                }
                (Opcode::NewClosure, _, InstructionData::A { dst, base, imm12 }) => {
                    let vm_target = map_func(self, u32::from(imm12))?;
                    if vm_target < 4096 {
                        Instruction::new_closure(dst, vm_target, base.0).encode()
                    } else {
                        let cidx = self.alloc_narrow12(vm, Constant::UInt(u64::from(vm_target)))?;
                        Instruction::new_closure_wide(dst, cidx, base.0).encode()
                    }
                }
                (Opcode::NewClosureWide, _, InstructionData::A { dst, base, imm12 }) => {
                    let Constant::UInt(raw_fid) = map_const_val(self, usize::from(imm12))? else {
                        return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                            "bytecode relocation",
                            "NewClosureWide constant is not UInt",
                        )));
                    };
                    let vm_target = map_func(self, raw_fid as u32)?;
                    let cidx = self.alloc_narrow12(vm, Constant::UInt(u64::from(vm_target)))?;
                    Instruction::new_closure_wide(dst, cidx, base.0).encode()
                }
                (Opcode::Call, _, InstructionData::C { payload }) => {
                    let argc = ((payload >> 14) & 0xFF) as u8;
                    let src_fid = payload & 0x3FFF;
                    let vm_target = map_func(self, src_fid)?;
                    if vm_target < (1 << 14) {
                        Instruction::call(vm_target, argc).encode()
                    } else {
                        let cidx = self.alloc_narrow14(vm, Constant::UInt(u64::from(vm_target)))?;
                        Instruction::call_far(cidx, argc).encode()
                    }
                }
                (Opcode::CallFar, _, InstructionData::C { payload }) => {
                    let argc = ((payload >> 14) & 0xFF) as u8;
                    let src_cidx = (payload & 0x3FFF) as usize;
                    let Constant::UInt(raw_fid) = map_const_val(self, src_cidx)? else {
                        return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                            "bytecode relocation",
                            "CallFar constant is not UInt",
                        )));
                    };
                    let vm_target = map_func(self, raw_fid as u32)?;
                    if vm_target < (1 << 14) {
                        Instruction::call(vm_target, argc).encode()
                    } else {
                        let cidx = self.alloc_narrow14(vm, Constant::UInt(u64::from(vm_target)))?;
                        Instruction::call_far(cidx, argc).encode()
                    }
                }
                (Opcode::CallMethodFar, _, InstructionData::C { payload }) => {
                    let argc = ((payload >> 17) & 0x1F) as u8;
                    let recv = Reg(((payload >> 12) & 0x1F) as u8);
                    let src_cidx = (payload & 0xFFF) as usize;
                    let cval = map_const_val(self, src_cidx)?;
                    let cidx = self.alloc_narrow12(vm, cval)?;
                    Instruction::call_method_far(recv, cidx, argc).encode()
                }
                (Opcode::CallWasm, _, InstructionData::C { payload }) => {
                    let argc = ((payload >> 14) & 0xFF) as u8;
                    let local_bid = payload & 0x3FFF;
                    let vm_bid = self.packages[pidx].wasm_binding_base + local_bid;
                    Instruction::call_wasm(vm_bid, argc).encode()
                }
                (Opcode::EffectCallDyn, _, InstructionData::E { payload }) => {
                    let argc = ((payload >> 17) & 0x1F) as u8;
                    let src_eff = TypeIndex::from_raw(payload & 0x1FFFF);
                    let vm_eff = map_type(self, src_eff)?;
                    Instruction::effect_call(vm_eff, argc).encode()
                }
                (Opcode::PushHandlerClosure, _, InstructionData::E { payload }) => {
                    let closure_reg = Reg(((payload >> 17) & 0x1F) as u8);
                    let src_eff = TypeIndex::from_raw(payload & 0x1FFFF);
                    let vm_eff = map_type(self, src_eff)?;
                    Instruction::push_handler_closure(vm_eff, closure_reg).encode()
                }
                (Opcode::PushCapturingHandler, _, InstructionData::E { payload }) => {
                    let closure_reg = Reg(((payload >> 17) & 0x1F) as u8);
                    let src_cidx = (payload & 0x1FFFF) as usize;
                    let Constant::UInt(packed) = map_const_val(self, src_cidx)? else {
                        return Err(ArtifactLoadError::InvalidMetadata(ArtifactError::new(
                            "bytecode relocation",
                            "PushCapturingHandler constant is not UInt",
                        )));
                    };
                    let src_eff = TypeIndex::from_raw((packed & 0xFFFF_FFFF) as u32);
                    let cont_pos = packed >> 32;
                    let vm_eff = map_type(self, src_eff)?;
                    let new_packed = (cont_pos << 32) | u64::from(vm_eff.as_u32());
                    let vm_cidx = self.alloc_wide17(vm, Constant::UInt(new_packed))?;
                    Instruction::push_capturing_handler(closure_reg, vm_cidx).encode()
                }
                _ => word,
            };
            relocated_instrs.push(new_word);
        }

        let display_owner = match cf_display_owner {
            Some(ty) => Some(map_type(self, ty)?),
            None => None,
        };
        let function_type = map_type(self, cf_function_type)?;
        let abi = FunctionAbi {
            captures: cf_abi
                .captures
                .iter()
                .map(|c| match *c {
                    CaptureAbi::Value => Ok(CaptureAbi::Value),
                    CaptureAbi::TraitProof { view } => Ok(CaptureAbi::TraitProof {
                        view: map_type(self, view)?,
                    }),
                })
                .collect::<Result<Vec<_>, ArtifactLoadError>>()?,
            parameters: cf_abi
                .parameters
                .iter()
                .map(|p| match *p {
                    ParameterAbi::Value => Ok(ParameterAbi::Value),
                    ParameterAbi::Trait { view } => Ok(ParameterAbi::Trait {
                        view: map_type(self, view)?,
                    }),
                    ParameterAbi::TraitSelf { view } => Ok(ParameterAbi::TraitSelf {
                        view: map_type(self, view)?,
                    }),
                })
                .collect::<Result<Vec<_>, ArtifactLoadError>>()?,
        };

        let mut fn_scopes = Vec::new();
        if let Some(raw_scopes) = self.packages[pidx].scopes_by_fn.get(&arch_fid.0) {
            let mapping = self.packages[pidx].pool_mapping.as_ref().unwrap();
            for &(pc, src_sc) in raw_scopes {
                fn_scopes.push(MethodCallScope {
                    func_id: vm_fid,
                    pc,
                    scope: mapping.map_scope(src_sc),
                });
            }
        }

        Ok((
            FunctionCode {
                display_owner,
                abi,
                func_id: vm_fid,
                instructions: relocated_instrs,
                register_count: cf_register_count,
                param_count: cf_param_count,
                is_closure: cf_is_closure,
                function_type,
            },
            fn_scopes,
        ))
    }

    fn order_module_initializers(
        &self,
        uninit: &BTreeSet<(usize, usize)>,
    ) -> Result<Vec<(usize, usize)>, ArtifactLoadError> {
        let mut hard_prereqs: HashMap<(usize, usize), BTreeSet<(usize, usize)>> = HashMap::new();
        let mut soft_deps: HashMap<(usize, usize), BTreeSet<(usize, usize)>> = HashMap::new();

        for &(pidx, midx) in uninit {
            let mdesc = &self.packages[pidx].artifact.exports.modules[midx];
            let mut hard = BTreeSet::new();
            let mut soft = BTreeSet::new();

            for &ldep in &mdesc.local_module_deps {
                let target = (pidx, ldep as usize);
                if uninit.contains(&target) {
                    soft.insert(target);
                }
            }
            for &edep in &mdesc.external_module_deps {
                let imp_m =
                    &self.packages[pidx].artifact.imports.module_dependencies[edep as usize];
                let tp = self.resolve_dep_pkg_idx(pidx, imp_m.package_index)?;
                let tm = self.packages[tp].mod_by_path[&imp_m.module_path];
                if uninit.contains(&(tp, tm)) {
                    soft.insert((tp, tm));
                }
            }

            // Hard prerequisites include direct prereqs AND transitive load deps of external prereqs
            // (so calling an external function whose body reads another module's global in that package
            // always initializes that backing module first!).
            let mut prereq_roots = Vec::new();
            for &lp in &mdesc.local_init_prereqs {
                let target = (pidx, lp as usize);
                if uninit.contains(&target) {
                    hard.insert(target);
                }
            }
            for &ep in &mdesc.external_init_prereqs {
                let imp_m = &self.packages[pidx].artifact.imports.module_dependencies[ep as usize];
                let tp = self.resolve_dep_pkg_idx(pidx, imp_m.package_index)?;
                let tm = self.packages[tp].mod_by_path[&imp_m.module_path];
                prereq_roots.push((tp, tm));
            }
            let mut visited_ext = HashSet::new();
            while let Some((tp, tm)) = prereq_roots.pop() {
                if (tp, tm) == (pidx, midx) || !visited_ext.insert((tp, tm)) {
                    continue;
                }
                if uninit.contains(&(tp, tm)) {
                    hard.insert((tp, tm));
                }
                let tdesc = &self.packages[tp].artifact.exports.modules[tm];
                for &ld in tdesc
                    .local_module_deps
                    .iter()
                    .chain(&tdesc.local_init_prereqs)
                {
                    prereq_roots.push((tp, ld as usize));
                }
                for &ed in tdesc
                    .external_module_deps
                    .iter()
                    .chain(&tdesc.external_init_prereqs)
                {
                    let imp_m =
                        &self.packages[tp].artifact.imports.module_dependencies[ed as usize];
                    let ntp = self.resolve_dep_pkg_idx(tp, imp_m.package_index)?;
                    let ntm = self.packages[ntp].mod_by_path[&imp_m.module_path];
                    prereq_roots.push((ntp, ntm));
                }
            }

            hard_prereqs.insert((pidx, midx), hard);
            soft_deps.insert((pidx, midx), soft);
        }

        let mut remaining = uninit.clone();
        let mut order = Vec::with_capacity(uninit.len());
        while !remaining.is_empty() {
            let ready: Vec<(usize, usize)> = remaining
                .iter()
                .copied()
                .filter(|m| !hard_prereqs[m].iter().any(|t| remaining.contains(t)))
                .collect();
            if ready.is_empty() {
                return Err(ArtifactLoadError::Runtime(VmError::InvalidArgument(
                    "cyclic cross-package module initialization dependency",
                )));
            }
            let chosen = ready
                .iter()
                .copied()
                .find(|m| !soft_deps[m].iter().any(|t| remaining.contains(t)))
                .unwrap_or(ready[0]);
            remaining.remove(&chosen);
            order.push(chosen);
        }
        Ok(order)
    }
}

impl Default for ModuleLoader {
    fn default() -> Self {
        Self::new()
    }
}
