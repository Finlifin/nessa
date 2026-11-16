//! Sandboxed WebAssembly execution engine (`WasmInstance`).
//!
//! Uses an explicit heap-allocated frame stack (`Vec<WasmFrame>`) rather than recursive Rust
//! calls so `return_call` / `return_call_indirect` execute in constant space and deep guest
//! call chains never risk overflowing the host OS thread stack.

use std::collections::HashMap;
use std::sync::Arc;

use crate::module::{
    BranchTarget, ConstInit, DataSegmentKind, ElemSegmentKind, ExportItem, MemImmediate,
    MemoryPlan, TablePlan, WasmModule, WasmOp,
};
use crate::{WasmConfig, WasmError, WasmTrap, WasmVal, WasmValType};

pub const WASM_PAGE_BYTES: usize = 65_536;

/// View into the active instance's linear memory passed to synchronous host callbacks.
pub struct WasmCaller<'a> {
    memories: &'a mut [Vec<u8>],
    max_pages: &'a [u32],
}

impl<'a> WasmCaller<'a> {
    #[inline]
    pub fn memory(&self, index: usize) -> Option<&[u8]> {
        self.memories.get(index).map(Vec::as_slice)
    }

    #[inline]
    pub fn memory_mut(&mut self, index: usize) -> Option<&mut [u8]> {
        self.memories.get_mut(index).map(Vec::as_mut_slice)
    }

    pub fn read_bytes(&self, offset: u32, len: u32) -> Result<&[u8], WasmTrap> {
        let mem = self
            .memories
            .first()
            .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
        let start = offset as usize;
        let end = start
            .checked_add(len as usize)
            .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
        if end > mem.len() {
            return Err(WasmTrap::OutOfBoundsMemoryAccess);
        }
        Ok(&mem[start..end])
    }

    pub fn write_bytes(&mut self, offset: u32, src: &[u8]) -> Result<(), WasmTrap> {
        let mem = self
            .memories
            .first_mut()
            .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
        let start = offset as usize;
        let end = start
            .checked_add(src.len())
            .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
        if end > mem.len() {
            return Err(WasmTrap::OutOfBoundsMemoryAccess);
        }
        mem[start..end].copy_from_slice(src);
        Ok(())
    }

