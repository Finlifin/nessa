//! Explicit `Self` binders in persisted trait interface signatures.

use std::collections::HashSet;
use std::fmt;

use crate::{TraitMethodKey, TypeIndex, TypeInfo, TypeKind, TypePool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraitTypeStep {
    Parameter(u32),
    Return,
    TupleElement(u32),
    OptionalInner,
    ErrorInner,
    ErrorMember(u32),
    EffectInner,
    EffectMember(u32),
    IterationItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraitParameterKind {
    Receiver,
    Required,
    Optional,
    ListVariadic,
    MapVariadic,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraitAssociatedPath {
    pub trait_owner: TypeIndex,
    pub name: str_interner::StrId,
    pub path: Vec<TraitTypeStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraitMethodSignature {
    pub associated_paths: Vec<TraitAssociatedPath>,
    pub declaration: TypeIndex,
    pub self_paths: Vec<Vec<TraitTypeStep>>,
    pub parameter_kinds: Vec<TraitParameterKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraitSignatureError {
    MissingSignature,
    InvalidType(TypeIndex),
    NotFunction(TypeIndex),
    NotTrait(TypeIndex),
    InvalidParameterKinds,
    InvalidSelfPath,
    ExcessiveNesting,
    SignatureMismatch,
    InvalidAssociatedPath,
    MissingAssociatedBinding,
}
impl fmt::Display for TraitSignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid trait method signature: {self:?}")
    }
}
impl std::error::Error for TraitSignatureError {}

fn canonical(types: &[TypeInfo], mut ty: TypeIndex) -> Result<TypeIndex, TraitSignatureError> {
    for _ in 0..types.len() {
        match types.get(ty.as_u32() as usize).map(|info| &info.kind) {
            Some(TypeKind::Typealias { target, .. }) => ty = *target,
            Some(_) => return Ok(ty),
            None => break,
        }
    }
    Err(TraitSignatureError::InvalidType(ty))
}

fn child(kind: &TypeKind, step: TraitTypeStep) -> Option<TypeIndex> {
    match (kind, step) {
        (TypeKind::IterationStepTemplate { item }, TraitTypeStep::IterationItem) => Some(*item),
        (
            TypeKind::Function { params, .. } | TypeKind::Effect { params, .. },
            TraitTypeStep::Parameter(index),
        ) => params.get(index as usize).copied(),
        (TypeKind::Function { ret, .. } | TypeKind::Effect { ret, .. }, TraitTypeStep::Return) => {
            Some(*ret)
        }
        (TypeKind::Tuple { elements }, TraitTypeStep::TupleElement(index)) => {
            elements.get(index as usize).copied()
        }
        (TypeKind::Optional { inner }, TraitTypeStep::OptionalInner)
        | (TypeKind::ErrorQualified { inner, .. }, TraitTypeStep::ErrorInner)
        | (TypeKind::EffectQualified { inner, .. }, TraitTypeStep::EffectInner) => Some(*inner),
        (TypeKind::ErrorQualified { errors, .. }, TraitTypeStep::ErrorMember(index)) => {
            errors.get(index as usize).copied()
        }
        (TypeKind::EffectQualified { effects, .. }, TraitTypeStep::EffectMember(index)) => {
            effects.get(index as usize).copied()
        }
        _ => None,
    }
}

pub(crate) fn validate_signature(
    types: &[TypeInfo],
    key: &TraitMethodKey,
) -> Result<(), TraitSignatureError> {
    let Some(signature) = &key.signature else {
        return Ok(());
    };
    let declaration = canonical(types, signature.declaration)?;
    let TypeKind::Function { params, .. } = &types[declaration.as_u32() as usize].kind else {
        return Err(TraitSignatureError::NotFunction(signature.declaration));
    };
    if signature.parameter_kinds.len() != params.len()
        || signature
            .parameter_kinds
            .iter()
            .filter(|&&kind| kind == TraitParameterKind::Receiver)
            .count()
            > 1
    {
        return Err(TraitSignatureError::InvalidParameterKinds);
    }
    if signature.self_paths.len() > 256 {
        return Err(TraitSignatureError::ExcessiveNesting);
    }
    let owner = canonical(types, key.trait_owner)?;
    if !matches!(types[owner.as_u32() as usize].kind, TypeKind::Trait { .. }) {
        return Err(TraitSignatureError::NotTrait(key.trait_owner));
    }
    let mut paths = HashSet::new();
    for path in &signature.self_paths {
        if path.is_empty() || path.len() > 256 {
            return Err(TraitSignatureError::InvalidSelfPath);
        }
        if !paths.insert(path.as_slice()) {
            return Err(TraitSignatureError::InvalidSelfPath);
        }
        let mut ty = declaration;
        for &step in path {
            ty = canonical(types, ty)?;
            ty = child(&types[ty.as_u32() as usize].kind, step)
                .ok_or(TraitSignatureError::InvalidSelfPath)?;
        }
        if canonical(types, ty)? != owner {
            return Err(TraitSignatureError::InvalidSelfPath);
        }
    }
    for path in &signature.self_paths {
        for other in &signature.self_paths {
            if path.len() < other.len() && other.starts_with(path) {
                return Err(TraitSignatureError::InvalidSelfPath);
            }
        }
    }
    let mut all_paths = signature.self_paths.clone();
    if signature.associated_paths.len() > 256 {
        return Err(TraitSignatureError::ExcessiveNesting);
    }
    for marker in &signature.associated_paths {
        let owner = canonical(types, marker.trait_owner)?;
        let TypeKind::Trait { assoc_types, .. } = &types[owner.as_u32() as usize].kind else {
            return Err(TraitSignatureError::InvalidAssociatedPath);
        };
        if !crate::associated::ancestor(types, key.trait_owner, owner) {
            return Err(TraitSignatureError::InvalidAssociatedPath);
        }
        let default = assoc_types
            .iter()
            .find(|(name, _)| *name == marker.name)
            .ok_or(TraitSignatureError::InvalidAssociatedPath)?
            .1;
        if crate::associated::contains_trait(types, default)
            && !matches!(&types[default.as_u32() as usize].kind, TypeKind::AssociatedType {trait_owner, name} if *trait_owner == owner && *name == marker.name)
        {
            return Err(TraitSignatureError::InvalidAssociatedPath);
        }
        if marker.path.is_empty() || marker.path.len() > 256 {
            return Err(TraitSignatureError::InvalidAssociatedPath);
        }
        let mut ty = declaration;
        for &step in &marker.path {
            ty = canonical(types, ty)?;
            ty = child(&types[ty.as_u32() as usize].kind, step)
                .ok_or(TraitSignatureError::InvalidAssociatedPath)?;
        }
        if canonical(types, ty)? != canonical(types, default)?
            || all_paths
                .iter()
                .any(|path| path.starts_with(&marker.path) || marker.path.starts_with(path))
        {
            return Err(TraitSignatureError::InvalidAssociatedPath);
        }
        all_paths.push(marker.path.clone());
    }
    for (index, kind) in signature.parameter_kinds.iter().enumerate() {
        if *kind == TraitParameterKind::Receiver
            && !paths.contains([TraitTypeStep::Parameter(index as u32)].as_slice())
        {
            return Err(TraitSignatureError::InvalidParameterKinds);
        }
    }
    validate_symbolic_paths(types, signature)?;
    Ok(())
}

fn validate_symbolic_paths(
    types: &[TypeInfo],
    signature: &TraitMethodSignature,
) -> Result<(), TraitSignatureError> {
    let mut pending = vec![(signature.declaration, Vec::new())];
    let mut items = 0;
    while let Some((ty, path)) = pending.pop() {
        items += 1;
        if path.len() > 256 || items > 262_144 {
            return Err(TraitSignatureError::ExcessiveNesting);
        }
        let ty = canonical(types, ty)?;
        let kind = &types[ty.as_u32() as usize].kind;
        if let TypeKind::AssociatedType { trait_owner, name } = kind
            && !signature.associated_paths.iter().any(|marker| {
                marker.trait_owner == *trait_owner && marker.name == *name && marker.path == path
            })
        {
            return Err(TraitSignatureError::InvalidAssociatedPath);
        }
        let mut children = Vec::new();
        match kind {
            TypeKind::IterationStepTemplate { item } => {
                children.push((*item, TraitTypeStep::IterationItem))
            }
            TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. } => {
                children.extend(
                    params
                        .iter()
                        .enumerate()
                        .map(|(i, &ty)| (ty, TraitTypeStep::Parameter(i as u32))),
                );
                children.push((*ret, TraitTypeStep::Return));
            }
            TypeKind::Tuple { elements } => children.extend(
                elements
                    .iter()
                    .enumerate()
                    .map(|(i, &ty)| (ty, TraitTypeStep::TupleElement(i as u32))),
            ),
            TypeKind::Optional { inner } => children.push((*inner, TraitTypeStep::OptionalInner)),
            TypeKind::ErrorQualified { errors, inner } => {
                children.push((*inner, TraitTypeStep::ErrorInner));
                children.extend(
                    errors
                        .iter()
                        .enumerate()
                        .map(|(i, &ty)| (ty, TraitTypeStep::ErrorMember(i as u32))),
                );
            }
            TypeKind::EffectQualified { effects, inner } => {
                children.push((*inner, TraitTypeStep::EffectInner));
                children.extend(
                    effects
                        .iter()
                        .enumerate()
                        .map(|(i, &ty)| (ty, TraitTypeStep::EffectMember(i as u32))),
                );
            }
            _ => {}
        }
        for (ty, step) in children {
            let mut path = path.clone();
            path.push(step);
            pending.push((ty, path));
        }
    }
    Ok(())
}

// A transient normalized shape keeps qualification sets unordered without
// registering temporary types or mistaking a nominal type's fields for binders.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    IterationStep(Box<Self>),
    Binder,
    AssociatedBinder(u32, u32),
    Leaf(u32),
    Tuple(Vec<Self>),
    Function(Vec<Self>, Box<Self>),
    Effect(bool, Vec<Self>, Box<Self>),
    Optional(Box<Self>),
    Error(Vec<Self>, Box<Self>),
    QualifiedEffect(Vec<Self>, Box<Self>),
}

#[derive(Clone, Copy)]
enum SelfReplacement {
    Concrete(TypeIndex),
    Binder,
}

struct ShapeBuilder<'a> {
    types: &'a [TypeInfo],
    structural: &'a [TypeIndex],
    paths: &'a [Vec<TraitTypeStep>],
    replacement: Option<SelfReplacement>,
    associated: &'a [(Vec<TraitTypeStep>, TypeIndex)],
    associated_binders: &'a [TraitAssociatedPath],
    remaining: usize,
}

