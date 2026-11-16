//! Validate executable artifacts before installing any code or heap constants.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::fmt;

use type_pool::{TypeIndex, TypeKind, TypePool};

use crate::{
    AddrMode, CaptureAbi, CodegenOutput, CompiledArtifact, CompiledFunction, Constant, Instruction,
    InstructionData, Opcode, ParameterAbi, Reg,
};

/// A malformed or unsupported artifact, with the offending metadata or PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactError {
    pub context: String,
    pub message: String,
}

impl ArtifactError {
    pub fn new(context: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            context: context.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.message)
    }
}

impl std::error::Error for ArtifactError {}

// Dense IDs keep bytecode stores bounded independently of their grow-to-ID API.
const MAX_FUNCTIONS: usize = 1 << 16;
const MAX_INSTRUCTIONS: usize = 1 << 24;
const MAX_CONSTANTS: usize = 1 << 22;
const MAX_SLOTS: u32 = 1 << 17;

#[derive(Clone, Copy)]
enum VContext {
    Static(&'static str),
    Global(usize),
    EnumConst(usize),
    Function(u32),
    ControlFlow(u32),
    Pc(u32, usize),
}

impl VContext {
    fn render(self) -> String {
        match self {
            Self::Static(text) => text.to_owned(),
            Self::Global(index) => format!("global {index}"),
            Self::EnumConst(index) => format!("enum constant {index}"),
            Self::Function(id) => format!("function {id}"),
            Self::ControlFlow(id) => format!("function {id} control flow"),
            Self::Pc(id, pc) => format!("function {id} PC {pc}"),
        }
    }
}

#[cold]
fn ctx_err(context: VContext, message: impl Into<String>) -> ArtifactError {
    ArtifactError::new(context.render(), message)
}

/// Collect native function IDs from actual call operands, never numeric constants.
pub fn builtin_references(output: &CodegenOutput) -> Result<BTreeSet<u32>, ArtifactError> {
    let mut references = BTreeSet::new();
    for function in &output.functions {
        let id = function.func_id.0;
        for (pc, &word) in function.instructions.iter().enumerate() {
            let instruction = decode(word, VContext::Pc(id, pc))?;
            if let (Opcode::CallBuiltin, InstructionData::C { payload }) =
                (instruction.opcode, instruction.data)
            {
                references.insert(Instruction::c_arg_count_func_id(payload).1);
            }
        }
    }
    Ok(references)
}

/// Check cross-pool references, executable operands and bounded frame layouts.
pub fn validate_artifact(artifact: &CompiledArtifact) -> Result<(), ArtifactError> {
    let output = &artifact.codegen_output;
    if output.functions.len() > MAX_FUNCTIONS || output.constants.len() > MAX_CONSTANTS {
        return Err(ArtifactError::new(
            "artifact",
            "function or constant limit exceeded",
        ));
    }
    let mut dense_functions = vec![None; output.functions.len()];
    let mut seen_ids = BTreeSet::new();
    let mut words = 0usize;
    for function in &output.functions {
        words = words
            .checked_add(function.instructions.len())
            .filter(|&count| count <= MAX_INSTRUCTIONS)
            .ok_or_else(|| ArtifactError::new("artifact", "instruction limit exceeded"))?;
        if !seen_ids.insert(function.func_id.0) {
            return Err(ArtifactError::new("functions", "duplicate function ID"));
        }
        if let Some(slot) = dense_functions.get_mut(function.func_id.0 as usize) {
            *slot = Some(function);
        }
    }
    let mut functions = Vec::with_capacity(output.functions.len());
    for slot in dense_functions {
        let Some(function) = slot else {
            return Err(ArtifactError::new(
                "functions",
                "function IDs must be dense from zero",
            ));
        };
        functions.push(function);
    }
    artifact
        .type_pool
        .validate()
        .map_err(|error| ArtifactError::new("type pool", error.to_string()))?;
    if artifact.type_pool.has_unresolved_derived_methods() {
        return Err(ArtifactError::new(
            "type pool",
            "unresolved derived method sentinel in executable artifact",
        ));
    }
    let pool_len = artifact.type_pool.len();
    let max_trait_slots = artifact
        .type_pool
        .trait_schemas_snapshot()
        .iter()
        .map(|schema| schema.slots.len())
        .max()
        .unwrap_or(0);
    let func_len = functions.len();
    let validator = Validator {
        artifact,
        pool: &artifact.type_pool,
        functions,
        max_trait_slots,
        has_list_roles: artifact.type_pool.list_type().is_some()
            && artifact.type_pool.list_buffer_type().is_some(),
        has_map_roles: artifact.type_pool.map_type().is_some()
            && artifact.type_pool.map_buffer_type().is_some(),
        validated_runtime_types: RefCell::new(vec![None; pool_len]),
        validated_error_ctors: RefCell::new(vec![false; pool_len]),
        validated_captures: RefCell::new(vec![None; func_len]),
        validated_strings: RefCell::new(HashSet::new()),
        cf_depths: RefCell::new(Vec::new()),
        cf_pending: RefCell::new(Vec::new()),
    };
    let mut names = BTreeSet::new();
    let mut builtins = BTreeSet::new();
    for builtin in &artifact.builtins {
        if builtin.id >= 1 << 14
            || builtin.name.is_empty()
            || !builtins.insert(builtin.id)
            || !names.insert(&builtin.name)
        {
            return Err(ArtifactError::new(
                "builtin manifest",
                "invalid or duplicate builtin ID/name",
            ));
        }
    }
    if !builtin_references(output)?.is_subset(&builtins) {
        return Err(ArtifactError::new(
            "builtin manifest",
            "call references undeclared builtin",
        ));
    }
    for (index, global) in output.globals.iter().enumerate() {
        validator.runtime_type(global.type_index, VContext::Global(index))?;
    }
    if output.globals.len() > MAX_SLOTS as usize {
        return Err(ArtifactError::new("globals", "global slot limit exceeded"));
    }
    for (index, constant) in output.constants.iter().enumerate() {
        match constant {
            Constant::Type(index) => {
                validator.type_index(*index, VContext::Static("Type constant"))?;
            }
            Constant::Enum {
                type_index,
                variant,
            } => {
                validator.enum_variant(*type_index, *variant, VContext::EnumConst(index))?;
            }
            _ => {}
        }
    }
    for function in &output.functions {
        validator.function(function)?;
    }
    if let Some(entry) = artifact.entry {
        let entry = validator.target(entry.0, VContext::Static("entry"))?;
        if entry.is_closure || entry.param_count != 0 {
            return Err(ArtifactError::new(
                "entry",
                "entry must be a non-closure with no parameters",
            ));
        }
    }
    validator.methods()?;
    crate::method_context::validate_method_contexts(artifact)?;
    Ok(())
}

fn decode(word: u32, context: VContext) -> Result<Instruction, ArtifactError> {
    let instruction = Instruction::decode(word)
        .ok_or_else(|| ctx_err(context, format!("invalid instruction {word:#010x}")))?;
    if (!instruction.opcode.uses_amode() && word & (3 << 22) != 0) || instruction.encode() != word {
        return Err(ctx_err(context, "reserved instruction bits are nonzero"));
    }
    Ok(instruction)
}

struct Validator<'a> {
    artifact: &'a CompiledArtifact,
    pool: &'a TypePool,
    functions: Vec<&'a CompiledFunction>,
    max_trait_slots: usize,
    has_list_roles: bool,
    has_map_roles: bool,
    validated_runtime_types: RefCell<Vec<Option<TypeIndex>>>,
    validated_error_ctors: RefCell<Vec<bool>>,
    validated_captures: RefCell<Vec<Option<u8>>>,
    validated_strings: RefCell<HashSet<u32>>,
    cf_depths: RefCell<Vec<Option<u32>>>,
    cf_pending: RefCell<Vec<(usize, u32)>>,
}