    pub fn read_u32_le(&self, offset: u32) -> Result<u32, WasmTrap> {
        let bytes = self.read_bytes(offset, 4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn write_u32_le(&mut self, offset: u32, val: u32) -> Result<(), WasmTrap> {
        self.write_bytes(offset, &val.to_le_bytes())
    }

    pub fn write_u64_le(&mut self, offset: u32, val: u64) -> Result<(), WasmTrap> {
        self.write_bytes(offset, &val.to_le_bytes())
    }

    pub fn memory_grow(&mut self, mem_idx: usize, delta_pages: u32) -> i32 {
        grow_memory_vec(self.memories, self.max_pages, mem_idx, delta_pages)
    }
}

pub type HostCallback =
    Box<dyn FnMut(&mut WasmCaller<'_>, &[WasmVal]) -> Result<Vec<WasmVal>, WasmTrap> + Send>;

pub type HostFuncMap = HashMap<(String, String), HostCallback>;

pub type HostDispatcher<'d> =
    dyn FnMut(&mut WasmCaller<'_>, &str, &str, &[WasmVal]) -> Result<Vec<WasmVal>, WasmTrap> + 'd;

#[derive(Clone)]
struct TableInstance {
    plan: TablePlan,
    elements: Vec<WasmVal>,
}

#[derive(Clone)]
struct GlobalInstance {
    val_type: WasmValType,
    mutable: bool,
    value: WasmVal,
}

struct WasmFrame {
    def_idx: usize,
    pc: usize,
    locals_base: usize,
    values_base: usize,
    labels_base: usize,
    result_arity: usize,
}

/// Live instantiated WebAssembly module with isolated linear memory, tables, and globals.
pub struct WasmInstance {
    module: Arc<WasmModule>,
    config: WasmConfig,
    fuel_remaining: Option<u64>,
    memories: Vec<Vec<u8>>,
    mem_max_pages: Vec<u32>,
    tables: Vec<TableInstance>,
    globals: Vec<GlobalInstance>,
    passive_data: Vec<Option<Vec<u8>>>,
    passive_elems: Vec<Option<Vec<WasmVal>>>,
    host_funcs: Vec<Option<HostCallback>>,
    // Reusable execution stacks across calls (zero per-call allocation once warm).
    value_stack: Vec<WasmVal>,
    locals_stack: Vec<WasmVal>,
    label_stack: Vec<u32>,
    call_stack: Vec<WasmFrame>,
}

impl WasmInstance {
    pub fn instantiate(
        module: Arc<WasmModule>,
        config: WasmConfig,
        host_imports: HostFuncMap,
        imported_globals: &HashMap<(String, String), WasmVal>,
    ) -> Result<Self, WasmError> {
        Self::instantiate_with_dynamic_imports(
            module,
            config,
            host_imports,
            &std::collections::HashSet::new(),
            imported_globals,
            None,
        )
    }

    pub fn instantiate_with_dynamic_imports(
        module: Arc<WasmModule>,
        config: WasmConfig,
        mut host_imports: HostFuncMap,
        dynamic_imports: &std::collections::HashSet<(String, String)>,
        imported_globals: &HashMap<(String, String), WasmVal>,
        start_dispatcher: Option<&mut HostDispatcher<'_>>,
    ) -> Result<Self, WasmError> {
        if let Some(m) = module.mem_imports.first() {
            return Err(WasmError::LinkError(format!(
                "external memory import `{}::{}` is not supported; export memory from the wasm module",
                m.module, m.name
            )));
        }
        if let Some(t) = module.table_imports.first() {
            return Err(WasmError::LinkError(format!(
                "external table import `{}::{}` is not supported; export tables from the wasm module",
                t.module, t.name
            )));
        }

        let mut host_funcs = Vec::with_capacity(module.func_imports.len());
        for imp in &module.func_imports {
            let key = (imp.module.clone(), imp.name.clone());
            if let Some(cb) = host_imports.remove(&key) {
                host_funcs.push(Some(cb));
            } else if dynamic_imports.contains(&key) {
                host_funcs.push(None);
            } else {
                return Err(WasmError::LinkError(format!(
                    "missing required host function import `{}::{}`",
                    imp.module, imp.name
                )));
            }
        }

        let mut globals =
            Vec::with_capacity(module.global_imports.len() + module.defined_globals.len());
        for g_imp in &module.global_imports {
            let key = (g_imp.module.clone(), g_imp.name.clone());
            let val = imported_globals.get(&key).copied().ok_or_else(|| {
                WasmError::LinkError(format!(
                    "missing required global import `{}::{}`",
                    g_imp.module, g_imp.name
                ))
            })?;
            if val.val_type() != g_imp.val_type {
                return Err(WasmError::SignatureMismatch(format!(
                    "global import `{}::{}` expected {}, got {}",
                    g_imp.module,
                    g_imp.name,
                    g_imp.val_type,
                    val.val_type()
                )));
            }
            globals.push(GlobalInstance {
                val_type: g_imp.val_type,
                mutable: g_imp.mutable,
                value: val,
            });
        }
        for g_def in &module.defined_globals {
            let init_val = eval_const_init(&g_def.init, &globals)?;
            globals.push(GlobalInstance {
                val_type: g_def.val_type,
                mutable: g_def.mutable,
                value: init_val,
            });
        }

        let mut memories = Vec::with_capacity(module.defined_memories.len());
        let mut mem_max_pages = Vec::with_capacity(module.defined_memories.len());
        for &MemoryPlan {
            min_pages,
            max_pages,
        } in &module.defined_memories
        {
            if min_pages > config.max_memory_pages {
                return Err(WasmError::MemoryLimitExceeded(format!(
                    "initial memory ({min_pages} pages) exceeds sandbox limit ({} pages)",
                    config.max_memory_pages
                )));
            }
            let eff_max = max_pages
                .map(|m| m.min(config.max_memory_pages))
                .unwrap_or(config.max_memory_pages);
            let byte_len = (min_pages as usize)
                .checked_mul(WASM_PAGE_BYTES)
                .ok_or_else(|| WasmError::MemoryLimitExceeded("memory size overflow".into()))?;
            memories.push(vec![0u8; byte_len]);
            mem_max_pages.push(eff_max);
        }

        let mut tables = Vec::with_capacity(module.defined_tables.len());
        for &plan in &module.defined_tables {
            if plan.min > 65_536 {
                return Err(WasmError::MemoryLimitExceeded(format!(
                    "initial table size ({}) exceeds 65536 elements",
                    plan.min
                )));
            }
            tables.push(TableInstance {
                plan,
                elements: vec![WasmVal::default_for(plan.element); plan.min as usize],
            });
        }

        // Initialize element segments.
        let mut passive_elems = Vec::with_capacity(module.elems.len());
        for elem in &module.elems {
            let mut vals = Vec::with_capacity(elem.items.len());
            for item in &elem.items {
                vals.push(eval_const_init(item, &globals)?);
            }
            match &elem.kind {
                ElemSegmentKind::Passive => passive_elems.push(Some(vals)),
                ElemSegmentKind::Declared => passive_elems.push(None),
                ElemSegmentKind::Active {
                    table_index,
                    offset,
                } => {
                    let off_val = eval_const_init(offset, &globals)?;
                    let WasmVal::I32(off_i32) = off_val else {
                        return Err(WasmError::Validation("non-i32 element offset".into()));
                    };
                    let off = off_i32 as u32 as usize;
                    let tbl = tables
                        .get_mut(*table_index as usize)
                        .ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    let end = off
                        .checked_add(vals.len())
                        .ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    if end > tbl.elements.len() {
                        return Err(WasmTrap::OutOfBoundsTableAccess.into());
                    }
                    tbl.elements[off..end].copy_from_slice(&vals);
                    passive_elems.push(None);
                }
            }
        }

        // Initialize data segments.
        let mut passive_data = Vec::with_capacity(module.data.len());
        for seg in &module.data {
            match &seg.kind {
                DataSegmentKind::Passive => passive_data.push(Some(seg.bytes.clone())),
                DataSegmentKind::Active {
                    memory_index,
                    offset,
                } => {
                    let off_val = eval_const_init(offset, &globals)?;
                    let WasmVal::I32(off_i32) = off_val else {
                        return Err(WasmError::Validation("non-i32 data offset".into()));
                    };
                    let off = off_i32 as u32 as usize;
                    let mem = memories
                        .get_mut(*memory_index as usize)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    let end = off
                        .checked_add(seg.bytes.len())
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    if end > mem.len() {
                        return Err(WasmTrap::OutOfBoundsMemoryAccess.into());
                    }
                    mem[off..end].copy_from_slice(&seg.bytes);
                    passive_data.push(None);
                }
            }
        }

        let mut inst = Self {
            module,
            config,
            fuel_remaining: config.fuel_limit,
            memories,
            mem_max_pages,
            tables,
            globals,
            passive_data,
            passive_elems,
            host_funcs,
            value_stack: Vec::with_capacity(256),
            locals_stack: Vec::with_capacity(256),
            label_stack: Vec::with_capacity(64),
            call_stack: Vec::with_capacity(64),
        };

        if let Some(start_idx) = inst.module.start_func {
            inst.invoke_func_by_index(start_idx, &[], start_dispatcher)?;
        }

        Ok(inst)
    }

    pub fn module(&self) -> &Arc<WasmModule> {
        &self.module
    }

    pub fn fuel_remaining(&self) -> Option<u64> {
        self.fuel_remaining
    }

    pub fn set_fuel(&mut self, fuel: Option<u64>) {
        self.fuel_remaining = fuel;
    }

    pub fn memory_pages(&self) -> u32 {
        self.memories
            .first()
            .map(|m| (m.len() / WASM_PAGE_BYTES) as u32)
            .unwrap_or(0)
    }

    pub fn memory_bytes(&self) -> Option<&[u8]> {
        self.memories.first().map(Vec::as_slice)
    }

    pub fn memory_grow(&mut self, delta_pages: u32) -> Result<u32, WasmError> {
        if self.memories.is_empty() {
            return Err(WasmError::UnknownExport("memory".into()));
        }
        let prev = grow_memory_vec(&mut self.memories, &self.mem_max_pages, 0, delta_pages);
        if prev < 0 {
            Err(WasmError::MemoryLimitExceeded(format!(
                "memory.grow({delta_pages}) denied by sandbox or module maximum"
            )))
        } else {
            Ok(prev as u32)
        }
    }

    pub fn read_memory(&self, offset: usize, len: usize) -> Result<&[u8], WasmError> {
        let mem = self
            .memories
            .first()
            .ok_or_else(|| WasmError::UnknownExport("memory".into()))?;
        let end = offset
            .checked_add(len)
            .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
        if end > mem.len() {
            return Err(WasmTrap::OutOfBoundsMemoryAccess.into());
        }
        Ok(&mem[offset..end])
    }

    pub fn write_memory(&mut self, offset: usize, data: &[u8]) -> Result<(), WasmError> {
        let mem = self
            .memories
            .first_mut()
            .ok_or_else(|| WasmError::UnknownExport("memory".into()))?;
        let end = offset
            .checked_add(data.len())
            .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
        if end > mem.len() {
            return Err(WasmTrap::OutOfBoundsMemoryAccess.into());
        }
        mem[offset..end].copy_from_slice(data);
        Ok(())
    }

    pub fn get_global(&self, export_name: &str) -> Result<WasmVal, WasmError> {
        let rec = self
            .module
            .exports
            .iter()
            .find(|e| e.name == export_name)
            .ok_or_else(|| WasmError::UnknownExport(export_name.to_owned()))?;
        let ExportItem::Global(idx) = rec.item else {
            return Err(WasmError::UnknownExport(export_name.to_owned()));
        };
        Ok(self.globals[idx as usize].value)
    }

    pub fn set_global(&mut self, export_name: &str, val: WasmVal) -> Result<(), WasmError> {
        let rec = self
            .module
            .exports
            .iter()
            .find(|e| e.name == export_name)
            .ok_or_else(|| WasmError::UnknownExport(export_name.to_owned()))?;
        let ExportItem::Global(idx) = rec.item else {
            return Err(WasmError::UnknownExport(export_name.to_owned()));
        };
        let g = &mut self.globals[idx as usize];
        if !g.mutable {
            return Err(WasmError::ImmutableGlobal(export_name.to_owned()));
        }
        if g.val_type != val.val_type() {
            return Err(WasmError::SignatureMismatch(format!(
                "global `{export_name}` expected {}, got {}",
                g.val_type,
                val.val_type()
            )));
        }
        g.value = val;
        Ok(())
    }

    /// Call an exported function by name using only the instance's pre-bound host callbacks.
    pub fn call_export(
        &mut self,
        export_name: &str,
        args: &[WasmVal],
    ) -> Result<Vec<WasmVal>, WasmError> {
        self.call_export_with_dispatcher(export_name, args, None)
    }

    /// Call an exported function with an optional dynamic host dispatcher `(module, name, args)`
    /// (used by Nessa's `std.wasm` to invoke GC-rooted Nessa closures safely without storing `&mut Vm`).
    pub fn call_export_with_dispatcher(
        &mut self,
        export_name: &str,
        args: &[WasmVal],
        dispatcher: Option<&mut HostDispatcher<'_>>,
    ) -> Result<Vec<WasmVal>, WasmError> {
        let rec = self
            .module
            .exports
            .iter()
            .find(|e| e.name == export_name)
            .ok_or_else(|| WasmError::UnknownExport(export_name.to_owned()))?;
        let ExportItem::Func(func_idx) = rec.item else {
            return Err(WasmError::UnknownExport(export_name.to_owned()));
        };
        let sig = self.module.func_sig(func_idx);
        if sig.params.len() != args.len() {
            return Err(WasmError::SignatureMismatch(format!(
                "export `{export_name}` expects {} arguments, got {}",
                sig.params.len(),
                args.len()
            )));
        }
        for (i, (expected, actual)) in sig.params.iter().zip(args.iter()).enumerate() {
            if *expected != actual.val_type() {
                return Err(WasmError::SignatureMismatch(format!(
                    "argument {} of `{export_name}` expected {}, got {}",
                    i + 1,
                    expected,
                    actual.val_type()
                )));
            }
        }
        self.invoke_func_by_index(func_idx, args, dispatcher)
    }

    fn reset_stacks(&mut self) {
        self.value_stack.clear();
        self.locals_stack.clear();
        self.label_stack.clear();
        self.call_stack.clear();
    }

    fn invoke_func_by_index(
        &mut self,
        entry_func_idx: u32,
        args: &[WasmVal],
        mut dispatcher: Option<&mut HostDispatcher<'_>>,
    ) -> Result<Vec<WasmVal>, WasmError> {
        self.reset_stacks();

        let num_imports = self.module.func_imports.len();
        if (entry_func_idx as usize) < num_imports {
            return self
                .call_imported_func(entry_func_idx as usize, args, dispatcher.as_deref_mut())
                .map_err(WasmError::Trap);
        }

        let def_idx = entry_func_idx as usize - num_imports;
        self.push_guest_frame(def_idx, args)?;
        let run_res = self.run_loop(dispatcher);
        if let Err(trap) = run_res {
            self.reset_stacks();
            return Err(WasmError::Trap(trap));
        }
        let out = std::mem::take(&mut self.value_stack);
        self.reset_stacks();
        Ok(out)
    }

    fn call_imported_func(
        &mut self,
        imp_idx: usize,
        args: &[WasmVal],
        dispatcher: Option<&mut HostDispatcher<'_>>,
    ) -> Result<Vec<WasmVal>, WasmTrap> {
        let expected_results =
            &self.module.types[self.module.func_imports[imp_idx].type_idx as usize].results;
        let results = if let Some(mut cb) = self.host_funcs[imp_idx].take() {
            let mut caller = WasmCaller {
                memories: &mut self.memories,
                max_pages: &self.mem_max_pages,
            };
            let res = cb(&mut caller, args);
            self.host_funcs[imp_idx] = Some(cb);
            res?
        } else if let Some(disp) = dispatcher {
            let imp = &self.module.func_imports[imp_idx];
            let mut caller = WasmCaller {
                memories: &mut self.memories,
                max_pages: &self.mem_max_pages,
            };
            disp(&mut caller, &imp.module, &imp.name, args)?
        } else {
            return Err(WasmTrap::HostTrap("unbound host import".into()));
        };

        if results.len() != expected_results.len()
            || results
                .iter()
                .zip(expected_results.iter())
                .any(|(v, ty)| v.val_type() != *ty)
        {
            return Err(WasmTrap::HostTrap(
                "host import returned mismatched result signature".into(),
            ));
        }
        Ok(results)
    }

    fn push_guest_frame(&mut self, def_idx: usize, args: &[WasmVal]) -> Result<(), WasmTrap> {
        if self.call_stack.len() >= self.config.max_call_depth {
            return Err(WasmTrap::CallStackExhausted);
        }
        let func = &self.module.defined_funcs[def_idx];
        let sig = &self.module.types[func.type_idx as usize];
        let locals_base = self.locals_stack.len();
        self.locals_stack.extend_from_slice(args);
        for &ty in &func.extra_locals {
            self.locals_stack.push(WasmVal::default_for(ty));
        }
        let values_base = self.value_stack.len();
        let labels_base = self.label_stack.len();
        self.label_stack.push(values_base as u32);
        self.call_stack.push(WasmFrame {
            def_idx,
            pc: 0,
            locals_base,
            values_base,
            labels_base,
            result_arity: sig.results.len(),
        });
        Ok(())
    }

    fn run_loop(
        &mut self,
        mut dispatcher: Option<&mut HostDispatcher<'_>>,
    ) -> Result<(), WasmTrap> {
        let module = Arc::clone(&self.module);
        let num_imports = module.func_imports.len();
        let mut arg_scratch: Vec<WasmVal> = Vec::new();

        while let Some(frame_idx) = self.call_stack.len().checked_sub(1) {
            if let Some(ref mut fuel) = self.fuel_remaining {
                if *fuel == 0 {
                    return Err(WasmTrap::OutOfFuel);
                }
                *fuel -= 1;
            }

            let (def_idx, pc, locals_base, values_base, labels_base, result_arity) = {
                let f = &mut self.call_stack[frame_idx];
                let cur_pc = f.pc;
                f.pc += 1;
                (
                    f.def_idx,
                    cur_pc,
                    f.locals_base,
                    f.values_base,
                    f.labels_base,
                    f.result_arity,
                )
            };

            let op = &module.defined_funcs[def_idx].ops[pc];
            match op {
                WasmOp::Unreachable => return Err(WasmTrap::Unreachable),
                WasmOp::Nop => {}
                WasmOp::EnterBlock { param_arity } => {
                    let base = self.value_stack.len() - (*param_arity as usize);
                    self.label_stack.push(base as u32);
                }
                WasmOp::IfJump {
                    param_arity,
                    false_pc,
                    pop_on_false,
                } => {
                    let cond = pop_i32(&mut self.value_stack);
                    let base = self.value_stack.len() - (*param_arity as usize);
                    if cond != 0 {
                        self.label_stack.push(base as u32);
                    } else {
                        if !*pop_on_false {
                            self.label_stack.push(base as u32);
                        }
                        self.call_stack[frame_idx].pc = *false_pc as usize;
                    }
                }
                WasmOp::ElseJump { end_after_pc } => {
                    self.label_stack.pop();
                    self.call_stack[frame_idx].pc = *end_after_pc as usize;
                }
                WasmOp::ExitBlock => {
                    self.label_stack.pop();
                }
                WasmOp::Br(bt) => {
                    exec_branch(
                        *bt,
                        &mut self.value_stack,
                        &mut self.label_stack,
                        &mut self.call_stack[frame_idx].pc,
                    );
                }
                WasmOp::BrIf(bt) => {
                    let cond = pop_i32(&mut self.value_stack);
                    if cond != 0 {
                        exec_branch(
                            *bt,
                            &mut self.value_stack,
                            &mut self.label_stack,
                            &mut self.call_stack[frame_idx].pc,
                        );
                    }
                }
                WasmOp::BrTable(tbl) => {
                    let idx = pop_i32(&mut self.value_stack) as u32 as usize;
                    let bt = tbl.targets.get(idx).copied().unwrap_or(tbl.default);
                    exec_branch(
                        bt,
                        &mut self.value_stack,
                        &mut self.label_stack,
                        &mut self.call_stack[frame_idx].pc,
                    );
                }
                WasmOp::Return => {
                    let top_start = self.value_stack.len() - result_arity;
                    if top_start != values_base {
                        self.value_stack
                            .copy_within(top_start..top_start + result_arity, values_base);
                    }
                    self.value_stack.truncate(values_base + result_arity);
                    self.locals_stack.truncate(locals_base);
                    self.label_stack.truncate(labels_base);
                    self.call_stack.pop();
                }
                WasmOp::Call(callee_idx) => {
                    let c_idx = *callee_idx as usize;
                    let param_len = module.func_sig(*callee_idx).params.len();
                    let arg_start = self.value_stack.len() - param_len;
                    arg_scratch.clear();
                    arg_scratch.extend_from_slice(&self.value_stack[arg_start..]);
                    self.value_stack.truncate(arg_start);
                    if c_idx < num_imports {
                        let res = self.call_imported_func(
                            c_idx,
                            &arg_scratch,
                            dispatcher.as_deref_mut(),
                        )?;
                        self.value_stack.extend_from_slice(&res);
                    } else {
                        self.push_guest_frame(c_idx - num_imports, &arg_scratch)?;
                    }
                }
                WasmOp::CallIndirect {
                    type_idx,
                    table_idx,
                } => {
                    let elem_idx = pop_i32(&mut self.value_stack) as u32 as usize;
                    let callee_idx = resolve_indirect_callee(
                        &module,
                        &self.tables,
                        *table_idx as usize,
                        elem_idx,
                        *type_idx,
                    )?;
                    let c_idx = callee_idx as usize;
                    let param_len = module.types[*type_idx as usize].params.len();
                    let arg_start = self.value_stack.len() - param_len;
                    arg_scratch.clear();
                    arg_scratch.extend_from_slice(&self.value_stack[arg_start..]);
                    self.value_stack.truncate(arg_start);
                    if c_idx < num_imports {
                        let res = self.call_imported_func(
                            c_idx,
                            &arg_scratch,
                            dispatcher.as_deref_mut(),
                        )?;
                        self.value_stack.extend_from_slice(&res);
                    } else {
                        self.push_guest_frame(c_idx - num_imports, &arg_scratch)?;
                    }
                }
                WasmOp::ReturnCall(callee_idx) => {
                    let c_idx = *callee_idx as usize;
                    let param_len = module.func_sig(*callee_idx).params.len();
                    let arg_start = self.value_stack.len() - param_len;
                    arg_scratch.clear();
                    arg_scratch.extend_from_slice(&self.value_stack[arg_start..]);
                    // Tear down current frame before tail-calling!
                    self.value_stack.truncate(values_base);
                    self.locals_stack.truncate(locals_base);
                    self.label_stack.truncate(labels_base);
                    self.call_stack.pop();
                    if c_idx < num_imports {
                        let res = self.call_imported_func(
                            c_idx,
                            &arg_scratch,
                            dispatcher.as_deref_mut(),
                        )?;
                        self.value_stack.extend_from_slice(&res);
                    } else {
                        self.push_guest_frame(c_idx - num_imports, &arg_scratch)?;
                    }
                }
                WasmOp::ReturnCallIndirect {
                    type_idx,
                    table_idx,
                } => {
                    let elem_idx = pop_i32(&mut self.value_stack) as u32 as usize;
                    let callee_idx = resolve_indirect_callee(
                        &module,
                        &self.tables,
                        *table_idx as usize,
                        elem_idx,
                        *type_idx,
                    )?;
                    let c_idx = callee_idx as usize;
                    let param_len = module.types[*type_idx as usize].params.len();
                    let arg_start = self.value_stack.len() - param_len;
                    arg_scratch.clear();
                    arg_scratch.extend_from_slice(&self.value_stack[arg_start..]);
                    self.value_stack.truncate(values_base);
                    self.locals_stack.truncate(locals_base);
                    self.label_stack.truncate(labels_base);
                    self.call_stack.pop();
                    if c_idx < num_imports {
                        let res = self.call_imported_func(
                            c_idx,
                            &arg_scratch,
                            dispatcher.as_deref_mut(),
                        )?;
                        self.value_stack.extend_from_slice(&res);
                    } else {
                        self.push_guest_frame(c_idx - num_imports, &arg_scratch)?;
                    }
                }
                WasmOp::Drop => {
                    self.value_stack.pop();
                }
                WasmOp::Select => {
                    let c = pop_i32(&mut self.value_stack);
                    let v2 = self.value_stack.pop().unwrap();
                    let v1 = self.value_stack.pop().unwrap();
                    self.value_stack.push(if c != 0 { v1 } else { v2 });
                }
                WasmOp::LocalGet(idx) => {
                    let val = self.locals_stack[locals_base + *idx as usize];
                    self.value_stack.push(val);
                }
                WasmOp::LocalSet(idx) => {
                    let val = self.value_stack.pop().unwrap();
                    self.locals_stack[locals_base + *idx as usize] = val;
                }
                WasmOp::LocalTee(idx) => {
                    let val = *self.value_stack.last().unwrap();
                    self.locals_stack[locals_base + *idx as usize] = val;
                }
                WasmOp::GlobalGet(idx) => {
                    self.value_stack.push(self.globals[*idx as usize].value);
                }
                WasmOp::GlobalSet(idx) => {
                    let val = self.value_stack.pop().unwrap();
                    self.globals[*idx as usize].value = val;
                }

                WasmOp::TableGet(t_idx) => {
                    let i = pop_i32(&mut self.value_stack) as u32 as usize;
                    let tbl = &self.tables[*t_idx as usize];
                    let val = *tbl
                        .elements
                        .get(i)
                        .ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    self.value_stack.push(val);
                }
                WasmOp::TableSet(t_idx) => {
                    let val = self.value_stack.pop().unwrap();
                    let i = pop_i32(&mut self.value_stack) as u32 as usize;
                    let tbl = &mut self.tables[*t_idx as usize];
                    let slot = tbl
                        .elements
                        .get_mut(i)
                        .ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    *slot = val;
                }
                WasmOp::TableSize(t_idx) => {
                    let sz = self.tables[*t_idx as usize].elements.len() as i32;
                    self.value_stack.push(WasmVal::I32(sz));
                }
                WasmOp::TableGrow(t_idx) => {
                    let n = pop_i32(&mut self.value_stack) as u32 as usize;
                    let init = self.value_stack.pop().unwrap();
                    let tbl = &mut self.tables[*t_idx as usize];
                    let cur = tbl.elements.len();
                    let max = tbl.plan.max.unwrap_or(65_536) as usize;
                    let res = match cur
                        .checked_add(n)
                        .filter(|&new_len| new_len <= max && new_len <= 65_536)
                    {
                        Some(new_len) => {
                            tbl.elements.resize(new_len, init);
                            cur as i32
                        }
                        None => -1,
                    };
                    self.value_stack.push(WasmVal::I32(res));
                }
                WasmOp::TableFill(t_idx) => {
                    let n = pop_i32(&mut self.value_stack) as u32 as usize;
                    let val = self.value_stack.pop().unwrap();
                    let i = pop_i32(&mut self.value_stack) as u32 as usize;
                    let tbl = &mut self.tables[*t_idx as usize];
                    let end = i.checked_add(n).ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    if end > tbl.elements.len() {
                        return Err(WasmTrap::OutOfBoundsTableAccess);
                    }
                    tbl.elements[i..end].fill(val);
                }
                WasmOp::TableCopy {
                    dst_table,
                    src_table,
                } => {
                    let n = pop_i32(&mut self.value_stack) as u32 as usize;
                    let s = pop_i32(&mut self.value_stack) as u32 as usize;
                    let d = pop_i32(&mut self.value_stack) as u32 as usize;
                    let s_end = s.checked_add(n).ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    let d_end = d.checked_add(n).ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    if s_end > self.tables[*src_table as usize].elements.len()
                        || d_end > self.tables[*dst_table as usize].elements.len()
                    {
                        return Err(WasmTrap::OutOfBoundsTableAccess);
                    }
                    if dst_table == src_table {
                        self.tables[*dst_table as usize]
                            .elements
                            .copy_within(s..s_end, d);
                    } else {
                        let tmp = self.tables[*src_table as usize].elements[s..s_end].to_vec();
                        self.tables[*dst_table as usize].elements[d..d_end].copy_from_slice(&tmp);
                    }
                }
                WasmOp::TableInit {
                    elem_idx,
                    table_idx,
                } => {
                    let n = pop_i32(&mut self.value_stack) as u32 as usize;
                    let s = pop_i32(&mut self.value_stack) as u32 as usize;
                    let d = pop_i32(&mut self.value_stack) as u32 as usize;
                    let seg = self.passive_elems[*elem_idx as usize]
                        .as_deref()
                        .unwrap_or(&[]);
                    let s_end = s.checked_add(n).ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    let d_end = d.checked_add(n).ok_or(WasmTrap::OutOfBoundsTableAccess)?;
                    if s_end > seg.len() || d_end > self.tables[*table_idx as usize].elements.len()
                    {
                        return Err(WasmTrap::OutOfBoundsTableAccess);
                    }
                    self.tables[*table_idx as usize].elements[d..d_end]
                        .copy_from_slice(&seg[s..s_end]);
                }
                WasmOp::ElemDrop(elem_idx) => {
                    self.passive_elems[*elem_idx as usize] = None;
                }
                WasmOp::RefNull(ty) => self.value_stack.push(WasmVal::default_for(*ty)),
                WasmOp::RefIsNull => {
                    let is_null = matches!(
                        self.value_stack.pop().unwrap(),
                        WasmVal::FuncRef(None) | WasmVal::ExternRef(None)
                    );
                    self.value_stack.push(WasmVal::I32(i32::from(is_null)));
                }
                WasmOp::RefFunc(f_idx) => {
                    self.value_stack.push(WasmVal::FuncRef(Some(*f_idx)));
                }

                WasmOp::MemorySize(m) => {
                    let pages = (self.memories[*m as usize].len() / WASM_PAGE_BYTES) as i32;
                    self.value_stack.push(WasmVal::I32(pages));
                }
                WasmOp::MemoryGrow(m) => {
                    let delta = pop_i32(&mut self.value_stack) as u32;
                    let prev = grow_memory_vec(
                        &mut self.memories,
                        &self.mem_max_pages,
                        *m as usize,
                        delta,
                    );
                    self.value_stack.push(WasmVal::I32(prev));
                }
                WasmOp::MemoryCopy { dst_mem, src_mem } => {
                    let n = pop_i32(&mut self.value_stack) as u32 as usize;
                    let s = pop_i32(&mut self.value_stack) as u32 as usize;
                    let d = pop_i32(&mut self.value_stack) as u32 as usize;
                    let s_end = s.checked_add(n).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    let d_end = d.checked_add(n).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    if s_end > self.memories[*src_mem as usize].len()
                        || d_end > self.memories[*dst_mem as usize].len()
                    {
                        return Err(WasmTrap::OutOfBoundsMemoryAccess);
                    }
                    if dst_mem == src_mem {
                        self.memories[*dst_mem as usize].copy_within(s..s_end, d);
                    } else {
                        let tmp = self.memories[*src_mem as usize][s..s_end].to_vec();
                        self.memories[*dst_mem as usize][d..d_end].copy_from_slice(&tmp);
                    }
                }
                WasmOp::MemoryFill(m) => {
                    let n = pop_i32(&mut self.value_stack) as u32 as usize;
                    let val = pop_i32(&mut self.value_stack) as u8;
                    let d = pop_i32(&mut self.value_stack) as u32 as usize;
                    let mem = &mut self.memories[*m as usize];
                    let end = d.checked_add(n).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    if end > mem.len() {
                        return Err(WasmTrap::OutOfBoundsMemoryAccess);
                    }
                    mem[d..end].fill(val);
                }
                WasmOp::MemoryInit { data_idx, mem_idx } => {
                    let n = pop_i32(&mut self.value_stack) as u32 as usize;
                    let s = pop_i32(&mut self.value_stack) as u32 as usize;
                    let d = pop_i32(&mut self.value_stack) as u32 as usize;
                    let seg = self.passive_data[*data_idx as usize]
                        .as_deref()
                        .unwrap_or(&[]);
                    let s_end = s.checked_add(n).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    let d_end = d.checked_add(n).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    if s_end > seg.len() || d_end > self.memories[*mem_idx as usize].len() {
                        return Err(WasmTrap::OutOfBoundsMemoryAccess);
                    }
                    self.memories[*mem_idx as usize][d..d_end].copy_from_slice(&seg[s..s_end]);
                }
                WasmOp::DataDrop(data_idx) => {
                    self.passive_data[*data_idx as usize] = None;
                }

                WasmOp::I32Load(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 4)?;
                    self.value_stack
                        .push(WasmVal::I32(i32::from_le_bytes(b.try_into().unwrap())));
                }
                WasmOp::I64Load(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 8)?;
                    self.value_stack
                        .push(WasmVal::I64(i64::from_le_bytes(b.try_into().unwrap())));
                }
                WasmOp::F32Load(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 4)?;
                    self.value_stack
                        .push(WasmVal::F32(u32::from_le_bytes(b.try_into().unwrap())));
                }
                WasmOp::F64Load(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 8)?;
                    self.value_stack
                        .push(WasmVal::F64(u64::from_le_bytes(b.try_into().unwrap())));
                }
                WasmOp::I32Load8S(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 1)?[0];
                    self.value_stack.push(WasmVal::I32(b as i8 as i32));
                }
                WasmOp::I32Load8U(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 1)?[0];
                    self.value_stack.push(WasmVal::I32(b as i32));
                }
                WasmOp::I32Load16S(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 2)?;
                    self.value_stack
                        .push(WasmVal::I32(i16::from_le_bytes([b[0], b[1]]) as i32));
                }
                WasmOp::I32Load16U(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 2)?;
                    self.value_stack
                        .push(WasmVal::I32(u16::from_le_bytes([b[0], b[1]]) as i32));
                }
                WasmOp::I64Load8S(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 1)?[0];
                    self.value_stack.push(WasmVal::I64(b as i8 as i64));
                }
                WasmOp::I64Load8U(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 1)?[0];
                    self.value_stack.push(WasmVal::I64(b as i64));
                }
                WasmOp::I64Load16S(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 2)?;
                    self.value_stack
                        .push(WasmVal::I64(i16::from_le_bytes([b[0], b[1]]) as i64));
                }
                WasmOp::I64Load16U(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 2)?;
                    self.value_stack
                        .push(WasmVal::I64(u16::from_le_bytes([b[0], b[1]]) as i64));
                }
                WasmOp::I64Load32S(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 4)?;
                    self.value_stack.push(WasmVal::I64(
                        i32::from_le_bytes(b.try_into().unwrap()) as i64
                    ));
                }
                WasmOp::I64Load32U(imm) => {
                    let b = load_bytes(&self.memories, *imm, &mut self.value_stack, 4)?;
                    self.value_stack.push(WasmVal::I64(
                        u32::from_le_bytes(b.try_into().unwrap()) as i64
                    ));
                }

                WasmOp::I32Store(imm) => {
                    let v = pop_i32(&mut self.value_stack).to_le_bytes();
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::I64Store(imm) => {
                    let v = pop_i64(&mut self.value_stack).to_le_bytes();
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::F32Store(imm) => {
                    let v = pop_f32(&mut self.value_stack).to_le_bytes();
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::F64Store(imm) => {
                    let v = pop_f64(&mut self.value_stack).to_le_bytes();
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::I32Store8(imm) => {
                    let v = [pop_i32(&mut self.value_stack) as u8];
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::I32Store16(imm) => {
                    let v = (pop_i32(&mut self.value_stack) as i16).to_le_bytes();
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::I64Store8(imm) => {
                    let v = [pop_i64(&mut self.value_stack) as u8];
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::I64Store16(imm) => {
                    let v = (pop_i64(&mut self.value_stack) as i16).to_le_bytes();
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }
                WasmOp::I64Store32(imm) => {
                    let v = (pop_i64(&mut self.value_stack) as i32).to_le_bytes();
                    store_bytes(&mut self.memories, *imm, &mut self.value_stack, &v)?;
                }

                WasmOp::I32Const(v) => self.value_stack.push(WasmVal::I32(*v)),
                WasmOp::I64Const(v) => self.value_stack.push(WasmVal::I64(*v)),
                WasmOp::F32Const(v) => self.value_stack.push(WasmVal::F32(*v)),
                WasmOp::F64Const(v) => self.value_stack.push(WasmVal::F64(*v)),

                WasmOp::I32Eqz => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I32(i32::from(a == 0)));
                }
                WasmOp::I32Eq => bin_i32_cmp(&mut self.value_stack, |a, b| a == b),
                WasmOp::I32Ne => bin_i32_cmp(&mut self.value_stack, |a, b| a != b),
                WasmOp::I32LtS => bin_i32_cmp(&mut self.value_stack, |a, b| a < b),
                WasmOp::I32LtU => {
                    bin_i32_cmp(&mut self.value_stack, |a, b| (a as u32) < (b as u32))
                }
                WasmOp::I32GtS => bin_i32_cmp(&mut self.value_stack, |a, b| a > b),
                WasmOp::I32GtU => {
                    bin_i32_cmp(&mut self.value_stack, |a, b| (a as u32) > (b as u32))
                }
                WasmOp::I32LeS => bin_i32_cmp(&mut self.value_stack, |a, b| a <= b),
                WasmOp::I32LeU => {
                    bin_i32_cmp(&mut self.value_stack, |a, b| (a as u32) <= (b as u32))
                }
                WasmOp::I32GeS => bin_i32_cmp(&mut self.value_stack, |a, b| a >= b),
                WasmOp::I32GeU => {
                    bin_i32_cmp(&mut self.value_stack, |a, b| (a as u32) >= (b as u32))
                }

                WasmOp::I64Eqz => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I32(i32::from(a == 0)));
                }
                WasmOp::I64Eq => bin_i64_cmp(&mut self.value_stack, |a, b| a == b),
                WasmOp::I64Ne => bin_i64_cmp(&mut self.value_stack, |a, b| a != b),
                WasmOp::I64LtS => bin_i64_cmp(&mut self.value_stack, |a, b| a < b),
                WasmOp::I64LtU => {
                    bin_i64_cmp(&mut self.value_stack, |a, b| (a as u64) < (b as u64))
                }
                WasmOp::I64GtS => bin_i64_cmp(&mut self.value_stack, |a, b| a > b),
                WasmOp::I64GtU => {
                    bin_i64_cmp(&mut self.value_stack, |a, b| (a as u64) > (b as u64))
                }
                WasmOp::I64LeS => bin_i64_cmp(&mut self.value_stack, |a, b| a <= b),
                WasmOp::I64LeU => {
                    bin_i64_cmp(&mut self.value_stack, |a, b| (a as u64) <= (b as u64))
                }
                WasmOp::I64GeS => bin_i64_cmp(&mut self.value_stack, |a, b| a >= b),
                WasmOp::I64GeU => {
                    bin_i64_cmp(&mut self.value_stack, |a, b| (a as u64) >= (b as u64))
                }

                WasmOp::F32Eq => bin_f32_cmp(&mut self.value_stack, |a, b| a == b),
                WasmOp::F32Ne => bin_f32_cmp(&mut self.value_stack, |a, b| a != b),
                WasmOp::F32Lt => bin_f32_cmp(&mut self.value_stack, |a, b| a < b),
                WasmOp::F32Gt => bin_f32_cmp(&mut self.value_stack, |a, b| a > b),
                WasmOp::F32Le => bin_f32_cmp(&mut self.value_stack, |a, b| a <= b),
                WasmOp::F32Ge => bin_f32_cmp(&mut self.value_stack, |a, b| a >= b),

                WasmOp::F64Eq => bin_f64_cmp(&mut self.value_stack, |a, b| a == b),
                WasmOp::F64Ne => bin_f64_cmp(&mut self.value_stack, |a, b| a != b),
                WasmOp::F64Lt => bin_f64_cmp(&mut self.value_stack, |a, b| a < b),
                WasmOp::F64Gt => bin_f64_cmp(&mut self.value_stack, |a, b| a > b),
                WasmOp::F64Le => bin_f64_cmp(&mut self.value_stack, |a, b| a <= b),
                WasmOp::F64Ge => bin_f64_cmp(&mut self.value_stack, |a, b| a >= b),

                WasmOp::I32Clz => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack
                        .push(WasmVal::I32(a.leading_zeros() as i32));
                }
                WasmOp::I32Ctz => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack
                        .push(WasmVal::I32(a.trailing_zeros() as i32));
                }
                WasmOp::I32Popcnt => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I32(a.count_ones() as i32));
                }
                WasmOp::I32Add => bin_i32(&mut self.value_stack, i32::wrapping_add),
                WasmOp::I32Sub => bin_i32(&mut self.value_stack, i32::wrapping_sub),
                WasmOp::I32Mul => bin_i32(&mut self.value_stack, i32::wrapping_mul),
                WasmOp::I32DivS => {
                    let b = pop_i32(&mut self.value_stack);
                    let a = pop_i32(&mut self.value_stack);
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    let (q, overflow) = a.overflowing_div(b);
                    if overflow {
                        return Err(WasmTrap::IntegerOverflow);
                    }
                    self.value_stack.push(WasmVal::I32(q));
                }
                WasmOp::I32DivU => {
                    let b = pop_i32(&mut self.value_stack) as u32;
                    let a = pop_i32(&mut self.value_stack) as u32;
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    self.value_stack.push(WasmVal::I32((a / b) as i32));
                }
                WasmOp::I32RemS => {
                    let b = pop_i32(&mut self.value_stack);
                    let a = pop_i32(&mut self.value_stack);
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    self.value_stack.push(WasmVal::I32(a.wrapping_rem(b)));
                }
                WasmOp::I32RemU => {
                    let b = pop_i32(&mut self.value_stack) as u32;
                    let a = pop_i32(&mut self.value_stack) as u32;
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    self.value_stack.push(WasmVal::I32((a % b) as i32));
                }
                WasmOp::I32And => bin_i32(&mut self.value_stack, |a, b| a & b),
                WasmOp::I32Or => bin_i32(&mut self.value_stack, |a, b| a | b),
                WasmOp::I32Xor => bin_i32(&mut self.value_stack, |a, b| a ^ b),
                WasmOp::I32Shl => bin_i32(&mut self.value_stack, |a, b| a.wrapping_shl(b as u32)),
                WasmOp::I32ShrS => bin_i32(&mut self.value_stack, |a, b| a.wrapping_shr(b as u32)),
                WasmOp::I32ShrU => bin_i32(&mut self.value_stack, |a, b| {
                    ((a as u32).wrapping_shr(b as u32)) as i32
                }),
                WasmOp::I32Rotl => bin_i32(&mut self.value_stack, |a, b| a.rotate_left(b as u32)),
                WasmOp::I32Rotr => bin_i32(&mut self.value_stack, |a, b| a.rotate_right(b as u32)),
                WasmOp::I32Extend8S => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I32(a as i8 as i32));
                }
                WasmOp::I32Extend16S => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I32(a as i16 as i32));
                }

                WasmOp::I64Clz => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack
                        .push(WasmVal::I64(a.leading_zeros() as i64));
                }
                WasmOp::I64Ctz => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack
                        .push(WasmVal::I64(a.trailing_zeros() as i64));
                }
                WasmOp::I64Popcnt => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I64(a.count_ones() as i64));
                }
                WasmOp::I64Add => bin_i64(&mut self.value_stack, i64::wrapping_add),
                WasmOp::I64Sub => bin_i64(&mut self.value_stack, i64::wrapping_sub),
                WasmOp::I64Mul => bin_i64(&mut self.value_stack, i64::wrapping_mul),
                WasmOp::I64DivS => {
                    let b = pop_i64(&mut self.value_stack);
                    let a = pop_i64(&mut self.value_stack);
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    let (q, overflow) = a.overflowing_div(b);
                    if overflow {
                        return Err(WasmTrap::IntegerOverflow);
                    }
                    self.value_stack.push(WasmVal::I64(q));
                }
                WasmOp::I64DivU => {
                    let b = pop_i64(&mut self.value_stack) as u64;
                    let a = pop_i64(&mut self.value_stack) as u64;
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    self.value_stack.push(WasmVal::I64((a / b) as i64));
                }
                WasmOp::I64RemS => {
                    let b = pop_i64(&mut self.value_stack);
                    let a = pop_i64(&mut self.value_stack);
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    self.value_stack.push(WasmVal::I64(a.wrapping_rem(b)));
                }
                WasmOp::I64RemU => {
                    let b = pop_i64(&mut self.value_stack) as u64;
                    let a = pop_i64(&mut self.value_stack) as u64;
                    if b == 0 {
                        return Err(WasmTrap::IntegerDivisionByZero);
                    }
                    self.value_stack.push(WasmVal::I64((a % b) as i64));
                }
                WasmOp::I64And => bin_i64(&mut self.value_stack, |a, b| a & b),
                WasmOp::I64Or => bin_i64(&mut self.value_stack, |a, b| a | b),
                WasmOp::I64Xor => bin_i64(&mut self.value_stack, |a, b| a ^ b),
                WasmOp::I64Shl => bin_i64(&mut self.value_stack, |a, b| a.wrapping_shl(b as u32)),
                WasmOp::I64ShrS => bin_i64(&mut self.value_stack, |a, b| a.wrapping_shr(b as u32)),
                WasmOp::I64ShrU => bin_i64(&mut self.value_stack, |a, b| {
                    ((a as u64).wrapping_shr(b as u32)) as i64
                }),
                WasmOp::I64Rotl => bin_i64(&mut self.value_stack, |a, b| a.rotate_left(b as u32)),
                WasmOp::I64Rotr => bin_i64(&mut self.value_stack, |a, b| a.rotate_right(b as u32)),
                WasmOp::I64Extend8S => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I64(a as i8 as i64));
                }
                WasmOp::I64Extend16S => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I64(a as i16 as i64));
                }
                WasmOp::I64Extend32S => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I64(a as i32 as i64));
                }

                WasmOp::F32Abs => un_f32(&mut self.value_stack, f32::abs),
                WasmOp::F32Neg => un_f32(&mut self.value_stack, |x| -x),
                WasmOp::F32Ceil => un_f32(&mut self.value_stack, f32::ceil),
                WasmOp::F32Floor => un_f32(&mut self.value_stack, f32::floor),
                WasmOp::F32Trunc => un_f32(&mut self.value_stack, f32::trunc),
                WasmOp::F32Nearest => un_f32(&mut self.value_stack, f32::round_ties_even),
                WasmOp::F32Sqrt => un_f32(&mut self.value_stack, f32::sqrt),
                WasmOp::F32Add => bin_f32(&mut self.value_stack, |a, b| a + b),
                WasmOp::F32Sub => bin_f32(&mut self.value_stack, |a, b| a - b),
                WasmOp::F32Mul => bin_f32(&mut self.value_stack, |a, b| a * b),
                WasmOp::F32Div => bin_f32(&mut self.value_stack, |a, b| a / b),
                WasmOp::F32Min => bin_f32(&mut self.value_stack, wasm_f32_min),
                WasmOp::F32Max => bin_f32(&mut self.value_stack, wasm_f32_max),
                WasmOp::F32Copysign => bin_f32(&mut self.value_stack, f32::copysign),

                WasmOp::F64Abs => un_f64(&mut self.value_stack, f64::abs),
                WasmOp::F64Neg => un_f64(&mut self.value_stack, |x| -x),
                WasmOp::F64Ceil => un_f64(&mut self.value_stack, f64::ceil),
                WasmOp::F64Floor => un_f64(&mut self.value_stack, f64::floor),
                WasmOp::F64Trunc => un_f64(&mut self.value_stack, f64::trunc),
                WasmOp::F64Nearest => un_f64(&mut self.value_stack, f64::round_ties_even),
                WasmOp::F64Sqrt => un_f64(&mut self.value_stack, f64::sqrt),
                WasmOp::F64Add => bin_f64(&mut self.value_stack, |a, b| a + b),
                WasmOp::F64Sub => bin_f64(&mut self.value_stack, |a, b| a - b),
                WasmOp::F64Mul => bin_f64(&mut self.value_stack, |a, b| a * b),
                WasmOp::F64Div => bin_f64(&mut self.value_stack, |a, b| a / b),
                WasmOp::F64Min => bin_f64(&mut self.value_stack, wasm_f64_min),
                WasmOp::F64Max => bin_f64(&mut self.value_stack, wasm_f64_max),
                WasmOp::F64Copysign => bin_f64(&mut self.value_stack, f64::copysign),

                WasmOp::I32WrapI64 => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I32(a as i32));
                }
                WasmOp::I32TruncF32S => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack
                        .push(WasmVal::I32(trunc_f64_to_i32(x as f64)?));
                }
                WasmOp::I32TruncF32U => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack
                        .push(WasmVal::I32(trunc_f64_to_u32(x as f64)? as i32));
                }
                WasmOp::I32TruncF64S => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I32(trunc_f64_to_i32(x)?));
                }
                WasmOp::I32TruncF64U => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack
                        .push(WasmVal::I32(trunc_f64_to_u32(x)? as i32));
                }
                WasmOp::I64ExtendI32S => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I64(a as i64));
                }
                WasmOp::I64ExtendI32U => {
                    let a = pop_i32(&mut self.value_stack) as u32;
                    self.value_stack.push(WasmVal::I64(a as i64));
                }
                WasmOp::I64TruncF32S => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I64(trunc_f32_to_i64(x)?));
                }
                WasmOp::I64TruncF32U => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack
                        .push(WasmVal::I64(trunc_f32_to_u64(x)? as i64));
                }
                WasmOp::I64TruncF64S => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I64(trunc_f64_to_i64(x)?));
                }
                WasmOp::I64TruncF64U => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack
                        .push(WasmVal::I64(trunc_f64_to_u64(x)? as i64));
                }
                WasmOp::F32ConvertI32S => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::f32(a as f32));
                }
                WasmOp::F32ConvertI32U => {
                    let a = pop_i32(&mut self.value_stack) as u32;
                    self.value_stack.push(WasmVal::f32(a as f32));
                }
                WasmOp::F32ConvertI64S => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::f32(a as f32));
                }
                WasmOp::F32ConvertI64U => {
                    let a = pop_i64(&mut self.value_stack) as u64;
                    self.value_stack.push(WasmVal::f32(a as f32));
                }
                WasmOp::F32DemoteF64 => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack.push(WasmVal::f32(x as f32));
                }
                WasmOp::F64ConvertI32S => {
                    let a = pop_i32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::f64(a as f64));
                }
                WasmOp::F64ConvertI32U => {
                    let a = pop_i32(&mut self.value_stack) as u32;
                    self.value_stack.push(WasmVal::f64(a as f64));
                }
                WasmOp::F64ConvertI64S => {
                    let a = pop_i64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::f64(a as f64));
                }
                WasmOp::F64ConvertI64U => {
                    let a = pop_i64(&mut self.value_stack) as u64;
                    self.value_stack.push(WasmVal::f64(a as f64));
                }
                WasmOp::F64PromoteF32 => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack.push(WasmVal::f64(x as f64));
                }
                WasmOp::I32ReinterpretF32 => {
                    let b = pop_f32(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I32(b as i32));
                }
                WasmOp::I64ReinterpretF64 => {
                    let b = pop_f64(&mut self.value_stack);
                    self.value_stack.push(WasmVal::I64(b as i64));
                }
                WasmOp::F32ReinterpretI32 => {
                    let b = pop_i32(&mut self.value_stack) as u32;
                    self.value_stack.push(WasmVal::F32(b));
                }
                WasmOp::F64ReinterpretI64 => {
                    let b = pop_i64(&mut self.value_stack) as u64;
                    self.value_stack.push(WasmVal::F64(b));
                }

                WasmOp::I32TruncSatF32S => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I32(x as i32));
                }
                WasmOp::I32TruncSatF32U => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I32((x as u32) as i32));
                }
                WasmOp::I32TruncSatF64S => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I32(x as i32));
                }
                WasmOp::I32TruncSatF64U => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I32((x as u32) as i32));
                }
                WasmOp::I64TruncSatF32S => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I64(x as i64));
                }
                WasmOp::I64TruncSatF32U => {
                    let x = f32::from_bits(pop_f32(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I64((x as u64) as i64));
                }
                WasmOp::I64TruncSatF64S => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I64(x as i64));
                }
                WasmOp::I64TruncSatF64U => {
                    let x = f64::from_bits(pop_f64(&mut self.value_stack));
                    self.value_stack.push(WasmVal::I64((x as u64) as i64));
                }

                WasmOp::Simd(simd_op) => {
                    crate::simd::exec_simd(simd_op, &mut self.value_stack, &mut self.memories)?;
                }
            }
        }
        Ok(())
    }
}