impl ShapeBuilder<'_> {
    fn build(
        &mut self,
        ty: TypeIndex,
        path: &mut Vec<TraitTypeStep>,
    ) -> Result<Shape, TraitSignatureError> {
        if path.len() > 256 || self.remaining == 0 {
            return Err(TraitSignatureError::ExcessiveNesting);
        }
        self.remaining -= 1;
        let ty = canonical(self.types, ty)?;
        if self.paths.iter().any(|marker| marker == path) {
            let replacement = match self.replacement {
                Some(SelfReplacement::Binder) => return Ok(Shape::Binder),
                Some(SelfReplacement::Concrete(ty)) => ty,
                None => ty,
            };
            let mut substituted = ShapeBuilder {
                types: self.types,
                structural: self.structural,
                paths: &[],
                replacement: None,
                associated: &[],
                associated_binders: &[],
                remaining: self.remaining,
            };
            let result = substituted.build(replacement, &mut Vec::new());
            self.remaining = substituted.remaining;
            return result;
        }
        if let Some((_, replacement)) = self.associated.iter().find(|(marker, _)| marker == path) {
            let mut substituted = ShapeBuilder {
                types: self.types,
                structural: self.structural,
                paths: &[],
                replacement: None,
                associated: &[],
                associated_binders: &[],
                remaining: self.remaining,
            };
            let result = substituted.build(*replacement, &mut Vec::new());
            self.remaining = substituted.remaining;
            return result;
        }
        if let Some(marker) = self
            .associated_binders
            .iter()
            .find(|marker| marker.path == *path)
        {
            return Ok(Shape::AssociatedBinder(
                marker.trait_owner.as_u32(),
                marker.name.as_u32(),
            ));
        }
        let kind = &self.types[ty.as_u32() as usize].kind;
        let shape = match kind {
            TypeKind::IterationStepTemplate { item } => Shape::IterationStep(Box::new(
                self.nested(*item, path, TraitTypeStep::IterationItem)?,
            )),
            TypeKind::Enum { .. } if self.structural.contains(&ty) => {
                let item = crate::iteration_step::step_payload(kind)
                    .ok_or(TraitSignatureError::InvalidType(ty))?;
                Shape::IterationStep(Box::new(self.nested(
                    item,
                    path,
                    TraitTypeStep::IterationItem,
                )?))
            }
            TypeKind::Tuple { elements } => {
                Shape::Tuple(self.sequence(elements, path, TraitTypeStep::TupleElement)?)
            }
            TypeKind::Function { params, ret } => Shape::Function(
                self.sequence(params, path, TraitTypeStep::Parameter)?,
                Box::new(self.nested(*ret, path, TraitTypeStep::Return)?),
            ),
            TypeKind::Effect {
                params,
                ret,
                is_async,
            } => {
                let affected = self.paths.iter().any(|marker| marker.starts_with(path))
                    || self
                        .associated
                        .iter()
                        .any(|(marker, _)| marker.starts_with(path))
                    || self
                        .associated_binders
                        .iter()
                        .any(|marker| marker.path.starts_with(path));
                if !affected && !self.structural.contains(&ty) {
                    return Ok(Shape::Leaf(ty.as_u32()));
                }
                Shape::Effect(
                    *is_async,
                    self.sequence(params, path, TraitTypeStep::Parameter)?,
                    Box::new(self.nested(*ret, path, TraitTypeStep::Return)?),
                )
            }
            TypeKind::Optional { inner } => Shape::Optional(Box::new(self.nested(
                *inner,
                path,
                TraitTypeStep::OptionalInner,
            )?)),
            TypeKind::ErrorQualified { errors, inner } => {
                let mut members = self.sequence(errors, path, TraitTypeStep::ErrorMember)?;
                let mut inner = self.nested(*inner, path, TraitTypeStep::ErrorInner)?;
                while let Shape::Error(nested, next) = inner {
                    members.extend(nested);
                    inner = *next;
                }
                members.sort_unstable();
                members.dedup();
                if members.is_empty() {
                    inner
                } else {
                    Shape::Error(members, Box::new(inner))
                }
            }
            TypeKind::EffectQualified { effects, inner } => {
                let mut members = self.sequence(effects, path, TraitTypeStep::EffectMember)?;
                members.sort_unstable();
                members.dedup();
                Shape::QualifiedEffect(
                    members,
                    Box::new(self.nested(*inner, path, TraitTypeStep::EffectInner)?),
                )
            }
            _ => Shape::Leaf(ty.as_u32()),
        };
        Ok(shape)
    }
    fn nested(
        &mut self,
        ty: TypeIndex,
        path: &mut Vec<TraitTypeStep>,
        step: TraitTypeStep,
    ) -> Result<Shape, TraitSignatureError> {
        path.push(step);
        let result = self.build(ty, path);
        path.pop();
        result
    }
    fn sequence(
        &mut self,
        types: &[TypeIndex],
        path: &mut Vec<TraitTypeStep>,
        step: fn(u32) -> TraitTypeStep,
    ) -> Result<Vec<Shape>, TraitSignatureError> {
        types
            .iter()
            .enumerate()
            .map(|(index, &ty)| self.nested(ty, path, step(index as u32)))
            .collect()
    }
}