impl Validator<'_> {
    fn pool(&self) -> &TypePool {
        self.pool
    }

    fn type_index(&self, index: TypeIndex, context: VContext) -> Result<TypeIndex, ArtifactError> {
        let ty = self
            .pool()
            .canonical_type(index)
            .ok_or_else(|| ctx_err(context, format!("invalid type index {}", index.as_u32())))?;
        // Abstract associated leaves belong to declarations, not executable
        // signatures, global slots, type constants or instruction operands.
        if self.pool().contains_associated_type(ty) {
            return Err(ctx_err(
                context,
                "unresolved associated type is not an executable type",
            ));
        }
        Ok(ty)
    }

    fn runtime_type(
        &self,
        index: TypeIndex,
        context: VContext,
    ) -> Result<TypeIndex, ArtifactError> {
        let root = self.type_index(index, context)?;
        if let Some(Some(cached)) = self
            .validated_runtime_types
            .borrow()
            .get(root.as_u32() as usize)
            .copied()
        {
            return Ok(cached);
        }
        let mut pending = vec![root];
        let mut seen = HashSet::new();
        while let Some(index) = pending.pop() {
            let index = self.type_index(index, context)?;
            if self
                .validated_runtime_types
                .borrow()
                .get(index.as_u32() as usize)
                .copied()
                .flatten()
                .is_some()
                || !seen.insert(index)
            {
                continue;
            }
            let info = self.pool().get(index);
            match &info.kind {
                TypeKind::ErrorQualified { inner, errors } => {
                    if info.size != 24 || info.align != 8 {
                        return Err(ctx_err(
                            context,
                            "unsupported legacy Error layout in executable type",
                        ));
                    }
                    if errors.is_empty()
                        || self.pool().identity_input().is_none()
                        || self.pool().stable_type_id(index).is_err()
                    {
                        return Err(ctx_err(
                            context,
                            "Error executable type requires authenticated identity",
                        ));
                    }
                    // A single payload slot cannot transport a bare trait
                    // data/proof pair. Concrete specialized payloads are valid.
                    let mut payloads = vec![*inner];
                    payloads.extend(errors.iter().copied());
                    let mut payload_seen = HashSet::new();
                    while let Some(payload) = payloads.pop() {
                        let payload = self.type_index(payload, context)?;
                        if !payload_seen.insert(payload) {
                            continue;
                        }
                        match &self.pool().get(payload).kind {
                            TypeKind::Trait { .. } => {
                                return Err(ctx_err(
                                    context,
                                    "Error payload trait storage requires an unsupported proof carrier",
                                ));
                            }
                            TypeKind::Optional { inner }
                            | TypeKind::Future { inner }
                            | TypeKind::EffectQualified { inner, .. } => payloads.push(*inner),
                            TypeKind::Tuple { elements } => payloads.extend(elements),
                            _ => {}
                        }
                    }
                    pending.push(*inner);
                    for &member in errors {
                        if member != type_pool::Intrinsic::Any.type_index() {
                            if self.pool().stable_type_id(member).is_err() {
                                return Err(ctx_err(
                                    context,
                                    "Error member requires a concrete stable identity",
                                ));
                            }
                            pending.push(member);
                        }
                    }
                }
                TypeKind::Optional { inner }
                | TypeKind::Future { inner }
                | TypeKind::Newtype { inner, .. }
                | TypeKind::EffectQualified { inner, .. } => pending.push(*inner),
                TypeKind::Tuple { elements } => pending.extend(elements),
                TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. } => {
                    pending.extend(params);
                    pending.push(*ret);
                }
                TypeKind::Struct { fields, .. } => {
                    pending.extend(fields.iter().map(|field| field.ty))
                }
                TypeKind::Enum { variants, .. } => pending.extend(
                    variants
                        .iter()
                        .flat_map(|variant| variant.fields.iter().map(|field| field.ty)),
                ),
                _ => {}
            }
        }
        {
            let mut cache = self.validated_runtime_types.borrow_mut();
            for verified in seen {
                if let Some(slot) = cache.get_mut(verified.as_u32() as usize) {
                    *slot = Some(verified);
                }
            }
        }
        Ok(root)
    }

    fn enum_variant(
        &self,
        ty: TypeIndex,
        tag: u32,
        context: VContext,
    ) -> Result<&type_pool::VariantInfo, ArtifactError> {
        let ty = self.type_index(ty, context)?;
        let TypeKind::Enum { variants, .. } = &self.pool().get(ty).kind else {
            return Err(ctx_err(context, "enum descriptor requires an enum type"));
        };
        variants
            .iter()
            .find(|variant| variant.tag == tag)
            .ok_or_else(|| ctx_err(context, "unknown enum variant tag"))
    }

    fn enum_descriptor(
        &self,
        index: u32,
        context: VContext,
    ) -> Result<&type_pool::VariantInfo, ArtifactError> {
        let Constant::Enum {
            type_index,
            variant,
        } = self.constant(index, context)?
        else {
            return Err(ctx_err(
                context,
                "enum instruction requires an enum descriptor constant",
            ));
        };
        self.runtime_type(*type_index, context)?;
        self.enum_variant(*type_index, *variant, context)
    }

    fn load_constant(&self, index: u32, context: VContext) -> Result<(), ArtifactError> {
        if let Constant::Enum {
            type_index,
            variant,
        } = self.constant(index, context)?
            && !self
                .enum_variant(*type_index, *variant, context)?
                .fields
                .is_empty()
        {
            return Err(ctx_err(
                context,
                "payload enum descriptor cannot be loaded as a nullary value",
            ));
        }
        Ok(())
    }

    fn target(&self, id: u32, context: VContext) -> Result<&CompiledFunction, ArtifactError> {
        self.functions
            .get(id as usize)
            .copied()
            .ok_or_else(|| ctx_err(context, format!("unknown function {id}")))
    }

    fn constant(&self, index: u32, context: VContext) -> Result<&Constant, ArtifactError> {
        self.artifact
            .codegen_output
            .constants
            .get(index as usize)
            .ok_or_else(|| ctx_err(context, format!("invalid constant index {index}")))
    }

    fn integer(&self, index: u32, context: VContext) -> Result<u64, ArtifactError> {
        match self.constant(index, context)? {
            Constant::UInt(value) => Ok(*value),
            Constant::Int(value) if *value >= 0 => Ok(*value as u64),
            _ => Err(ctx_err(
                context,
                "metadata constant must be a nonnegative 64-bit integer",
            )),
        }
    }

    fn integer_u32(&self, index: u32, context: VContext) -> Result<u32, ArtifactError> {
        u32::try_from(self.integer(index, context)?)
            .map_err(|_| ctx_err(context, "metadata integer exceeds u32"))
    }

    fn string(&self, id: u32, context: VContext) -> Result<(), ArtifactError> {
        if self.validated_strings.borrow().contains(&id) {
            return Ok(());
        }
        if str_interner::try_get(str_interner::StrId::from_raw(id)).is_some() {
            self.validated_strings.borrow_mut().insert(id);
            Ok(())
        } else {
            Err(ctx_err(context, format!("invalid string ID {id}")))
        }
    }

    fn call(&self, id: u32, count: u8, context: VContext) -> Result<(), ArtifactError> {
        let target = self.target(id, context)?;
        if target.is_closure && self.captures(target, context)? != 0 {
            return Err(ctx_err(
                context,
                "direct call target requires a closure environment",
            ));
        }
        if target.param_count != count {
            return Err(ctx_err(
                context,
                format!(
                    "direct call supplies {count} parameters but function {id} requires {}; default/variadic argument adaptation is not encoded",
                    target.param_count
                ),
            ));
        }
        Ok(())
    }

    fn effect(&self, index: u32, context: VContext) -> Result<usize, ArtifactError> {
        let ty = self.runtime_type(TypeIndex::from_raw(index), context)?;
        match &self.pool().get(ty).kind {
            TypeKind::Effect { params, .. } => Ok(params.len()),
            _ => Err(ctx_err(
                context,
                "effect operand does not reference an effect type",
            )),
        }
    }

    fn captures(
        &self,
        function: &CompiledFunction,
        context: VContext,
    ) -> Result<u8, ArtifactError> {
        let slot_index = function.func_id.0 as usize;
        if let Some(Some(cached)) = self.validated_captures.borrow().get(slot_index).copied() {
            return Ok(cached);
        }
        if let Some(owner) = function.display_owner {
            crate::validate_display_owner(
                self.pool(),
                function.func_id,
                owner,
                function.function_type,
                &function.abi,
                function.param_count,
                function.is_closure,
            )
            .map_err(|message| ctx_err(context, message))?;
        }
        let abi = &function.abi;
        if abi.physical_parameter_count() != function.param_count as usize
            || (!function.is_closure && abi.capture_count() != 0)
        {
            return Err(ctx_err(
                context,
                "explicit function ABI disagrees with physical entry",
            ));
        }
        if function.function_type != TypeIndex::INVALID {
            let ty = self.runtime_type(function.function_type, context)?;
            let TypeKind::Function { params, .. } = &self.pool().get(ty).kind else {
                return Err(ctx_err(context, "function metadata is not a Function type"));
            };
            if params.len() != abi.logical_parameter_count() {
                return Err(ctx_err(
                    context,
                    "explicit function ABI disagrees with logical signature",
                ));
            }
            for (&parameter, &ty) in abi.parameters.iter().zip(params) {
                match parameter {
                    ParameterAbi::Value => {}
                    ParameterAbi::Trait { view } => {
                        if self.pool().canonical_type(view) != self.pool().canonical_type(ty) {
                            return Err(ctx_err(
                                context,
                                "trait proof view disagrees with logical parameter",
                            ));
                        }
                    }
                    ParameterAbi::TraitSelf { view } => {
                        let concrete = self.type_index(ty, context)?;
                        let view = self.type_index(view, context)?;
                        if matches!(self.pool().get(concrete).kind, TypeKind::Trait { .. })
                            || !self.pool().vtables_snapshot().iter().any(|table| {
                                table.implementor == concrete
                                    && table.trait_type == view
                                    && self
                                        .pool()
                                        .check_associated_implementation_bindings(
                                            concrete,
                                            view,
                                            table.visible_scope,
                                        )
                                        .is_ok()
                            })
                        {
                            return Err(ctx_err(
                                context,
                                "specialized Self proof requires a concrete implementation",
                            ));
                        }
                    }
                }
            }
        } else if abi
            .parameters
            .iter()
            .any(|parameter| matches!(parameter, ParameterAbi::TraitSelf { .. }))
        {
            return Err(ctx_err(
                context,
                "specialized Self proof requires a logical signature",
            ));
        }
        let views = abi
            .captures
            .iter()
            .filter_map(|capture| match capture {
                CaptureAbi::Value => None,
                CaptureAbi::TraitProof { view } => Some(*view),
            })
            .chain(
                abi.parameters
                    .iter()
                    .filter_map(|parameter| match parameter {
                        ParameterAbi::Value => None,
                        ParameterAbi::Trait { view } | ParameterAbi::TraitSelf { view } => {
                            Some(*view)
                        }
                    }),
            );
        for view in views {
            let view = self.type_index(view, context)?;
            if !matches!(self.pool().get(view).kind, TypeKind::Trait { .. }) {
                return Err(ctx_err(context, "proof view must be a trait type"));
            }
        }
        for parameter in &abi.parameters {
            if let ParameterAbi::Trait { view } = parameter
                && self
                    .pool()
                    .trait_has_associated_types(*view)
                    .map_err(|error| ctx_err(context, error.to_string()))?
            {
                return Err(ctx_err(
                    context,
                    "associated trait proof ABI is not supported",
                ));
            }
        }
        for (index, capture) in abi.captures.iter().enumerate() {
            if matches!(capture, CaptureAbi::TraitProof { .. })
                && (index == 0 || abi.captures[index - 1] != CaptureAbi::Value)
            {
                return Err(ctx_err(
                    context,
                    "trait proof capture must follow its data capture",
                ));
            }
        }
        let count = abi.capture_count() as u8;
        if let Some(slot) = self.validated_captures.borrow_mut().get_mut(slot_index) {
            *slot = Some(count);
        }
        Ok(count)
    }

    fn closure(&self, id: u32, count: u8, context: VContext) -> Result<(), ArtifactError> {
        let target = self.target(id, context)?;
        if count > target.param_count
            || (!target.is_closure && count != 0)
            || self.captures(target, context)? != count
        {
            return Err(ctx_err(
                context,
                "closure capture count disagrees with target metadata",
            ));
        }
        Ok(())
    }

    fn methods(&self) -> Result<(), ArtifactError> {
        for schema in self.pool().trait_schemas_snapshot() {
            for slot in &schema.slots {
                self.string(
                    slot.name.as_u32(),
                    VContext::Static("trait dispatch schema"),
                )?;
            }
        }
        for index in 0..self.pool().len() {
            for method in self.pool().methods_of(TypeIndex::from_raw(index as u32)) {
                let target = self.target(method.func_id, VContext::Static("method table"))?;
                self.call(
                    method.func_id,
                    target.param_count,
                    VContext::Static("method table"),
                )?;
                self.string(method.name.as_u32(), VContext::Static("method table"))?;
            }
        }
        for implementation in self.pool().trait_impls_snapshot() {
            for method in &implementation.methods {
                let target =
                    self.target(method.func_id, VContext::Static("trait implementation"))?;
                self.call(
                    method.func_id,
                    target.param_count,
                    VContext::Static("trait implementation"),
                )?;
            }
        }
        for (index, table) in self.pool().vtables_snapshot().iter().enumerate() {
            if self.pool().trait_schema(table.trait_type).is_some() {
                let descriptors = self
                    .pool()
                    .checked_vtable_descriptors(index)
                    .map_err(|error| ArtifactError::new("vtable interface", error.to_string()))?;
                for descriptor in descriptors {
                    if descriptor.signature.is_none() {
                        continue;
                    }
                    let key = type_pool::TraitMethodKey {
                        trait_owner: descriptor.trait_owner,
                        name: descriptor.name,
                        signature: descriptor.signature,
                    };
                    let target =
                        self.target(descriptor.func_id, VContext::Static("vtable interface"))?;
                    self.pool()
                        .check_trait_method_signature_in_impl(
                            &key,
                            table.implementor,
                            target.function_type,
                            descriptor.implementation_trait,
                            descriptor.implementation_scope,
                        )
                        .map_err(|error| {
                            ArtifactError::new("vtable interface", error.to_string())
                        })?;
                }
            }
            for &id in &table.entries {
                let target = self.target(id, VContext::Static("vtable"))?;
                self.call(id, target.param_count, VContext::Static("vtable"))?;
            }
        }
        Ok(())
    }

    fn function(&self, function: &CompiledFunction) -> Result<(), ArtifactError> {
        let id = function.func_id.0;
        let context = VContext::Function(id);
        if function.register_count > Reg::MAX_GP
            || function.param_count > function.register_count
            || function.instructions.is_empty()
        {
            return Err(ctx_err(
                context,
                "invalid register/parameter count or empty function",
            ));
        }
        self.string(function.name.as_u32(), context)?;
        if function.function_type != TypeIndex::INVALID {
            self.runtime_type(function.function_type, context)?;
        }
        self.captures(function, context)?;
        let slots = match decode(function.instructions[0], context)?.data {
            InstructionData::E { payload }
                if function.instructions[0] >> 24 == Opcode::AllocateSlots as u32 =>
            {
                payload
            }
            _ => 0,
        };
        if slots > MAX_SLOTS {
            return Err(ctx_err(context, "local slot limit exceeded"));
        }
        if !function.safepoint_pcs.is_empty() {
            let mut safepoints = HashSet::with_capacity(function.safepoint_pcs.len());
            for &pc in &function.safepoint_pcs {
                if pc as usize >= function.instructions.len() || !safepoints.insert(pc) {
                    return Err(ctx_err(
                        context,
                        "safepoint PC is duplicate or outside function",
                    ));
                }
                if function.instructions[pc as usize] >> 24 != Opcode::Safepoint as u32 {
                    return Err(ctx_err(context, "safepoint PC does not point to Safepoint"));
                }
            }
        }
        for (pc, &word) in function.instructions.iter().enumerate() {
            let pc_context = VContext::Pc(id, pc);
            self.instruction(function, pc, slots, decode(word, pc_context)?, pc_context)?;
        }
        self.control_flow(function)?;
        Ok(())
    }

    // A loop must not accumulate handlers indefinitely. Joins require equal
    // local handler depth; handlers inherited from callers are left untouched.
    fn control_flow(&self, function: &CompiledFunction) -> Result<(), ArtifactError> {
        let context = VContext::ControlFlow(function.func_id.0);
        let mut depths = self.cf_depths.borrow_mut();
        depths.clear();
        depths.resize(function.instructions.len(), None);
        let mut pending = self.cf_pending.borrow_mut();
        pending.clear();
        pending.push((0usize, 0u32));
        while let Some((pc, depth)) = pending.pop() {
            if let Some(previous) = depths[pc] {
                if previous != depth {
                    return Err(ctx_err(
                        context,
                        "inconsistent handler depth at control-flow join",
                    ));
                }
                continue;
            }
            depths[pc] = Some(depth);
            let instruction = decode(function.instructions[pc], context)?;
            let mut depth = depth;
            match instruction.opcode {
                Opcode::PushHandlerClosure | Opcode::PushCapturingHandler => {
                    depth += 1;
                }
                Opcode::PopHandler | Opcode::AwaitPopHandler => {
                    depth = depth
                        .checked_sub(1)
                        .ok_or_else(|| ctx_err(context, "handler stack underflow"))?;
                }
                Opcode::ResetClosure => {
                    if let InstructionData::E { payload } = instruction.data
                        && ((payload >> 12) & 31) > depth
                    {
                        return Err(ctx_err(context, "reset consumes unavailable handlers"));
                    }
                }
                _ => {}
            }
            if matches!(
                instruction.opcode,
                Opcode::Return | Opcode::ReturnUnit | Opcode::Unreachable | Opcode::MatchFail
            ) {
                continue;
            }
            if let InstructionData::J { offset, .. } = instruction.data {
                pending.push(((pc as i64 + offset as i64) as usize, depth));
                if matches!(instruction.opcode, Opcode::Jmp | Opcode::JmpFar) {
                    continue;
                }
            }
            if pc + 1 >= function.instructions.len() {
                return Err(ctx_err(
                    context,
                    "execution falls off function without a terminator",
                ));
            }
            pending.push((pc + 1, depth));
        }
        Ok(())
    }

    fn instruction(
        &self,
        function: &CompiledFunction,
        pc: usize,
        slots: u32,
        instruction: Instruction,
        context: VContext,
    ) -> Result<(), ArtifactError> {
        use Opcode::*;
        let fail = |message| ctx_err(context, message);
        let register = |reg: Reg| {
            if reg.0 >= function.register_count {
                Err(fail("register outside declared register count"))
            } else {
                Ok(())
            }
        };
        let arguments = |count: u8| {
            if count > function.register_count {
                Err(fail("arguments exceed declared register count"))
            } else {
                Ok(())
            }
        };
        match instruction.data {
            InstructionData::R { dst, src1, src2 } => {
                register(dst)?;
                register(src1)?;
                if matches!(instruction.opcode, Neg | BitNot | Mov | Swap) {
                    if src2.0 != 0 {
                        return Err(fail("unused source register is nonzero"));
                    }
                } else {
                    register(src2)?;
                }
            }
            InstructionData::A { dst, base, imm12 } => {
                let index = imm12 as u32;
                let wide = ((base.0 as u32) << 12) | index;
                match instruction.opcode {
                    Load => {
                        register(dst)?;
                        if base.0 != 0 {
                            return Err(fail("unused load base register is nonzero"));
                        }
                        match instruction.amode {
                            AddrMode::Const => {
                                self.load_constant(index, context)?;
                            }
                            AddrMode::Imm => {}
                        }
                    }
                    LoadConstWide => {
                        register(dst)?;
                        self.load_constant(wide, context)?;
                    }
                    LoadUnit | LoadTrue | LoadFalse | LoadNull => {
                        register(dst)?;
                        if base.0 != 0 || index != 0 {
                            return Err(fail("unused literal operands are nonzero"));
                        }
                    }
                    TypeCheck | TypeCast | TypeCastSafe | TypeAssert => {
                        register(dst)?;
                        register(base)?;
                        self.runtime_type(TypeIndex::from_raw(index), context)?;
                    }
                    TraitProof | TraitAssert | TraitProject => {
                        register(dst)?;
                        register(base)?;
                        let view = self.runtime_type(TypeIndex::from_raw(index), context)?;
                        if !matches!(self.pool().get(view).kind, TypeKind::Trait { .. }) {
                            return Err(fail("trait proof operand must name a trait view"));
                        }
                        if self.pool().trait_schema(view).is_none() {
                            return Err(fail(
                                "trait proof operand is missing its interface schema",
                            ));
                        }
                    }
                    ErrorOk | ErrorErr => {
                        register(dst)?;
                        register(base)?;
                        let Constant::Type(ty) = self.constant(index, context)? else {
                            return Err(fail("Error construction requires a Type constant"));
                        };
                        let ty = self.runtime_type(*ty, context)?;
                        let info = self.pool().get(ty);
                        let TypeKind::ErrorQualified { errors, .. } = &info.kind else {
                            return Err(fail("Error construction requires a qualified type"));
                        };
                        let already_checked = self
                            .validated_error_ctors
                            .borrow()
                            .get(ty.as_u32() as usize)
                            .copied()
                            .unwrap_or(false);
                        if !already_checked {
                            if info.size != 24
                                || info.align != 8
                                || errors.is_empty()
                                || self.pool().identity_input().is_none()
                                || self.pool().stable_type_id(ty).is_err()
                            {
                                return Err(fail(
                                    "Error construction requires authenticated executable metadata",
                                ));
                            }
                            for &member in errors {
                                if member != type_pool::Intrinsic::Any.type_index()
                                    && self.pool().stable_type_id(member).is_err()
                                {
                                    return Err(fail(
                                        "Error member requires a concrete stable identity",
                                    ));
                                }
                            }
                            if let Some(slot) = self
                                .validated_error_ctors
                                .borrow_mut()
                                .get_mut(ty.as_u32() as usize)
                            {
                                *slot = true;
                            }
                        }
                    }
                    ErrorIsOk | ErrorPayload => {
                        register(dst)?;
                        register(base)?;
                        if index != 0 {
                            return Err(fail("unused Error operands are nonzero"));
                        }
                    }
                    NewEnum | EnumIs => {
                        register(dst)?;
                        register(base)?;
                        self.enum_descriptor(index, context)?;
                    }
                    EnumField => {
                        register(dst)?;
                        register(base)?;
                    }
                    LoadField | StoreField => {
                        register(dst)?;
                        register(base)?;
                    }
                    LoadGlobal | LoadGlobalWide | StoreGlobal | StoreGlobalWide => {
                        register(dst)?;
                        let index =
                            if matches!(instruction.opcode, LoadGlobalWide | StoreGlobalWide) {
                                wide
                            } else {
                                if base.0 != 0 {
                                    return Err(fail("unused global base register is nonzero"));
                                }
                                index
                            };
                        if index as usize >= self.artifact.codegen_output.globals.len() {
                            return Err(fail("global slot outside schema"));
                        }
                    }
                    LoadSlot | StoreSlot => {
                        register(dst)?;
                        if wide >= slots {
                            return Err(fail("local slot outside entry allocation"));
                        }
                    }
                    NewObject => {
                        register(dst)?;
                        let ty = self.runtime_type(TypeIndex::from_raw(index), context)?;
                        if self.pool().is_reserved_collection_role(ty) {
                            return Err(fail(
                                "reserved collection roles require collection allocation",
                            ));
                        }
                        let words = match &self.pool().get(ty).kind {
                            TypeKind::Struct { fields, .. } => fields.len(),
                            TypeKind::Tuple { elements } => elements.len(),
                            TypeKind::Newtype { .. } => 1,
                            _ => {
                                return Err(fail(
                                    "object allocation requires struct, tuple, or newtype",
                                ));
                            }
                        };
                        if words > u16::MAX as usize {
                            return Err(fail("object payload exceeds header capacity"));
                        }
                    }
                    NewClosure | NewClosureWide => {
                        register(dst)?;
                        arguments(base.0)?;
                        let id = if instruction.opcode == NewClosureWide {
                            self.integer_u32(index, context)?
                        } else {
                            index
                        };
                        self.closure(id, base.0, context)?;
                    }
                    LoadIndex | StoreIndex => {
                        register(dst)?;
                        register(base)?;
                        if index >= 32 {
                            return Err(fail("index operand is not a register"));
                        }
                        register(Reg(index as u8))?;
                        if !self.has_list_roles && !self.has_map_roles {
                            return Err(fail(
                                "index instruction requires List or Map paired roles",
                            ));
                        }
                    }
                    NewList => {
                        register(dst)?;
                        if base.0 != 0 {
                            return Err(fail("unused list allocation operand is nonzero"));
                        }
                        if !self.has_list_roles {
                            return Err(fail(
                                "collection instruction requires List and Buffer roles",
                            ));
                        }
                    }
                    NewMap => {
                        register(dst)?;
                        if base.0 != 0 {
                            return Err(fail("unused map allocation operand is nonzero"));
                        }
                        if !self.has_map_roles {
                            return Err(fail("map instruction requires Map and MapBuffer roles"));
                        }
                    }
                    _ => return Err(fail("unexpected addressed opcode")),
                }
            }
            InstructionData::J { cond, offset } => {
                if !matches!(instruction.opcode, Jmp | JmpFar) {
                    register(cond)?;
                }
                let target = pc as i64 + offset as i64;
                if target < 0 || target as usize >= function.instructions.len() {
                    return Err(fail("jump target outside function"));
                }
                if target == 0 && function.instructions[0] >> 24 == AllocateSlots as u32 {
                    return Err(fail("jump reenters local slot allocation"));
                }
            }
            InstructionData::C { payload } => match instruction.opcode {
                Call | CallFar | CallBuiltin => {
                    let (count, id) = Instruction::c_arg_count_func_id(payload);
                    arguments(count)?;
                    if instruction.opcode != CallBuiltin {
                        let id = if instruction.opcode == CallFar {
                            self.integer_u32(id, context)?
                        } else {
                            id
                        };
                        self.call(id, count, context)?;
                    }
                }
                CallIndirect | CallIndirectProof => {
                    let (count, closure) = Instruction::c_call_indirect(payload);
                    arguments(count)?;
                    register(closure)?;
                    if payload & 0x1ff != 0 {
                        return Err(fail("unused indirect call bits are nonzero"));
                    }
                    if instruction.opcode == CallIndirectProof && closure.0 < count {
                        return Err(fail(
                            "physical indirect calls require a separate callee register",
                        ));
                    }
                }
                TraitCall => {
                    let (count, proof, slot) = Instruction::c_call_method(payload);
                    arguments(count)?;
                    register(proof)?;
                    if count == 0 || proof.0 < count {
                        return Err(fail(
                            "trait call requires receiver data and a separate proof register",
                        ));
                    }
                    if (slot as usize) >= self.max_trait_slots {
                        return Err(fail("trait call slot is outside every interface schema"));
                    }
                }
                CallMethod | CallMethodFar => {
                    let (count, receiver, method) = Instruction::c_call_method(payload);
                    arguments(count)?;
                    register(receiver)?;
                    if count >= function.register_count {
                        return Err(fail("method arguments leave no receiver register"));
                    }
                    self.string(
                        if instruction.opcode == CallMethodFar {
                            self.integer_u32(method, context)?
                        } else {
                            method
                        },
                        context,
                    )?;
                }
                Return => {
                    register(Instruction::c_return_reg(payload))?;
                    if payload & 0x1ffff != 0 {
                        return Err(fail("unused return bits are nonzero"));
                    }
                }
                ReturnUnit => {
                    if payload != 0 {
                        return Err(fail("unused return payload is nonzero"));
                    }
                }
                _ => return Err(fail("unexpected call opcode")),
            },
            InstructionData::E { payload } => match instruction.opcode {
                AllocateSlots => {
                    if pc != 0 || payload > MAX_SLOTS {
                        return Err(fail("slot allocation must be bounded and at entry"));
                    }
                }
                EffectCallDyn => {
                    let count = (payload >> 17) as u8;
                    arguments(count)?;
                    if self.effect(payload & 0x1ffff, context)? != count as usize {
                        return Err(fail("effect argument count mismatch"));
                    }
                }
                PushHandlerClosure | PushCapturingHandler => {
                    register(Reg((payload >> 17) as u8))?;
                    if instruction.opcode == PushCapturingHandler {
                        let metadata = self.integer(payload & 0x1ffff, context)?;
                        if metadata >> 32 >= 32
                            || metadata >> 32 > self.effect(metadata as u32, context)? as u64
                        {
                            return Err(fail(
                                "continuation parameter outside effect argument list",
                            ));
                        }
                    } else {
                        self.effect(payload & 0x1ffff, context)?;
                    }
                }
                ResetClosure => {
                    register(Instruction::e_control_regs(payload).0)?;
                    if payload & 0xfff != 0 {
                        return Err(fail("unused reset bits are nonzero"));
                    }
                }
                ResumeContinuation | ResumeContinuationOnce | AwaitFuture => {
                    let (first, second) = Instruction::e_control_regs(payload);
                    register(first)?;
                    register(second)?;
                    if payload & 0xfff != 0 {
                        return Err(fail("unused continuation bits are nonzero"));
                    }
                }
                PopHandler | AwaitPopHandler | Safepoint | Unreachable | Nop | MatchFail => {
                    if payload != 0 {
                        return Err(fail("unused system payload is nonzero"));
                    }
                }
                _ => return Err(fail("unexpected effect/system opcode")),
            },
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinImport, FuncId, FunctionAbi, GlobalInfo, MethodCallScope};
    use type_pool::Intrinsic;

    fn refresh_scopes(artifact: &mut CompiledArtifact) {
        artifact.codegen_output.method_call_scopes.clear();
        for function in &artifact.codegen_output.functions {
            for (pc, &word) in function.instructions.iter().enumerate() {
                if Instruction::decode(word)
                    .is_some_and(|inst| crate::instruction_requires_scope(inst.opcode))
                {
                    artifact
                        .codegen_output
                        .method_call_scopes
                        .push(MethodCallScope {
                            func_id: function.func_id,
                            pc: pc as u32,
                            scope: 0,
                        });
                }
            }
        }
    }

    fn artifact(instructions: Vec<Instruction>) -> CompiledArtifact {
        let mut type_pool = TypePool::with_intrinsics();
        type_pool
            .install_scopes(vec![type_pool::ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            }])
            .unwrap();
        let mut artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                method_call_scopes: vec![],
                functions: vec![CompiledFunction {
                    display_owner: None,
                    func_id: FuncId(0),
                    name: str_interner::intern("validation_entry"),
                    instructions: instructions.into_iter().map(Instruction::encode).collect(),
                    register_count: 32,
                    param_count: 0,
                    is_closure: false,
                    function_type: TypeIndex::INVALID,
                    abi: FunctionAbi::default(),
                    safepoint_pcs: vec![],
                }],
                constants: vec![],
                globals: vec![],
            },
            type_pool,
            entry: Some(FuncId(0)),
            builtin_abi_version: 9,
            builtins: vec![],
        };
        refresh_scopes(&mut artifact);
        artifact
    }

    fn rejected(artifact: &CompiledArtifact, message: &str) {
        let error = validate_artifact(artifact).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
    }

    #[test]
    fn associated_default_declarations_cannot_escape_into_executable_types() {
        let mut declaration = artifact(vec![Instruction::return_unit()]);
        let name = str_interner::intern("Item");
        let owner = declaration.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("CopySource"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let symbolic = declaration.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::AssociatedType {
                trait_owner: owner,
                name,
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let TypeKind::Trait { assoc_types, .. } = &mut declaration.type_pool.get_mut(owner).kind
        else {
            unreachable!()
        };
        assoc_types.push((name, symbolic));
        declaration
            .type_pool
            .register_associated_default(type_pool::AssociatedTypeDefault {
                trait_owner: owner,
                name,
                expression: type_pool::AssociatedTypeExpr::SelfType { trait_owner: owner },
            })
            .unwrap();
        // A valid, unused abstract declaration is allowed in the pool.
        validate_artifact(&declaration).unwrap();
        let optional = declaration
            .type_pool
            .intern_structural(TypeKind::Optional { inner: symbolic });
        let alias = declaration.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("AbstractItem"),
                target: optional,
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let template = declaration
            .type_pool
            .intern_iteration_step_template(symbolic)
            .unwrap();
        let self_template = declaration
            .type_pool
            .intern_iteration_step_template(owner)
            .unwrap();
        let nested = declaration.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![template, self_template],
        });
        let snapshot = declaration.type_pool.snapshot();
        let executable = || {
            let mut value = artifact(vec![Instruction::return_unit()]);
            value.type_pool = TypePool::restore(snapshot.clone()).unwrap();
            value
        };
        for ty in [symbolic, optional, alias, template, self_template, nested] {
            let mut global = executable();
            global.codegen_output.globals.push(GlobalInfo {
                type_index: ty,
                is_mutable: false,
            });
            rejected(&global, "unresolved associated type");

            let mut constant = executable();
            constant.codegen_output.constants.push(Constant::Type(ty));
            rejected(&constant, "unresolved associated type");

            let mut function = executable();
            let signature = function.type_pool.intern_structural(TypeKind::Function {
                params: vec![],
                ret: ty,
            });
            let entry = &mut function.codegen_output.functions[0];
            entry.function_type = signature;
            entry.abi = FunctionAbi::default();
            rejected(&function, "unresolved associated type");

            let mut operand = executable();
            operand.codegen_output.functions[0].instructions.insert(
                0,
                Instruction::a_type(
                    Opcode::TypeCheck,
                    AddrMode::Imm,
                    Reg(0),
                    Reg(0),
                    ty.as_u32() as u16,
                )
                .encode(),
            );
            refresh_scopes(&mut operand);
            rejected(&operand, "unresolved associated type");
        }
    }

    #[test]
    fn valid_globals_slots_and_builtin_manifest_are_checked_together() {
        let mut artifact = artifact(vec![
            Instruction::allocate_slots(1),
            Instruction::load_imm(Reg(0), 42),
            Instruction::store_slot(0, Reg(0)),
            Instruction::load_slot(Reg(0), 0),
            Instruction::store_global(0, Reg(0)),
            Instruction::call_builtin(0, 1),
            Instruction::return_unit(),
        ]);
        artifact.codegen_output.globals.push(GlobalInfo {
            type_index: Intrinsic::I64.type_index(),
            is_mutable: true,
        });
        artifact.builtins.push(BuiltinImport {
            id: 0,
            name: "print".into(),
        });
        validate_artifact(&artifact).unwrap();
        assert_eq!(
            builtin_references(&artifact.codegen_output).unwrap(),
            BTreeSet::from([0])
        );
        artifact.codegen_output.functions[0].instructions[4] =
            Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(1), 0).encode();
        rejected(&artifact, "unused global base register");
        artifact.codegen_output.functions[0].instructions[4] =
            Instruction::store_global(0, Reg(0)).encode();
        artifact.builtins.clear();
        rejected(&artifact, "undeclared builtin");
    }

    #[test]
    fn enum_descriptors_checked_loads_and_match_failure_are_validated() {
        let mut artifact = artifact(vec![Instruction::match_fail()]);
        let ty = artifact.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Enum {
                name: str_interner::intern("Option"),
                variants: vec![
                    type_pool::VariantInfo {
                        name: str_interner::intern("None"),
                        tag: 0,
                        fields: vec![],
                    },
                    type_pool::VariantInfo {
                        name: str_interner::intern("Some"),
                        tag: 7,
                        fields: vec![type_pool::FieldInfo {
                            name: str_interner::intern("value"),
                            ty: Intrinsic::I64.type_index(),
                            has_default: false,
                            offset: 0,
                        }],
                    },
                ],
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        artifact.codegen_output.constants = vec![
            Constant::Enum {
                type_index: ty,
                variant: 7,
            },
            Constant::Enum {
                type_index: ty,
                variant: 0,
            },
        ];
        artifact.codegen_output.functions[0].instructions = vec![
            Instruction::new_enum(Reg(0), Reg(1), 0).encode(),
            Instruction::enum_is(Reg(2), Reg(0), 0).encode(),
            Instruction::enum_field(Reg(3), Reg(0), 0).encode(),
            Instruction::match_fail().encode(),
        ];
        refresh_scopes(&mut artifact);
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::load_const(Reg(0), 0).encode();
        refresh_scopes(&mut artifact);
        rejected(&artifact, "payload enum descriptor cannot be loaded");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::load_const_wide(Reg(0), 0).encode();
        refresh_scopes(&mut artifact);
        rejected(&artifact, "payload enum descriptor cannot be loaded");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::load_const(Reg(0), 1).encode();
        refresh_scopes(&mut artifact);
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.constants[0] = Constant::Enum {
            type_index: ty,
            variant: 9,
        };
        rejected(&artifact, "unknown enum variant");
        artifact.codegen_output.constants[0] = Constant::Enum {
            type_index: Intrinsic::I64.type_index(),
            variant: 0,
        };
        rejected(&artifact, "requires an enum type");
        artifact.codegen_output.constants[0] = Constant::Int(7);
        rejected(&artifact, "requires an enum descriptor");
        artifact.codegen_output.functions[0].instructions =
            vec![Instruction::e_type(Opcode::MatchFail, 1).encode()];
        refresh_scopes(&mut artifact);
        rejected(&artifact, "unused system payload");
    }

    #[test]
    fn proof_operands_require_explicit_layout_schema_and_separate_registers() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        let view = artifact.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("ProofInterface"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let declaration = artifact.type_pool.intern_structural(TypeKind::Function {
            params: vec![view],
            ret: Intrinsic::Unit.type_index(),
        });
        artifact
            .type_pool
            .register_trait_schema(type_pool::TraitDispatchSchema {
                trait_type: view,
                slots: vec![type_pool::TraitMethodKey {
                    trait_owner: view,
                    name: str_interner::intern("run"),
                    signature: Some(type_pool::TraitMethodSignature {
                        associated_paths: Vec::new(),
                        declaration,
                        self_paths: vec![vec![type_pool::TraitTypeStep::Parameter(0)]],
                        parameter_kinds: vec![type_pool::TraitParameterKind::Receiver],
                    }),
                }],
            })
            .unwrap();
        artifact.codegen_output.functions[0].instructions = vec![
            Instruction::trait_proof(Reg(1), Reg(0), view).encode(),
            Instruction::return_unit().encode(),
        ];
        refresh_scopes(&mut artifact);
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::trait_proof(Reg(1), Reg(0), Intrinsic::I64.type_index()).encode();
        rejected(&artifact, "must name a trait view");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::trait_call(Reg(1), 1, 1).encode();
        rejected(&artifact, "outside every interface schema");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::trait_call(Reg(0), 0, 1).encode();
        rejected(&artifact, "separate proof register");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::trait_call(Reg(1), 0, 1).encode();
        validate_artifact(&artifact).unwrap();
    }

    #[test]
    fn specialized_self_proofs_require_concrete_signatures_and_implementations() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        artifact.entry = None;
        let view = artifact.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("SelfView"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: type_pool::TypeId(778, 1),
            size: 0,
            align: 0,
        });
        artifact
            .type_pool
            .register_trait_schema(type_pool::TraitDispatchSchema {
                trait_type: view,
                slots: vec![],
            })
            .unwrap();
        let concrete = Intrinsic::I64.type_index();
        artifact
            .type_pool
            .add_trait_impl(type_pool::TraitImplRecord {
                trait_type: view,
                implementor: concrete,
                methods: vec![],
                visible_scope: None,
            });
        artifact.type_pool.add_vtable(type_pool::VTable {
            trait_type: view,
            implementor: concrete,
            entries: vec![],
            visible_scope: None,
        });
        let signature = artifact.type_pool.intern_structural(TypeKind::Function {
            params: vec![concrete],
            ret: Intrinsic::Unit.type_index(),
        });
        let function = &mut artifact.codegen_output.functions[0];
        function.function_type = signature;
        function.param_count = 2;
        function.abi = FunctionAbi {
            captures: vec![],
            parameters: vec![ParameterAbi::TraitSelf { view }],
        };
        validate_artifact(&artifact).unwrap();
        let name = str_interner::intern("Item");
        let TypeKind::Trait { assoc_types, .. } = &mut artifact.type_pool.get_mut(view).kind else {
            panic!("fixture trait");
        };
        assoc_types.push((name, Intrinsic::Any.type_index()));
        rejected(&artifact, "concrete implementation");
        let mut snapshot = artifact.type_pool.snapshot();
        snapshot
            .associated_bindings
            .push(type_pool::AssociatedTypeBinding {
                implementor: concrete,
                trait_type: view,
                visible_scope: None,
                trait_owner: view,
                name,
                value: concrete,
            });
        artifact.type_pool = TypePool::restore(snapshot).unwrap();
        // An internal concrete entry has a fixed binding environment; a bare
        // associated associated-interface parameter remains rejected by its separate test.
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].function_type =
            artifact.type_pool.intern_structural(TypeKind::Function {
                params: vec![view],
                ret: Intrinsic::Unit.type_index(),
            });
        rejected(&artifact, "concrete implementation");
        artifact.codegen_output.functions[0].function_type = TypeIndex::INVALID;
        rejected(&artifact, "logical signature");
        artifact.codegen_output.functions[0].function_type = signature;
        artifact.codegen_output.functions[0].abi.parameters[0] = ParameterAbi::TraitSelf {
            view: artifact.type_pool.well_known.eq,
        };
        rejected(&artifact, "concrete implementation");
    }

    #[test]
    fn explicit_entry_layout_rejects_capture_signature_and_proof_corruption() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        artifact.entry = None;
        let signature = artifact.type_pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::Unit.type_index(),
        });
        let function = &mut artifact.codegen_output.functions[0];
        function.is_closure = true;
        function.param_count = 2;
        function.function_type = signature;
        function.abi = FunctionAbi {
            captures: vec![CaptureAbi::Value],
            parameters: vec![ParameterAbi::Value],
        };
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].abi.captures.clear();
        rejected(&artifact, "physical entry");
        let function = &mut artifact.codegen_output.functions[0];
        function.abi.parameters.push(ParameterAbi::Value);
        rejected(&artifact, "logical signature");
        let function = &mut artifact.codegen_output.functions[0];
        function.abi = FunctionAbi {
            captures: vec![CaptureAbi::Value],
            parameters: vec![ParameterAbi::Value],
        };
        function.is_closure = false;
        rejected(&artifact, "physical entry");
        let function = &mut artifact.codegen_output.functions[0];
        function.is_closure = true;
        function.function_type = TypeIndex::INVALID;
        function.abi = FunctionAbi {
            captures: vec![],
            parameters: vec![ParameterAbi::Trait {
                view: Intrinsic::I64.type_index(),
            }],
        };
        rejected(&artifact, "proof view must be a trait");
        artifact.codegen_output.functions[0].abi.parameters[0] = ParameterAbi::Trait {
            view: artifact.type_pool.well_known.eq,
        };
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].abi = FunctionAbi {
            captures: vec![
                CaptureAbi::Value,
                CaptureAbi::TraitProof {
                    view: TypeIndex::INVALID,
                },
            ],
            parameters: vec![],
        };
        rejected(&artifact, "invalid type index");
        artifact.codegen_output.functions[0].abi = FunctionAbi {
            captures: vec![
                CaptureAbi::TraitProof {
                    view: artifact.type_pool.well_known.eq,
                },
                CaptureAbi::Value,
            ],
            parameters: vec![],
        };
        rejected(&artifact, "must follow its data capture");
    }

    #[test]
    fn rejects_sparse_duplicate_and_invalid_entry_functions() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        artifact.codegen_output.functions[0].func_id = FuncId(u32::MAX);
        rejected(&artifact, "dense");
        artifact.codegen_output.functions[0].func_id = FuncId(0);
        artifact.entry = Some(FuncId(1));
        rejected(&artifact, "unknown function");
        artifact.entry = Some(FuncId(0));
        artifact.codegen_output.functions[0].param_count = 1;
        artifact.codegen_output.functions[0].abi = FunctionAbi::value_only(1);
        rejected(&artifact, "entry must");
    }

    #[test]
    fn associated_trait_views_cannot_forge_a_parameter_proof_abi() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        let view = artifact.type_pool.well_known.eq;
        let signature = artifact.type_pool.intern_structural(TypeKind::Function {
            params: vec![view],
            ret: Intrinsic::Unit.type_index(),
        });
        artifact.entry = None;
        let function = &mut artifact.codegen_output.functions[0];
        function.function_type = signature;
        function.param_count = 2;
        function.abi = FunctionAbi {
            captures: vec![],
            parameters: vec![ParameterAbi::Trait { view }],
        };
        validate_artifact(&artifact).unwrap();
        let TypeKind::Trait { assoc_types, .. } = &mut artifact.type_pool.get_mut(view).kind else {
            panic!();
        };
        assoc_types.push((str_interner::intern("Item"), Intrinsic::Any.type_index()));
        rejected(&artifact, "associated trait proof ABI is not supported");
        artifact.codegen_output.functions[0].function_type = TypeIndex::INVALID;
        rejected(&artifact, "associated trait proof ABI is not supported");
    }

    #[test]
    fn rejects_invalid_types_constants_registers_and_reserved_bits() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        artifact
            .codegen_output
            .constants
            .push(Constant::Type(TypeIndex::INVALID));
        rejected(&artifact, "invalid type index");
        artifact.codegen_output.constants.clear();
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::load_const(Reg(0), 42).encode();
        rejected(&artifact, "constant index");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::mov(Reg(2), Reg(0)).encode();
        artifact.codegen_output.functions[0].register_count = 2;
        rejected(&artifact, "register outside");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::return_unit().encode() | (3 << 22);
        rejected(&artifact, "reserved instruction bits");
        artifact.codegen_output.functions[0].instructions[0] = 0xff000000;
        rejected(&artifact, "invalid instruction");
    }

    #[test]
    fn rejects_slot_reallocation_and_out_of_bounds_jumps() {
        let bad_cases = [
            (
                vec![
                    Instruction::allocate_slots(1),
                    Instruction::load_slot(Reg(0), 1),
                    Instruction::return_unit(),
                ],
                "local slot",
            ),
            (
                vec![
                    Instruction::nop(),
                    Instruction::allocate_slots(1),
                    Instruction::return_unit(),
                ],
                "at entry",
            ),
            (
                vec![Instruction::jmp(-1), Instruction::return_unit()],
                "jump target",
            ),
            (
                vec![Instruction::allocate_slots(0), Instruction::jmp(-1)],
                "reenters",
            ),
            (vec![Instruction::nop()], "falls off"),
            (
                vec![
                    Instruction::e_type(Opcode::AllocateSlots, MAX_SLOTS + 1),
                    Instruction::return_unit(),
                ],
                "slot limit",
            ),
        ];
        for (instructions, message) in bad_cases {
            rejected(&artifact(instructions), message);
        }
    }

    #[test]
    fn validates_far_function_metadata_and_rejects_missing_default_arguments() {
        let mut artifact = artifact(vec![
            Instruction::call_far(0, 0),
            Instruction::return_unit(),
        ]);
        artifact.codegen_output.constants.push(Constant::UInt(1));
        let callee = CompiledFunction {
            display_owner: None,
            func_id: FuncId(1),
            name: str_interner::intern("default_parameter_function"),
            instructions: vec![Instruction::return_unit().encode()],
            register_count: 32,
            param_count: 1,
            is_closure: false,
            function_type: TypeIndex::INVALID,
            abi: FunctionAbi::value_only(1),
            safepoint_pcs: vec![],
        };
        artifact.codegen_output.functions.push(callee);
        rejected(&artifact, "supplies 0 parameters");
        artifact.codegen_output.functions[0].instructions[0] = Instruction::call_far(0, 1).encode();
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.constants[0] = Constant::UInt(u32::MAX as u64 + 1);
        rejected(&artifact, "exceeds u32");
        artifact.codegen_output.constants[0] = Constant::Int(-1);
        rejected(&artifact, "nonnegative");
    }

    #[test]
    fn checks_closure_capture_layout_and_signature() {
        let mut artifact = artifact(vec![
            Instruction::new_closure(Reg(2), 1, 1),
            Instruction::return_unit(),
        ]);
        let signature = artifact.type_pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        artifact.codegen_output.functions.push(CompiledFunction {
            display_owner: None,
            func_id: FuncId(1),
            name: str_interner::intern("checked_closure"),
            instructions: vec![Instruction::ret(Reg(0)).encode()],
            register_count: 32,
            param_count: 2,
            is_closure: true,
            function_type: signature,
            abi: FunctionAbi::closure_values(1, 1),
            safepoint_pcs: vec![],
        });
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::new_closure(Reg(2), 1, 2).encode();
        rejected(&artifact, "capture count");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::new_closure(Reg(2), 1, 1).encode();
        artifact.codegen_output.functions[1].abi = FunctionAbi::closure_values(2, 0);
        rejected(&artifact, "logical signature");
    }

    #[test]
    fn methods_use_checked_string_handles_and_reject_unresolved_derived_sentinel() {
        let mut artifact = artifact(vec![
            Instruction::call_method_far(Reg(0), 0, 0),
            Instruction::return_unit(),
        ]);
        artifact
            .codegen_output
            .constants
            .push(Constant::UInt(str_interner::intern("eq").as_u32() as u64));
        artifact.type_pool.add_method(
            Intrinsic::I64.type_index(),
            type_pool::MethodSlot {
                name: str_interner::intern("eq"),
                func_id: 0,
                trait_impl: None,
                visible_scope: None,
                access: type_pool::MethodAccess::Public,
            },
        );
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.constants[0] = Constant::UInt(u32::MAX as u64);
        rejected(&artifact, "invalid string ID");
        artifact.codegen_output.constants[0] =
            Constant::UInt(str_interner::intern("eq").as_u32() as u64);
        let mut snapshot = artifact.type_pool.snapshot();
        snapshot.methods[Intrinsic::I64.type_index().as_u32() as usize][0].func_id =
            type_pool::DERIVE_FUNC_ID;
        artifact.type_pool = TypePool::restore(snapshot).unwrap();
        rejected(&artifact, "unresolved derived method sentinel");
    }

    #[test]
    fn rejects_handler_stack_growth_underflow_and_validates_unreachable_terminator() {
        let mut artifact = artifact(vec![
            Instruction::e_type(Opcode::PopHandler, 0),
            Instruction::return_unit(),
        ]);
        rejected(&artifact, "handler stack underflow");
        let effect = artifact.type_pool.intern_structural(TypeKind::Effect {
            params: vec![],
            ret: Intrinsic::Unit.type_index(),
            is_async: false,
        });
        artifact.codegen_output.functions[0].instructions = vec![
            Instruction::push_handler_closure(effect, Reg(0)).encode(),
            Instruction::jmp(-1).encode(),
        ];
        rejected(&artifact, "inconsistent handler depth");
        artifact.codegen_output.functions[0].instructions =
            vec![Instruction::unreachable().encode()];
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].instructions =
            vec![Instruction::e_type(Opcode::Unreachable, 1).encode()];
        rejected(&artifact, "unused system payload");
    }

    #[test]
    fn collection_operands_require_roles_and_checked_index_registers() {
        let mut artifact = artifact(vec![
            Instruction::a_type(Opcode::NewList, AddrMode::Imm, Reg(0), Reg(0), 3),
            Instruction::a_type(Opcode::LoadIndex, AddrMode::Imm, Reg(1), Reg(0), 2),
            Instruction::a_type(Opcode::StoreIndex, AddrMode::Imm, Reg(1), Reg(0), 2),
            Instruction::return_unit(),
        ]);
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].register_count = 2;
        rejected(&artifact, "register outside");
        artifact.codegen_output.functions[0].register_count = 32;
        artifact.codegen_output.functions[0].instructions[1] =
            Instruction::a_type(Opcode::LoadIndex, AddrMode::Imm, Reg(1), Reg(0), 32).encode();
        rejected(&artifact, "index operand");
        artifact.codegen_output.functions[0].instructions[1] = Instruction::return_unit().encode();
        let list_index = artifact.type_pool.list_type().unwrap().as_u32() as usize;
        let mut snapshot = artifact.type_pool.snapshot();
        snapshot.types.truncate(list_index);
        snapshot.methods.truncate(list_index);
        artifact.type_pool = TypePool::restore(snapshot).unwrap();
        rejected(&artifact, "requires List and Buffer");
    }

    #[test]
    fn ordinary_allocations_cannot_forge_collection_layouts_even_through_aliases() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        for ty in [
            artifact.type_pool.list_type().unwrap(),
            artifact.type_pool.list_buffer_type().unwrap(),
            artifact.type_pool.map_type().unwrap(),
            artifact.type_pool.map_buffer_type().unwrap(),
            artifact.type_pool.bytes_type().unwrap(),
            artifact.type_pool.bytes_buffer_type().unwrap(),
        ] {
            artifact.codegen_output.functions[0].instructions[0] =
                Instruction::new_object(Reg(0), ty).encode();
            rejected(&artifact, "reserved collection roles");
        }
        let alias = artifact.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("ListAlias"),
                target: artifact.type_pool.list_type().unwrap(),
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::new_object(Reg(0), alias).encode();
        rejected(&artifact, "reserved collection roles");
    }

    #[test]
    fn rejects_invalid_handcrafted_type_pool_before_instruction_checks() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        artifact.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("broken_alias"),
                target: TypeIndex::INVALID,
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 1,
        });
        rejected(&artifact, "type pool:");
    }

    #[test]
    fn zero_capture_adapters_support_direct_calls_but_capturing_closures_do_not() {
        let mut artifact = artifact(vec![
            Instruction::new_closure(Reg(2), 1, 0),
            Instruction::call(1, 1),
            Instruction::return_unit(),
        ]);
        let signature = artifact.type_pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::Any.type_index()],
            ret: Intrinsic::Unit.type_index(),
        });
        artifact.codegen_output.functions.push(CompiledFunction {
            display_owner: None,
            func_id: FuncId(1),
            name: str_interner::intern("native_adapter"),
            instructions: vec![Instruction::return_unit().encode()],
            register_count: 32,
            param_count: 1,
            is_closure: true,
            function_type: signature,
            abi: FunctionAbi::closure_values(0, 1),
            safepoint_pcs: vec![],
        });
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[1].function_type = TypeIndex::INVALID;
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[1].abi = FunctionAbi::closure_values(1, 0);
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::new_closure(Reg(2), 1, 1).encode();
        rejected(&artifact, "requires a closure environment");
    }

    #[test]
    fn checks_capturing_handler_metadata_and_continuation_positions() {
        let mut artifact = artifact(vec![
            Instruction::push_capturing_handler(Reg(0), 0),
            Instruction::e_type(Opcode::PopHandler, 0),
            Instruction::return_unit(),
        ]);
        let effect = artifact.type_pool.intern_structural(TypeKind::Effect {
            params: vec![],
            ret: Intrinsic::Unit.type_index(),
            is_async: false,
        });
        artifact
            .codegen_output
            .constants
            .push(Constant::UInt(effect.as_u32() as u64));
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.constants[0] =
            Constant::UInt(effect.as_u32() as u64 | (32u64 << 32));
        rejected(&artifact, "continuation parameter");
        artifact.codegen_output.constants[0] = Constant::Int(-1);
        rejected(&artifact, "nonnegative");
    }

    #[test]
    fn checks_vtable_function_ids_and_rejects_unresolved_derived_sentinels() {
        let mut artifact = artifact(vec![Instruction::return_unit()]);
        let trait_type = artifact.type_pool.well_known.eq;
        artifact
            .type_pool
            .add_trait_impl(type_pool::TraitImplRecord {
                trait_type,
                visible_scope: None,
                implementor: Intrinsic::I64.type_index(),
                methods: vec![],
            });
        artifact.type_pool.add_vtable(type_pool::VTable {
            trait_type,
            visible_scope: None,
            implementor: Intrinsic::I64.type_index(),
            entries: vec![123456],
        });
        rejected(&artifact, "vtable: unknown function");
        let mut snapshot = artifact.type_pool.snapshot();
        snapshot.vtables[0].entries[0] = type_pool::DERIVE_FUNC_ID;
        artifact.type_pool = TypePool::restore(snapshot).unwrap();
        rejected(&artifact, "unresolved derived method sentinel");
        let mut snapshot = artifact.type_pool.snapshot();
        snapshot.vtables[0].entries[0] = 0;
        snapshot.trait_impls[0].methods.push(type_pool::MethodSlot {
            name: str_interner::intern("eq"),
            func_id: type_pool::DERIVE_FUNC_ID,
            trait_impl: Some(trait_type),
            visible_scope: None,
            access: type_pool::MethodAccess::Public,
        });
        artifact.type_pool = TypePool::restore(snapshot).unwrap();
        rejected(&artifact, "unresolved derived method sentinel");
    }

    #[test]
    fn map_allocation_requires_paired_roles_and_zero_unused_operand() {
        let mut artifact = artifact(vec![
            Instruction::a_type(Opcode::NewMap, AddrMode::Imm, Reg(0), Reg(0), 4095),
            Instruction::a_type(Opcode::LoadIndex, AddrMode::Imm, Reg(1), Reg(0), 2),
            Instruction::return_unit(),
        ]);
        validate_artifact(&artifact).unwrap();
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::a_type(Opcode::NewMap, AddrMode::Imm, Reg(0), Reg(1), 0).encode();
        rejected(&artifact, "unused map allocation operand");
        artifact.codegen_output.functions[0].instructions[0] =
            Instruction::a_type(Opcode::NewMap, AddrMode::Imm, Reg(0), Reg(0), 0).encode();
        let map_index = artifact.type_pool.map_type().unwrap().as_u32() as usize;
        let mut snapshot = artifact.type_pool.snapshot();
        snapshot.types.truncate(map_index);
        snapshot.methods.truncate(map_index);
        artifact.type_pool = TypePool::restore(snapshot).unwrap();
        rejected(&artifact, "requires Map and MapBuffer roles");
        let mut snapshot = TypePool::with_intrinsics().snapshot();
        let list_index = TypePool::with_intrinsics().list_type().unwrap().as_u32() as usize;
        for _ in 0..2 {
            snapshot.types.remove(list_index);
            snapshot.methods.remove(list_index);
        }
        artifact.type_pool = TypePool::restore(snapshot).unwrap();
        assert!(artifact.type_pool.list_type().is_none());
        validate_artifact(&artifact).unwrap();
    }
    #[test]
    fn a_known_function_cannot_replace_a_different_trait_slot() {
        for target in [0, 1] {
            let mut artifact = artifact(vec![Instruction::return_unit()]);
            let trait_type = artifact.type_pool.well_known.eq;
            let name = str_interner::intern("eq");
            artifact
                .type_pool
                .register_trait_schema(type_pool::TraitDispatchSchema {
                    trait_type,
                    slots: vec![type_pool::TraitMethodKey {
                        trait_owner: trait_type,
                        name,
                        signature: None,
                    }],
                })
                .unwrap();
            artifact
                .type_pool
                .add_trait_impl(type_pool::TraitImplRecord {
                    trait_type,
                    implementor: Intrinsic::I64.type_index(),
                    visible_scope: None,
                    methods: vec![type_pool::MethodSlot {
                        name,
                        func_id: 0,
                        trait_impl: Some(trait_type),
                        visible_scope: None,
                        access: type_pool::MethodAccess::Public,
                    }],
                });
            artifact.type_pool.add_vtable(type_pool::VTable {
                trait_type,
                implementor: Intrinsic::I64.type_index(),
                visible_scope: None,
                entries: vec![target],
            });
            artifact.codegen_output.functions.push(CompiledFunction {
                display_owner: None,
                func_id: FuncId(1),
                name: str_interner::intern("unrelated_function"),
                instructions: vec![Instruction::return_unit().encode()],
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: TypeIndex::INVALID,
                abi: FunctionAbi::default(),
                safepoint_pcs: vec![],
            });
            if target == 0 {
                validate_artifact(&artifact).unwrap();
            } else {
                assert!(validate_artifact(&artifact).is_err());
            }
        }
    }
    #[test]
    fn vtable_targets_must_implement_the_persisted_interface_signature() {
        for (parameters, result, valid) in [
            (
                vec![Intrinsic::I64.type_index(); 2],
                Intrinsic::Bool.type_index(),
                true,
            ),
            (
                vec![Intrinsic::I64.type_index(); 2],
                Intrinsic::I64.type_index(),
                false,
            ),
            (
                vec![Intrinsic::Bool.type_index(); 2],
                Intrinsic::Bool.type_index(),
                false,
            ),
            (
                vec![Intrinsic::I64.type_index()],
                Intrinsic::Bool.type_index(),
                false,
            ),
        ] {
            let mut artifact = artifact(vec![
                Instruction::load_true(Reg(0)),
                Instruction::ret(Reg(0)),
            ]);
            artifact.entry = None;
            let trait_type = artifact.type_pool.well_known.eq;
            let name = str_interner::intern("eq");
            let declaration = artifact.type_pool.intern_structural(TypeKind::Function {
                params: vec![trait_type; 2],
                ret: Intrinsic::Bool.type_index(),
            });
            let implementation = artifact.type_pool.intern_structural(TypeKind::Function {
                params: parameters.clone(),
                ret: result,
            });
            artifact.codegen_output.functions[0].function_type = implementation;
            artifact.codegen_output.functions[0].param_count = parameters.len() as u8;
            artifact.codegen_output.functions[0].abi = FunctionAbi::value_only(parameters.len());
            artifact
                .type_pool
                .register_trait_schema(type_pool::TraitDispatchSchema {
                    trait_type,
                    slots: vec![type_pool::TraitMethodKey {
                        trait_owner: trait_type,
                        name,
                        signature: Some(type_pool::TraitMethodSignature {
                            associated_paths: Vec::new(),
                            declaration,
                            self_paths: vec![
                                vec![type_pool::TraitTypeStep::Parameter(0)],
                                vec![type_pool::TraitTypeStep::Parameter(1)],
                            ],
                            parameter_kinds: vec![
                                type_pool::TraitParameterKind::Receiver,
                                type_pool::TraitParameterKind::Required,
                            ],
                        }),
                    }],
                })
                .unwrap();
            artifact
                .type_pool
                .add_trait_impl(type_pool::TraitImplRecord {
                    trait_type,
                    implementor: Intrinsic::I64.type_index(),
                    visible_scope: None,
                    methods: vec![type_pool::MethodSlot {
                        name,
                        func_id: 0,
                        trait_impl: Some(trait_type),
                        visible_scope: None,
                        access: type_pool::MethodAccess::Public,
                    }],
                });
            artifact.type_pool.add_vtable(type_pool::VTable {
                trait_type,
                implementor: Intrinsic::I64.type_index(),
                visible_scope: None,
                entries: vec![0],
            });
            if valid {
                validate_artifact(&artifact).unwrap();
            } else {
                rejected(&artifact, "SignatureMismatch");
            }
        }
    }
}