fn eval_const_init(init: &ConstInit, globals: &[GlobalInstance]) -> Result<WasmVal, WasmError> {
    match *init {
        ConstInit::Val(v) => Ok(v),
        ConstInit::GlobalGet(idx) => globals
            .get(idx as usize)
            .map(|g| g.value)
            .ok_or_else(|| WasmError::Validation("invalid global.get in const expr".into())),
    }
}

fn grow_memory_vec(
    memories: &mut [Vec<u8>],
    max_pages: &[u32],
    mem_idx: usize,
    delta_pages: u32,
) -> i32 {
    let Some(mem) = memories.get_mut(mem_idx) else {
        return -1;
    };
    let cur_pages = (mem.len() / WASM_PAGE_BYTES) as u32;
    let limit = max_pages.get(mem_idx).copied().unwrap_or(1024);
    let Some(new_pages) = cur_pages.checked_add(delta_pages) else {
        return -1;
    };
    if new_pages > limit || new_pages > 65_536 {
        return -1;
    }
    let Some(new_bytes) = (new_pages as usize).checked_mul(WASM_PAGE_BYTES) else {
        return -1;
    };
    if mem
        .try_reserve_exact(new_bytes.saturating_sub(mem.len()))
        .is_err()
    {
        return -1;
    }
    mem.resize(new_bytes, 0);
    cur_pages as i32
}

