//! Authentication of explicit compiler-generated Display traversal entries.

use type_pool::{Intrinsic, TypeIndex, TypeKind, TypePool};

use crate::{FuncId, FunctionAbi, ParameterAbi};

/// Validate an opted-in traversal without changing legacy functions' interpretation.
pub fn validate_display_owner(
    pool: &TypePool,
    id: FuncId,
    owner: TypeIndex,
    signature: TypeIndex,
    abi: Option<&FunctionAbi>,
    param_count: u8,
    is_closure: bool,
) -> Result<(), &'static str> {
    let owner = pool.canonical_type(owner).ok_or("invalid Display owner")?;
    if pool
        .checked_collection_role(owner)
        .map_err(|_| "invalid Display owner layout")?
        .is_some()
    {
        return Err("collection roles cannot own a generated Display traversal");
    }
    match &pool.get(owner).kind {
        TypeKind::Struct { fields, .. } if fields.len() <= u16::MAX as usize => {
            if pool.get(owner).size as usize != fields.len() * 8
                || pool.get(owner).align != 8
                || fields
                    .iter()
                    .enumerate()
                    .any(|(index, field)| field.offset as usize != index * 8)
            {
                return Err("invalid generated Display struct layout");
            }
        }
        TypeKind::Tuple { elements } if elements.len() <= u16::MAX as usize => {}
        TypeKind::Enum { variants, .. }
            if variants
                .iter()
                .all(|variant| variant.fields.len() < u16::MAX as usize) => {}
        _ => return Err("generated Display owner is not a supported aggregate"),
    }
    let Some(abi) = abi else {
        return Err("generated Display requires explicit ABI");
    };
    if is_closure
        || param_count != 1
        || !abi.captures.is_empty()
        || abi.parameters != [ParameterAbi::Value]
    {
        return Err("generated Display requires one concrete receiver and no captures");
    }
    let signature = pool
        .canonical_type(signature)
        .ok_or("invalid generated Display signature")?;
    let TypeKind::Function { params, ret } = &pool.get(signature).kind else {
        return Err("generated Display signature is not a function");
    };
    if params.len() != 1
        || pool.canonical_type(params[0]) != Some(owner)
        || pool.as_intrinsic(*ret) != Some(Intrinsic::Str)
    {
        return Err("generated Display signature must be fn(owner)->String");
    }
    // Bootstrap Display occupies the fixed trait prefix. Do not compare its
    // TypeId's second word with a relocated process-local string identifier.
    let display = pool.well_known.display;
    if display.as_u32() != Intrinsic::COUNT as u32
        || pool.canonical_type(display) != Some(display)
        || pool.get(display).type_id.0 != 0x4e45_5353_5452_4954
    {
        return Err("generated Display requires the trusted bootstrap interface");
    }
    let name = str_interner::intern("to_string");
    if !pool.trait_impls_snapshot().iter().any(|record| {
        record.visible_scope.is_none()
            && pool.canonical_type(record.implementor) == Some(owner)
            && record.trait_type == display
            && record.methods.iter().any(|method| {
                method.name == name
                    && method.func_id == id.0
                    && method.trait_impl == Some(display)
                    && method.visible_scope.is_none()
            })
    }) {
        return Err("generated Display owner does not authenticate its global target");
    }
    Ok(())
}