impl TypePool {
    /// Check a child declaration against its inherited contract, preserving
    /// the distinction between a `Self` binder and an explicit trait annotation.
    /// Qualification members are compared as sets after symbolic substitution.
    pub fn check_trait_method_redeclaration(
        &self,
        inherited: &TraitMethodKey,
        redeclared: &TraitMethodKey,
    ) -> Result<(), TraitSignatureError> {
        let shape = |key: &TraitMethodKey| {
            let signature = key
                .signature
                .as_ref()
                .ok_or(TraitSignatureError::MissingSignature)?;
            validate_signature(&self.types, key)?;
            ShapeBuilder {
                types: &self.types,
                structural: &self.structural_types,
                paths: &signature.self_paths,
                replacement: Some(SelfReplacement::Binder),
                associated: &[],
                associated_binders: &signature.associated_paths,
                remaining: 262_144,
            }
            .build(signature.declaration, &mut Vec::new())
        };
        let left = shape(inherited)?;
        let right = shape(redeclared)?;
        if left == right
            && inherited.signature.as_ref().map(|s| &s.parameter_kinds)
                == redeclared.signature.as_ref().map(|s| &s.parameter_kinds)
        {
            Ok(())
        } else {
            Err(TraitSignatureError::SignatureMismatch)
        }
    }