#[inline]
fn exec_branch(
    bt: BranchTarget,
    value_stack: &mut Vec<WasmVal>,
    label_stack: &mut Vec<u32>,
    pc_out: &mut usize,
) {
    let target_label_idx = label_stack.len() - 1 - (bt.label_depth as usize);
    let stack_base = label_stack[target_label_idx] as usize;
    let arity = bt.arity as usize;
    let src_start = value_stack.len() - arity;
    if src_start != stack_base && arity > 0 {
        value_stack.copy_within(src_start..src_start + arity, stack_base);
    }
    value_stack.truncate(stack_base + arity);
    if bt.is_loop {
        label_stack.truncate(target_label_idx + 1);
    } else {
        label_stack.truncate(target_label_idx);
    }
    *pc_out = bt.target_pc as usize;
}

fn resolve_indirect_callee(
    module: &WasmModule,
    tables: &[TableInstance],
    table_idx: usize,
    elem_idx: usize,
    expected_type_idx: u32,
) -> Result<u32, WasmTrap> {
    let tbl = tables
        .get(table_idx)
        .ok_or(WasmTrap::OutOfBoundsTableAccess)?;
    let elem = *tbl
        .elements
        .get(elem_idx)
        .ok_or(WasmTrap::OutOfBoundsTableAccess)?;
    let callee_idx = match elem {
        WasmVal::FuncRef(Some(idx)) => idx,
        WasmVal::FuncRef(None) => return Err(WasmTrap::IndirectCallNull),
        _ => return Err(WasmTrap::IndirectCallSignatureMismatch),
    };
    let actual_sig = module.func_sig(callee_idx);
    let expected_sig = &module.types[expected_type_idx as usize];
    if actual_sig != expected_sig {
        return Err(WasmTrap::IndirectCallSignatureMismatch);
    }
    Ok(callee_idx)
}

#[inline]
fn pop_i32(stack: &mut Vec<WasmVal>) -> i32 {
    match stack.pop().expect("validated stack") {
        WasmVal::I32(v) => v,
        _ => unreachable!("validated i32"),
    }
}

#[inline]
fn pop_i64(stack: &mut Vec<WasmVal>) -> i64 {
    match stack.pop().expect("validated stack") {
        WasmVal::I64(v) => v,
        _ => unreachable!("validated i64"),
    }
}

#[inline]
fn pop_f32(stack: &mut Vec<WasmVal>) -> u32 {
    match stack.pop().expect("validated stack") {
        WasmVal::F32(v) => v,
        _ => unreachable!("validated f32"),
    }
}

#[inline]
fn pop_f64(stack: &mut Vec<WasmVal>) -> u64 {
    match stack.pop().expect("validated stack") {
        WasmVal::F64(v) => v,
        _ => unreachable!("validated f64"),
    }
}

#[inline]
fn load_bytes<'a>(
    memories: &'a [Vec<u8>],
    imm: MemImmediate,
    stack: &mut Vec<WasmVal>,
    len: usize,
) -> Result<&'a [u8], WasmTrap> {
    let base = pop_i32(stack) as u32 as u64;
    let addr = base
        .checked_add(imm.offset)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let end = addr
        .checked_add(len as u64)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let mem = &memories[imm.mem as usize];
    if end > mem.len() as u64 {
        return Err(WasmTrap::OutOfBoundsMemoryAccess);
    }
    Ok(&mem[addr as usize..end as usize])
}