    /// Specialize a declared interface for a concrete implementor or a more
    /// specific trait view. Only source-recorded `Self` binders are replaced;
    /// explicit trait annotations and unaffected nominal effects retain their
    /// identity. Invalid metadata returns before any types are registered.
    pub fn instantiate_trait_method_signature(
        &mut self,
        key: &TraitMethodKey,
        implementor: TypeIndex,
    ) -> Result<TypeIndex, TraitSignatureError> {
        self.instantiate_trait_method_signature_in_impl(key, implementor, key.trait_owner, None)
    }

    pub fn instantiate_trait_method_signature_in_impl(
        &mut self,
        key: &TraitMethodKey,
        implementor: TypeIndex,
        implementation_trait: TypeIndex,
        scope: Option<u32>,
    ) -> Result<TypeIndex, TraitSignatureError> {
        let signature = key
            .signature
            .as_ref()
            .ok_or(TraitSignatureError::MissingSignature)?;
        validate_signature(&self.types, key)?;
        let implementor = canonical(&self.types, implementor)?;
        let associated =
            self.associated_substitutions(signature, implementor, implementation_trait, scope)?;
        let shape = ShapeBuilder {
            types: &self.types,
            structural: &self.structural_types,
            paths: &signature.self_paths,
            replacement: Some(SelfReplacement::Concrete(implementor)),
            associated: &associated,
            associated_binders: &[],
            remaining: 262_144,
        }
        .build(signature.declaration, &mut Vec::new())?;
        self.intern_signature_shape(shape)
    }

    fn intern_signature_shape(&mut self, shape: Shape) -> Result<TypeIndex, TraitSignatureError> {
        let kind = match shape {
            Shape::IterationStep(item) => {
                let item = self.intern_signature_shape(*item)?;
                return self
                    .intern_iteration_step(item)
                    .map_err(|_| TraitSignatureError::SignatureMismatch);
            }
            // Only concrete substitution reaches the interning path.
            Shape::Binder | Shape::AssociatedBinder(_, _) => {
                unreachable!("symbolic signatures are compared without interning")
            }
            Shape::Leaf(index) => return Ok(TypeIndex::from_raw(index)),
            Shape::Tuple(elements) => TypeKind::Tuple {
                elements: self.intern_signature_shapes(elements)?,
            },
            Shape::Function(params, ret) => TypeKind::Function {
                params: self.intern_signature_shapes(params)?,
                ret: self.intern_signature_shape(*ret)?,
            },
            Shape::Effect(is_async, params, ret) => TypeKind::Effect {
                params: self.intern_signature_shapes(params)?,
                ret: self.intern_signature_shape(*ret)?,
                is_async,
            },
            Shape::Optional(inner) => TypeKind::Optional {
                inner: self.intern_signature_shape(*inner)?,
            },
            Shape::Error(errors, inner) => TypeKind::ErrorQualified {
                errors: self.intern_signature_shapes(errors)?,
                inner: self.intern_signature_shape(*inner)?,
            },
            Shape::QualifiedEffect(effects, inner) => TypeKind::EffectQualified {
                effects: self.intern_signature_shapes(effects)?,
                inner: self.intern_signature_shape(*inner)?,
            },
        };
        if let TypeKind::ErrorQualified { errors, inner } = kind {
            self.normalize_error_type(errors, inner)
                .map_err(|_| TraitSignatureError::ExcessiveNesting)
        } else {
            Ok(self.intern_structural(kind))
        }
    }

    fn intern_signature_shapes(
        &mut self,
        shapes: Vec<Shape>,
    ) -> Result<Vec<TypeIndex>, TraitSignatureError> {
        shapes
            .into_iter()
            .map(|shape| self.intern_signature_shape(shape))
            .collect()
    }

    /// Verify an implementation's logical function signature, substituting only
    /// explicitly recorded `Self` occurrences. This is invariant matching, not
    /// gradual consistency or parameter/return variance. Parameter modes must
    /// also be checked against source declarations by the resolver.
    pub fn check_trait_method_signature(
        &self,
        key: &TraitMethodKey,
        implementor: TypeIndex,
        actual_function_type: TypeIndex,
    ) -> Result<(), TraitSignatureError> {
        self.check_trait_method_signature_in_impl(
            key,
            implementor,
            actual_function_type,
            key.trait_owner,
            None,
        )
    }

    pub fn check_trait_method_signature_in_impl(
        &self,
        key: &TraitMethodKey,
        implementor: TypeIndex,
        actual_function_type: TypeIndex,
        implementation_trait: TypeIndex,
        scope: Option<u32>,
    ) -> Result<(), TraitSignatureError> {
        let signature = key
            .signature
            .as_ref()
            .ok_or(TraitSignatureError::MissingSignature)?;
        validate_signature(&self.types, key)?;
        let implementor = canonical(&self.types, implementor)?;
        let actual = canonical(&self.types, actual_function_type)?;
        if !matches!(
            self.types[actual.as_u32() as usize].kind,
            TypeKind::Function { .. }
        ) {
            return Err(TraitSignatureError::NotFunction(actual_function_type));
        }
        let associated =
            self.associated_substitutions(signature, implementor, implementation_trait, scope)?;
        let mut expected = ShapeBuilder {
            types: &self.types,
            structural: &self.structural_types,
            paths: &signature.self_paths,
            replacement: Some(SelfReplacement::Concrete(implementor)),
            associated: &associated,
            associated_binders: &[],
            remaining: 262_144,
        };
        let expected = expected.build(signature.declaration, &mut Vec::new())?;
        let mut actual = ShapeBuilder {
            types: &self.types,
            structural: &self.structural_types,
            // Binders belong only to the declared interface. Applying its
            // paths to the implementation would erase nominal effect identity.
            paths: &[],
            replacement: None,
            associated: &[],
            associated_binders: &[],
            remaining: 262_144,
        };
        let actual = actual.build(actual_function_type, &mut Vec::new())?;
        if expected == actual {
            Ok(())
        } else {
            Err(TraitSignatureError::SignatureMismatch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Intrinsic, TraitDispatchSchema, TypeId};
    use TraitTypeStep::{OptionalInner, Parameter, Return, TupleElement};

    fn function(pool: &mut TypePool, params: Vec<TypeIndex>, ret: TypeIndex) -> TypeIndex {
        pool.intern_structural(TypeKind::Function { params, ret })
    }
    fn optional(pool: &mut TypePool, inner: TypeIndex) -> TypeIndex {
        pool.intern_structural(TypeKind::Optional { inner })
    }
    fn key(pool: &mut TypePool) -> TraitMethodKey {
        let owner = pool.well_known.eq;
        let optional_self = optional(pool, owner);
        let declaration = function(pool, vec![owner, owner, owner], optional_self);
        TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("interface"),
            signature: Some(TraitMethodSignature {
                associated_paths: Vec::new(),
                declaration,
                self_paths: vec![
                    vec![Parameter(0)],
                    vec![Parameter(1)],
                    vec![Return, OptionalInner],
                ],
                parameter_kinds: vec![
                    TraitParameterKind::Receiver,
                    TraitParameterKind::Required,
                    TraitParameterKind::Required,
                ],
            }),
        }
    }
    fn alias(pool: &mut TypePool, target: TypeIndex) -> TypeIndex {
        pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("SignatureAlias"),
                target,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        })
    }

    #[test]
    fn redeclarations_compare_symbolic_self_across_reordered_qualification_sets() {
        let mut pool = TypePool::with_intrinsics();
        let base = pool.well_known.eq;
        let mut register_trait = |name| {
            pool.register(TypeInfo {
                kind: TypeKind::Trait {
                    name: str_interner::intern(name),
                    parents: Vec::new(),
                    assoc_types: Vec::new(),
                },
                type_id: TypeId::ZERO,
                size: 0,
                align: 0,
            })
        };
        let other = register_trait("Other");
        let child = register_trait("Child");
        let template = |pool: &mut TypePool, owner| {
            let ret = pool.intern_structural(TypeKind::ErrorQualified {
                errors: vec![owner, other],
                inner: Intrinsic::I64.type_index(),
            });
            let TypeKind::ErrorQualified { errors, .. } = &pool.get(ret).kind else {
                panic!("qualified result");
            };
            let member = errors.iter().position(|&ty| ty == owner).unwrap();
            let declaration = function(pool, vec![owner], ret);
            TraitMethodKey {
                trait_owner: owner,
                name: str_interner::intern("qualified"),
                signature: Some(TraitMethodSignature {
                    associated_paths: Vec::new(),
                    declaration,
                    self_paths: vec![
                        vec![Parameter(0)],
                        vec![Return, TraitTypeStep::ErrorMember(member as u32)],
                    ],
                    parameter_kinds: vec![TraitParameterKind::Receiver],
                }),
            }
        };
        let inherited = template(&mut pool, base);
        let mut redeclared = template(&mut pool, child);
        assert_ne!(
            inherited.signature.as_ref().unwrap().self_paths,
            redeclared.signature.as_ref().unwrap().self_paths
        );
        assert_eq!(
            pool.check_trait_method_redeclaration(&inherited, &redeclared),
            Ok(())
        );
        // The same concrete type index cannot stand in for a source Self binder.
        redeclared.signature.as_mut().unwrap().self_paths.pop();
        assert_eq!(
            pool.check_trait_method_redeclaration(&inherited, &redeclared),
            Err(TraitSignatureError::SignatureMismatch)
        );
    }

    #[test]
    fn instantiation_specializes_self_without_rewriting_explicit_trait_parameters() {
        let mut pool = TypePool::with_intrinsics();
        let key = key(&mut pool);
        for implementor in [Intrinsic::I64.type_index(), pool.well_known.partial_eq] {
            let specialized = pool
                .instantiate_trait_method_signature(&key, implementor)
                .unwrap();
            let TypeKind::Function { params, ret } = &pool.get(specialized).kind else {
                panic!("specialized signature must be a function");
            };
            assert_eq!(params, &[implementor, implementor, key.trait_owner]);
            assert!(
                matches!(pool.get(*ret).kind, TypeKind::Optional { inner } if inner == implementor)
            );
            assert_eq!(
                pool.check_trait_method_signature(&key, implementor, specialized),
                Ok(())
            );
            let count = pool.snapshot().types.len();
            assert_eq!(
                pool.instantiate_trait_method_signature(&key, implementor),
                Ok(specialized)
            );
            assert_eq!(pool.snapshot().types.len(), count);
        }
    }

    #[test]
    fn iteration_item_paths_do_not_grant_nominal_enums_self_substitution() {
        let mut pool = TypePool::with_intrinsics();
        let mut key = key(&mut pool);
        let owner = key.trait_owner;
        let template = pool.intern_iteration_step_template(owner).unwrap();
        let declaration = function(&mut pool, vec![owner], template);
        key.signature = Some(TraitMethodSignature {
            declaration,
            self_paths: vec![
                vec![Parameter(0)],
                vec![Return, TraitTypeStep::IterationItem],
            ],
            parameter_kinds: vec![TraitParameterKind::Receiver],
            associated_paths: vec![],
        });
        let specialized = pool
            .instantiate_trait_method_signature(&key, Intrinsic::I64.type_index())
            .unwrap();
        let TypeKind::Function { ret, .. } = pool.get(specialized).kind else {
            panic!()
        };
        assert_eq!(
            pool.checked_iteration_step_item(ret).unwrap(),
            Some(Intrinsic::I64.type_index())
        );

        let concrete = pool
            .intern_iteration_step(Intrinsic::I64.type_index())
            .unwrap();
        let mut nominal = pool.get(concrete).clone();
        let TypeKind::Enum { variants, .. } = &mut nominal.kind else {
            panic!()
        };
        variants[1].fields[0].ty = owner;
        let nominal = pool.register(nominal);
        let declaration = function(&mut pool, vec![owner], nominal);
        key.signature.as_mut().unwrap().declaration = declaration;
        let count = pool.len();
        assert_eq!(
            pool.instantiate_trait_method_signature(&key, Intrinsic::I64.type_index()),
            Err(TraitSignatureError::InvalidSelfPath)
        );
        assert_eq!(pool.len(), count);
        assert_eq!(pool.checked_iteration_step_item(nominal).unwrap(), None);
    }

    #[test]
    fn invalid_instantiation_does_not_register_partial_types() {
        let mut pool = TypePool::with_intrinsics();
        let mut key = key(&mut pool);
        let count = pool.snapshot().types.len();
        assert_eq!(
            pool.instantiate_trait_method_signature(&key, TypeIndex::INVALID),
            Err(TraitSignatureError::InvalidType(TypeIndex::INVALID))
        );
        key.signature
            .as_mut()
            .unwrap()
            .self_paths
            .push(vec![Return, Parameter(9)]);
        assert_eq!(
            pool.instantiate_trait_method_signature(&key, Intrinsic::I64.type_index()),
            Err(TraitSignatureError::InvalidSelfPath)
        );
        assert_eq!(pool.snapshot().types.len(), count);
        key.signature = None;
        assert_eq!(
            pool.instantiate_trait_method_signature(&key, Intrinsic::I64.type_index()),
            Err(TraitSignatureError::MissingSignature)
        );
        assert_eq!(pool.snapshot().types.len(), count);
    }

    #[test]
    fn self_substitution_preserves_explicit_trait_and_alias_identity() {
        let mut pool = TypePool::with_intrinsics();
        let key = key(&mut pool);
        let owner = key.trait_owner;
        let implementor = Intrinsic::I64.type_index();
        let implementor_alias = alias(&mut pool, implementor);
        let ret = optional(&mut pool, implementor);
        let actual = function(&mut pool, vec![implementor_alias, implementor, owner], ret);
        assert_eq!(
            pool.check_trait_method_signature(&key, implementor_alias, actual),
            Ok(())
        );
        let bad_explicit_trait =
            function(&mut pool, vec![implementor, implementor, implementor], ret);
        assert_eq!(
            pool.check_trait_method_signature(&key, implementor, bad_explicit_trait),
            Err(TraitSignatureError::SignatureMismatch)
        );
        let bad_any = function(
            &mut pool,
            vec![Intrinsic::Any.type_index(), implementor, owner],
            ret,
        );
        assert_eq!(
            pool.check_trait_method_signature(&key, implementor, bad_any),
            Err(TraitSignatureError::SignatureMismatch)
        );
        let bad_arity = function(&mut pool, vec![implementor, owner], ret);
        assert!(
            pool.check_trait_method_signature(&key, implementor, bad_arity)
                .is_err()
        );
        let bad_return = function(
            &mut pool,
            vec![implementor, implementor, owner],
            Intrinsic::Bool.type_index(),
        );
        assert!(
            pool.check_trait_method_signature(&key, implementor, bad_return)
                .is_err()
        );
    }

    #[test]
    fn nested_function_tuple_self_and_structural_implementors_are_checked() {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.well_known.eq;
        let tuple = pool.intern_structural(TypeKind::Tuple {
            elements: vec![owner, Intrinsic::I64.type_index()],
        });
        let nested = function(&mut pool, vec![tuple], owner);
        let declaration = function(&mut pool, vec![nested], owner);
        let key = TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("nested"),
            signature: Some(TraitMethodSignature {
                associated_paths: Vec::new(),
                declaration,
                self_paths: vec![
                    vec![Parameter(0), Parameter(0), TupleElement(0)],
                    vec![Parameter(0), Return],
                    vec![Return],
                ],
                parameter_kinds: vec![TraitParameterKind::Required],
            }),
        };
        let implementor = pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Bool.type_index(), Intrinsic::Str.type_index()],
        });
        let actual_tuple = pool.intern_structural(TypeKind::Tuple {
            elements: vec![implementor, Intrinsic::I64.type_index()],
        });
        let actual_nested = function(&mut pool, vec![actual_tuple], implementor);
        let actual = function(&mut pool, vec![actual_nested], implementor);
        assert_eq!(
            pool.check_trait_method_signature(&key, implementor, actual),
            Ok(())
        );
        assert_eq!(
            pool.instantiate_trait_method_signature(&key, implementor),
            Ok(actual)
        );
        let wrong_nested = function(&mut pool, vec![actual_tuple], Intrinsic::Any.type_index());
        let wrong = function(&mut pool, vec![wrong_nested], implementor);
        assert!(
            pool.check_trait_method_signature(&key, implementor, wrong)
                .is_err()
        );
    }

    #[test]
    fn effect_signatures_and_qualification_sets_preserve_identity_and_async_shape() {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.well_known.eq;
        let effect = pool.intern_structural(TypeKind::Effect {
            params: vec![owner],
            ret: owner,
            is_async: true,
        });
        let qualified = pool.intern_structural(TypeKind::ErrorQualified {
            errors: vec![owner, Intrinsic::Bool.type_index()],
            inner: effect,
        });
        let declaration = function(&mut pool, vec![qualified], Intrinsic::Unit.type_index());
        let error_index = match &pool.get(qualified).kind {
            TypeKind::ErrorQualified { errors, .. } => {
                errors.iter().position(|&ty| ty == owner).unwrap()
            }
            _ => unreachable!(),
        };
        let key = TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("effect_interface"),
            signature: Some(TraitMethodSignature {
                associated_paths: Vec::new(),
                declaration,
                parameter_kinds: vec![TraitParameterKind::Required],
                self_paths: vec![
                    vec![Parameter(0), TraitTypeStep::ErrorMember(error_index as u32)],
                    vec![Parameter(0), TraitTypeStep::ErrorInner, Parameter(0)],
                    vec![Parameter(0), TraitTypeStep::ErrorInner, Return],
                ],
            }),
        };
        let implementor = Intrinsic::I64.type_index();
        let effect = pool.intern_structural(TypeKind::Effect {
            params: vec![implementor],
            ret: implementor,
            is_async: true,
        });
        let qualified = pool.intern_structural(TypeKind::ErrorQualified {
            errors: vec![Intrinsic::Bool.type_index(), implementor],
            inner: effect,
        });
        let actual = function(&mut pool, vec![qualified], Intrinsic::Unit.type_index());
        assert_eq!(
            pool.check_trait_method_signature(&key, implementor, actual),
            Ok(())
        );
        let sync = pool.intern_structural(TypeKind::Effect {
            params: vec![implementor],
            ret: implementor,
            is_async: false,
        });
        let qualified = pool.intern_structural(TypeKind::ErrorQualified {
            errors: vec![implementor, Intrinsic::Bool.type_index()],
            inner: sync,
        });
        let wrong = function(&mut pool, vec![qualified], Intrinsic::Unit.type_index());
        assert!(
            pool.check_trait_method_signature(&key, implementor, wrong)
                .is_err()
        );

        // A named effect without a binder remains nominal, even if a distinct
        // named effect has exactly the same parameters and return type.
        let make_nominal = |pool: &mut TypePool| {
            pool.register(TypeInfo {
                kind: TypeKind::Effect {
                    params: vec![implementor],
                    ret: implementor,
                    is_async: false,
                },
                type_id: TypeId::ZERO,
                size: 0,
                align: 0,
            })
        };
        let left = make_nominal(&mut pool);
        let right = make_nominal(&mut pool);
        let declaration = function(&mut pool, vec![left], Intrinsic::Unit.type_index());
        let plain = TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("nominal_effect"),
            signature: Some(TraitMethodSignature {
                associated_paths: Vec::new(),
                declaration,
                self_paths: Vec::new(),
                parameter_kinds: vec![TraitParameterKind::Required],
            }),
        };
        assert_eq!(
            pool.instantiate_trait_method_signature(&plain, implementor),
            Ok(declaration)
        );
        let wrong = function(&mut pool, vec![right], Intrinsic::Unit.type_index());
        assert!(
            pool.check_trait_method_signature(&plain, implementor, wrong)
                .is_err()
        );
    }

    #[test]
    fn self_paths_never_erase_an_actual_nominal_effect_identity() {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.well_known.eq;
        let implementor = Intrinsic::I64.type_index();
        let declaration_effect = pool.intern_structural(TypeKind::Effect {
            params: vec![owner],
            ret: implementor,
            is_async: false,
        });
        let declaration = function(
            &mut pool,
            vec![declaration_effect],
            Intrinsic::Unit.type_index(),
        );
        let key = TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("effect_self"),
            signature: Some(TraitMethodSignature {
                associated_paths: Vec::new(),
                declaration,
                self_paths: vec![vec![Parameter(0), Parameter(0)]],
                parameter_kinds: vec![TraitParameterKind::Required],
            }),
        };
        let kind = TypeKind::Effect {
            params: vec![implementor],
            ret: implementor,
            is_async: false,
        };
        let structural_effect = pool.intern_structural(kind.clone());
        let structural = function(
            &mut pool,
            vec![structural_effect],
            Intrinsic::Unit.type_index(),
        );
        assert_eq!(
            pool.check_trait_method_signature(&key, implementor, structural),
            Ok(())
        );
        assert_eq!(
            pool.instantiate_trait_method_signature(&key, implementor),
            Ok(structural)
        );
        let nominal_effect = pool.register(TypeInfo {
            kind,
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let actual = function(
            &mut pool,
            vec![nominal_effect],
            Intrinsic::Unit.type_index(),
        );
        assert_eq!(
            pool.check_trait_method_signature(&key, implementor, actual),
            Err(TraitSignatureError::SignatureMismatch)
        );
    }

    #[test]
    fn static_self_return_and_legacy_signature_absence_remain_distinct() {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.well_known.eq;
        let declaration = function(&mut pool, Vec::new(), owner);
        let mut key = TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("zero"),
            signature: Some(TraitMethodSignature {
                associated_paths: Vec::new(),
                declaration,
                self_paths: vec![vec![Return]],
                parameter_kinds: Vec::new(),
            }),
        };
        let actual = function(&mut pool, Vec::new(), Intrinsic::I64.type_index());
        assert_eq!(
            pool.check_trait_method_signature(&key, Intrinsic::I64.type_index(), actual),
            Ok(())
        );
        key.signature = None;
        assert_eq!(
            pool.check_trait_method_signature(&key, Intrinsic::I64.type_index(), actual),
            Err(TraitSignatureError::MissingSignature)
        );
    }

    #[test]
    fn malformed_paths_modes_and_declarations_fail_atomically_and_on_restore() {
        let mut pool = TypePool::with_intrinsics();
        let valid = key(&mut pool);
        for paths in [
            vec![vec![]],
            vec![vec![Parameter(99)]],
            vec![vec![Return]],
            vec![vec![Parameter(0)], vec![Parameter(0)]],
            vec![vec![Parameter(0), OptionalInner]],
        ] {
            let mut invalid = valid.clone();
            invalid.signature.as_mut().unwrap().self_paths = paths;
            assert!(
                pool.register_trait_schema(TraitDispatchSchema {
                    trait_type: valid.trait_owner,
                    slots: vec![invalid]
                })
                .is_err()
            );
            assert!(pool.trait_schemas_snapshot().is_empty());
        }
        let mut invalid = valid.clone();
        invalid.signature.as_mut().unwrap().parameter_kinds.pop();
        assert!(validate_signature(&pool.types, &invalid).is_err());
        let mut invalid = valid.clone();
        invalid.signature.as_mut().unwrap().declaration = Intrinsic::I64.type_index();
        assert_eq!(
            validate_signature(&pool.types, &invalid),
            Err(TraitSignatureError::NotFunction(
                Intrinsic::I64.type_index()
            ))
        );
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: valid.trait_owner,
            slots: vec![valid],
        })
        .unwrap();
        let mut snapshot = pool.snapshot();
        snapshot.trait_schemas[0].slots[0]
            .signature
            .as_mut()
            .unwrap()
            .self_paths
            .push(vec![Parameter(2)]);
        // Param2 is deliberately explicit trait, so declaring another Self is
        // structurally valid metadata; source provenance is producer-owned.
        TypePool::restore(snapshot).unwrap();
        let mut snapshot = pool.snapshot();
        snapshot.trait_schemas[0].slots[0]
            .signature
            .as_mut()
            .unwrap()
            .self_paths
            .push(vec![Return, OptionalInner, OptionalInner]);
        assert!(TypePool::restore(snapshot).is_err());
    }
}