#[inline]
fn store_bytes(
    memories: &mut [Vec<u8>],
    imm: MemImmediate,
    stack: &mut Vec<WasmVal>,
    bytes: &[u8],
) -> Result<(), WasmTrap> {
    let base = pop_i32(stack) as u32 as u64;
    let addr = base
        .checked_add(imm.offset)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let end = addr
        .checked_add(bytes.len() as u64)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let mem = &mut memories[imm.mem as usize];
    if end > mem.len() as u64 {
        return Err(WasmTrap::OutOfBoundsMemoryAccess);
    }
    mem[addr as usize..end as usize].copy_from_slice(bytes);
    Ok(())
}

#[inline]
fn bin_i32(stack: &mut Vec<WasmVal>, f: impl FnOnce(i32, i32) -> i32) {
    let b = pop_i32(stack);
    let a = pop_i32(stack);
    stack.push(WasmVal::I32(f(a, b)));
}

#[inline]
fn bin_i64(stack: &mut Vec<WasmVal>, f: impl FnOnce(i64, i64) -> i64) {
    let b = pop_i64(stack);
    let a = pop_i64(stack);
    stack.push(WasmVal::I64(f(a, b)));
}

#[inline]
fn bin_i32_cmp(stack: &mut Vec<WasmVal>, f: impl FnOnce(i32, i32) -> bool) {
    let b = pop_i32(stack);
    let a = pop_i32(stack);
    stack.push(WasmVal::I32(i32::from(f(a, b))));
}

#[inline]
fn bin_i64_cmp(stack: &mut Vec<WasmVal>, f: impl FnOnce(i64, i64) -> bool) {
    let b = pop_i64(stack);
    let a = pop_i64(stack);
    stack.push(WasmVal::I32(i32::from(f(a, b))));
}

#[inline]
fn bin_f32_cmp(stack: &mut Vec<WasmVal>, f: impl FnOnce(f32, f32) -> bool) {
    let b = f32::from_bits(pop_f32(stack));
    let a = f32::from_bits(pop_f32(stack));
    stack.push(WasmVal::I32(i32::from(f(a, b))));
}

#[inline]
fn bin_f64_cmp(stack: &mut Vec<WasmVal>, f: impl FnOnce(f64, f64) -> bool) {
    let b = f64::from_bits(pop_f64(stack));
    let a = f64::from_bits(pop_f64(stack));
    stack.push(WasmVal::I32(i32::from(f(a, b))));
}

#[inline]
fn un_f32(stack: &mut Vec<WasmVal>, f: impl FnOnce(f32) -> f32) {
    let a = f32::from_bits(pop_f32(stack));
    stack.push(WasmVal::f32(f(a)));
}

#[inline]
fn bin_f32(stack: &mut Vec<WasmVal>, f: impl FnOnce(f32, f32) -> f32) {
    let b = f32::from_bits(pop_f32(stack));
    let a = f32::from_bits(pop_f32(stack));
    stack.push(WasmVal::f32(f(a, b)));
}

#[inline]
fn un_f64(stack: &mut Vec<WasmVal>, f: impl FnOnce(f64) -> f64) {
    let a = f64::from_bits(pop_f64(stack));
    stack.push(WasmVal::f64(f(a)));
}

#[inline]
fn bin_f64(stack: &mut Vec<WasmVal>, f: impl FnOnce(f64, f64) -> f64) {
    let b = f64::from_bits(pop_f64(stack));
    let a = f64::from_bits(pop_f64(stack));
    stack.push(WasmVal::f64(f(a, b)));
}

#[inline]
fn wasm_f32_min(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_negative() || b.is_sign_negative()) {
        -0.0
    } else {
        a.min(b)
    }
}

#[inline]
fn wasm_f32_max(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_positive() || b.is_sign_positive()) {
        0.0
    } else {
        a.max(b)
    }
}

#[inline]
fn wasm_f64_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_negative() || b.is_sign_negative()) {
        -0.0
    } else {
        a.min(b)
    }
}

#[inline]
fn wasm_f64_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_positive() || b.is_sign_positive()) {
        0.0
    } else {
        a.max(b)
    }
}

fn trunc_f64_to_i32(x: f64) -> Result<i32, WasmTrap> {
    if x.is_nan() {
        return Err(WasmTrap::InvalidConversionToInteger);
    }
    let t = x.trunc();
    if t < (i32::MIN as f64) || t > (i32::MAX as f64) {
        return Err(WasmTrap::IntegerOverflow);
    }
    Ok(t as i32)
}

fn trunc_f64_to_u32(x: f64) -> Result<u32, WasmTrap> {
    if x.is_nan() {
        return Err(WasmTrap::InvalidConversionToInteger);
    }
    let t = x.trunc();
    if t < 0.0 || t > (u32::MAX as f64) {
        return Err(WasmTrap::IntegerOverflow);
    }
    Ok(t as u32)
}

fn trunc_f32_to_i64(x: f32) -> Result<i64, WasmTrap> {
    if x.is_nan() {
        return Err(WasmTrap::InvalidConversionToInteger);
    }
    let t = x.trunc();
    // `i64::MAX as f32` rounds up to `2^63` (`9223372036854775808.0`), so `>= 2^63` overflows!
    if t < -9_223_372_036_854_775_808.0_f32 || t >= 9_223_372_036_854_775_808.0_f32 {
        return Err(WasmTrap::IntegerOverflow);
    }
    Ok(t as i64)
}

fn trunc_f32_to_u64(x: f32) -> Result<u64, WasmTrap> {
    if x.is_nan() {
        return Err(WasmTrap::InvalidConversionToInteger);
    }
    let t = x.trunc();
    if t < 0.0 || t >= 18_446_744_073_709_551_616.0_f32 {
        return Err(WasmTrap::IntegerOverflow);
    }
    Ok(t as u64)
}

fn trunc_f64_to_i64(x: f64) -> Result<i64, WasmTrap> {
    if x.is_nan() {
        return Err(WasmTrap::InvalidConversionToInteger);
    }
    let t = x.trunc();
    if t < -9_223_372_036_854_775_808.0_f64 || t >= 9_223_372_036_854_775_808.0_f64 {
        return Err(WasmTrap::IntegerOverflow);
    }
    Ok(t as i64)
}

fn trunc_f64_to_u64(x: f64) -> Result<u64, WasmTrap> {
    if x.is_nan() {
        return Err(WasmTrap::InvalidConversionToInteger);
    }
    let t = x.trunc();
    if t < 0.0 || t >= 18_446_744_073_709_551_616.0_f64 {
        return Err(WasmTrap::IntegerOverflow);
    }
    Ok(t as u64)
}
