//! Persistent source provenance for dependent associated type defaults.
use crate::{SnapshotError, TypeIndex, TypeInfo, TypeKind, TypePool};
use std::collections::{HashMap, HashSet};
use str_interner::StrId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssociatedTypeExpr {
    /// A declared associated type that every implementation must bind explicitly.
    Required,
    Concrete(TypeIndex),
    SelfType {
        trait_owner: TypeIndex,
    },
    Binding {
        trait_owner: TypeIndex,
        name: StrId,
    },
    Optional {
        inner: Box<Self>,
    },
    IterationStep {
        item: Box<Self>,
    },
    Tuple {
        elements: Vec<Self>,
    },
    Function {
        parameters: Vec<Self>,
        return_type: Box<Self>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociatedTypeDefault {
    pub trait_owner: TypeIndex,
    pub name: StrId,
    pub expression: AssociatedTypeExpr,
}

pub(crate) fn contains_symbolic(types: &[TypeInfo], root: TypeIndex) -> bool {
    contains_symbolic_inner(types, root, true)
}

fn contains_symbolic_inner(types: &[TypeInfo], root: TypeIndex, templates: bool) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(ty) = pending.pop() {
        if !seen.insert(ty) {
            continue;
        }
        match types.get(ty.as_u32() as usize).map(|info| &info.kind) {
            Some(TypeKind::IterationStepTemplate { item }) => {
                if templates {
                    return true;
                }
                pending.push(*item);
            }
            None | Some(TypeKind::AssociatedType { .. }) => return true,
            Some(TypeKind::Typealias { target, .. }) => pending.push(*target),
            Some(TypeKind::Newtype { inner, .. } | TypeKind::Optional { inner }) => {
                pending.push(*inner)
            }
            Some(TypeKind::Struct { fields, .. }) => pending.extend(fields.iter().map(|f| f.ty)),
            Some(TypeKind::Enum { variants, .. }) => {
                pending.extend(variants.iter().flat_map(|v| v.fields.iter().map(|f| f.ty)))
            }
            Some(TypeKind::Tuple { elements }) => pending.extend(elements),
            Some(TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. }) => {
                pending.extend(params);
                pending.push(*ret);
            }
            Some(TypeKind::ErrorQualified { errors, inner }) => {
                pending.extend(errors);
                pending.push(*inner);
            }
            Some(TypeKind::EffectQualified { effects, inner }) => {
                pending.extend(effects);
                pending.push(*inner);
            }
            _ => {}
        }
    }
    false
}
// Validate all nominal layouts in one graph pass, including recursive layouts.
// Rewalking a long chain from every descriptor would make legacy snapshots quadratic.
pub(crate) fn symbolic_types(types: &[TypeInfo]) -> HashSet<TypeIndex> {
    let mut reverse = vec![Vec::new(); types.len()];
    let mut pending = Vec::new();
    for (index, info) in types.iter().enumerate() {
        let source = TypeIndex::from_raw(index as u32);
        let mut dependencies = Vec::new();
        match &info.kind {
            TypeKind::AssociatedType { .. } | TypeKind::IterationStepTemplate { .. } => {
                pending.push(source)
            }
            TypeKind::Typealias { target, .. } => dependencies.push(*target),
            TypeKind::Newtype { inner, .. } | TypeKind::Optional { inner } => {
                dependencies.push(*inner)
            }
            TypeKind::Struct { fields, .. } => {
                dependencies.extend(fields.iter().map(|field| field.ty))
            }
            TypeKind::Enum { variants, .. } => dependencies.extend(
                variants
                    .iter()
                    .flat_map(|variant| variant.fields.iter().map(|field| field.ty)),
            ),
            TypeKind::Tuple { elements } => dependencies.extend(elements),
            TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. } => {
                dependencies.extend(params);
                dependencies.push(*ret);
            }
            TypeKind::ErrorQualified { errors, inner } => {
                dependencies.extend(errors);
                dependencies.push(*inner);
            }
            TypeKind::EffectQualified { effects, inner } => {
                dependencies.extend(effects);
                dependencies.push(*inner);
            }
            _ => {}
        }
        for dependency in dependencies {
            if let Some(dependants) = reverse.get_mut(dependency.as_u32() as usize) {
                dependants.push(source);
            } else {
                pending.push(source);
            }
        }
    }
    let mut result = HashSet::new();
    while let Some(ty) = pending.pop() {
        if result.insert(ty) {
            pending.extend(&reverse[ty.as_u32() as usize]);
        }
    }
    result
}
fn declared(types: &[TypeInfo], owner: TypeIndex, name: StrId) -> bool {
    matches!(types.get(owner.as_u32() as usize).map(|info|&info.kind),Some(TypeKind::Trait{assoc_types,..}) if assoc_types.iter().any(|(n,_)|*n==name))
}
fn validate_expr(
    types: &[TypeInfo],
    root: TypeIndex,
    expr: &AssociatedTypeExpr,
    depth: usize,
    items: &mut usize,
) -> Result<(), SnapshotError> {
    *items += 1;
    if depth >= 256 || *items > 262_144 {
        return Err(SnapshotError::new(
            "associated default expression exceeds metadata limits",
        ));
    }
    match expr {
        AssociatedTypeExpr::Required if depth == 0 => {}
        AssociatedTypeExpr::Required => {
            return Err(SnapshotError::new(
                "required associated declaration cannot be nested in a default expression",
            ));
        }
        AssociatedTypeExpr::Concrete(ty) => {
            if contains_symbolic(types, *ty) || crate::associated::contains_trait(types, *ty) {
                return Err(SnapshotError::new(
                    "associated default concrete type contains an abstract binder or trait view",
                ));
            }
        }
        AssociatedTypeExpr::SelfType { trait_owner } => {
            if !crate::associated::ancestor(types, root, *trait_owner) {
                return Err(SnapshotError::new(
                    "associated default Self owner is not a declaring trait ancestor",
                ));
            }
        }
        AssociatedTypeExpr::Binding { trait_owner, name } => {
            if !crate::associated::ancestor(types, root, *trait_owner)
                || !declared(types, *trait_owner, *name)
            {
                return Err(SnapshotError::new(
                    "associated default binding does not identify an ancestor declaration",
                ));
            }
        }
        AssociatedTypeExpr::IterationStep { item: inner }
        | AssociatedTypeExpr::Optional { inner } => {
            validate_expr(types, root, inner, depth + 1, items)?
        }
        AssociatedTypeExpr::Tuple { elements } => {
            for element in elements {
                validate_expr(types, root, element, depth + 1, items)?;
            }
        }
        AssociatedTypeExpr::Function {
            parameters,
            return_type,
        } => {
            for parameter in parameters {
                validate_expr(types, root, parameter, depth + 1, items)?;
            }
            validate_expr(types, root, return_type, depth + 1, items)?;
        }
    }
    Ok(())
}
pub(crate) fn validate_defaults(
    types: &[TypeInfo],
    defaults: &[AssociatedTypeDefault],
    complete: bool,
    allow_abstract_nominal: bool,
) -> Result<(), SnapshotError> {
    let mut keys = HashSet::new();
    let mut items = 0;
    for default in defaults {
        if !declared(types, default.trait_owner, default.name)
            || !keys.insert((default.trait_owner, default.name))
        {
            return Err(SnapshotError::new(
                "duplicate or foreign associated default declaration",
            ));
        }
        let Some(TypeInfo {
            kind: TypeKind::Trait { assoc_types, .. },
            ..
        }) = types.get(default.trait_owner.as_u32() as usize)
        else {
            return Err(SnapshotError::new(
                "associated template owner is not a trait",
            ));
        };
        let marker = assoc_types
            .iter()
            .find(|&&(name, _)| name == default.name)
            .ok_or_else(|| SnapshotError::new("unknown associated template"))?
            .1;
        if !matches!(types.get(marker.as_u32() as usize).map(|info|&info.kind),Some(TypeKind::AssociatedType{trait_owner,name}) if *trait_owner==default.trait_owner&&*name==default.name)
        {
            return Err(SnapshotError::new(
                "associated template requires a matching symbolic declaration",
            ));
        }
        validate_expr(
            types,
            default.trait_owner,
            &default.expression,
            0,
            &mut items,
        )?;
    }
    if complete {
        let symbolic = symbolic_types(types);
        for (index, info) in types.iter().enumerate() {
            if let TypeKind::AssociatedType { trait_owner, name } = info.kind
                && (!keys.contains(&(trait_owner, name))
                    || !matches!(types.get(trait_owner.as_u32() as usize).map(|info|&info.kind),Some(TypeKind::Trait{assoc_types,..}) if assoc_types.iter().any(|&(n,ty)|n==name&&ty.as_u32() as usize==index)))
            {
                return Err(SnapshotError::new(
                    "symbolic associated type has no matching default declaration",
                ));
            }
            if matches!(
                info.kind,
                TypeKind::Struct { .. } | TypeKind::Enum { .. } | TypeKind::Newtype { .. }
            ) && symbolic.contains(&TypeIndex::from_raw(index as u32))
                && !(allow_abstract_nominal && info.type_id == crate::TypeId::ZERO)
            {
                return Err(SnapshotError::new(
                    "runtime nominal fields contain an abstract associated type",
                ));
            }
        }
    }
    Ok(())
}
impl TypePool {
    /// Detect compiler-only associated binders and dependent templates, including
    /// a Self-only template with no AssociatedType leaf. Such types are never
    /// executable layouts, signatures, or Type values before specialization.
    pub fn contains_associated_type(&self, ty: TypeIndex) -> bool {
        contains_symbolic(&self.types, ty)
    }
    pub fn associated_defaults_snapshot(&self) -> &[AssociatedTypeDefault] {
        &self.associated_defaults
    }
    pub fn register_associated_default(
        &mut self,
        default: AssociatedTypeDefault,
    ) -> Result<(), SnapshotError> {
        self.invalidate_type_identities();
        let mut candidate = self.associated_defaults.clone();
        candidate.push(default);
        validate_defaults(&self.types, &candidate, false, false)?;
        self.associated_defaults = candidate;
        Ok(())
    }
}

impl TypePool {
    /// Resolve a new exact implementation's defaults. Overrides and inherited
    /// provider values are installed before dependency traversal, so an explicit
    /// override can break a declaration-default cycle.
    pub fn resolve_associated_defaults(
        &mut self,
        implementor: TypeIndex,
        trait_type: TypeIndex,
        scope: Option<u32>,
        overrides: &[(TypeIndex, StrId, TypeIndex)],
    ) -> Result<Vec<crate::AssociatedTypeBinding>, SnapshotError> {
        if scope.is_some_and(|scope| self.scopes.get(scope as usize).is_none()) {
            return Err(SnapshotError::new(
                "invalid associated implementation scope",
            ));
        }
        let implementor = self
            .canonical_type(implementor)
            .ok_or_else(|| SnapshotError::new("invalid associated implementor"))?;
        let trait_type = self
            .canonical_type(trait_type)
            .ok_or_else(|| SnapshotError::new("invalid associated trait"))?;
        let mut declared = Vec::new();
        let mut pending = vec![trait_type];
        let mut seen = HashSet::new();
        while let Some(owner) = pending.pop() {
            if !seen.insert(owner) {
                continue;
            }
            let Some(TypeInfo {
                kind:
                    TypeKind::Trait {
                        assoc_types,
                        parents,
                        ..
                    },
                ..
            }) = self.types.get(owner.as_u32() as usize)
            else {
                return Err(SnapshotError::new("associated defaults require a trait"));
            };
            declared.extend(assoc_types.iter().map(|&(name, ty)| (owner, name, ty)));
            pending.extend(parents);
        }
        let mut values = std::collections::HashMap::new();
        for &(owner, name, value) in overrides {
            if !declared.iter().any(|&(o, n, _)| o == owner && n == name)
                || values.insert((owner, name), value).is_some()
            {
                return Err(SnapshotError::new(
                    "duplicate or foreign associated override",
                ));
            }
            if !self.is_static_associated_type(value) {
                return Err(SnapshotError::new(
                    "associated override contains a trait view or abstract binder",
                ));
            }
        }
        for &(owner, name, _) in &declared {
            if owner == trait_type || values.contains_key(&(owner, name)) {
                continue;
            }
            let value = self.inherited_associated_value(implementor, owner, name, scope)?;
            values.insert((owner, name), value);
        }
        let mut resolver = DefaultResolver {
            pool: self,
            implementor,
            declared: &declared,
            values,
            active: HashSet::new(),
            items: 0,
        };
        for &(owner, name, default) in &declared {
            resolver.key((owner, name), default, 0)?;
        }
        Ok(declared
            .iter()
            .map(|&(trait_owner, name, _)| crate::AssociatedTypeBinding {
                implementor,
                trait_type,
                visible_scope: scope,
                trait_owner,
                name,
                value: resolver.values[&(trait_owner, name)],
            })
            .collect())
    }
    fn inherited_associated_value(
        &self,
        implementor: TypeIndex,
        owner: TypeIndex,
        name: StrId,
        scope: Option<u32>,
    ) -> Result<TypeIndex, SnapshotError> {
        let parent = match scope {
            Some(scope) => self
                .find_trait_impl_scoped(implementor, owner, scope)
                .map_err(|error| SnapshotError::new(error.to_string()))?,
            None => self.find_trait_impl(implementor, owner),
        };
        let missing = || {
            SnapshotError::new(
                "inherited associated type requires an exact visible parent implementation binding",
            )
        };
        let parent = parent.ok_or_else(missing)?;
        self.associated_bindings
            .iter()
            .find(|binding| {
                binding.implementor == implementor
                    && binding.trait_type == parent.trait_type
                    && binding.visible_scope == parent.visible_scope
                    && binding.trait_owner == owner
                    && binding.name == name
            })
            .map(|binding| binding.value)
            .ok_or_else(missing)
    }
}

struct DefaultResolver<'a> {
    pool: &'a mut TypePool,
    implementor: TypeIndex,
    declared: &'a [(TypeIndex, StrId, TypeIndex)],
    values: HashMap<(TypeIndex, StrId), TypeIndex>,
    active: HashSet<(TypeIndex, StrId)>,
    items: usize,
}
impl DefaultResolver<'_> {
    fn key(
        &mut self,
        key: (TypeIndex, StrId),
        legacy: TypeIndex,
        depth: usize,
    ) -> Result<TypeIndex, SnapshotError> {
        if let Some(&value) = self.values.get(&key) {
            return Ok(value);
        }
        if depth >= 256 {
            return Err(SnapshotError::new(
                "associated default dependency nesting exceeds 256",
            ));
        }
        if !self.active.insert(key) {
            return Err(SnapshotError::new(
                "cyclic associated type default dependency",
            ));
        }
        let expression = self
            .pool
            .associated_defaults
            .iter()
            .find(|default| (default.trait_owner, default.name) == key)
            .map(|default| default.expression.clone());
        let value = match expression {
            Some(expression) => self.expression(&expression, depth + 1)?,
            None if self.pool.is_static_associated_type(legacy) => legacy,
            None => {
                return Err(SnapshotError::new(
                    "associated declaration has no usable default",
                ));
            }
        };
        if !self.pool.is_static_associated_type(value) {
            return Err(SnapshotError::new(
                "associated default contains a trait view or abstract binder",
            ));
        }
        self.active.remove(&key);
        self.values.insert(key, value);
        Ok(value)
    }
    fn expression(
        &mut self,
        expression: &AssociatedTypeExpr,
        depth: usize,
    ) -> Result<TypeIndex, SnapshotError> {
        self.items += 1;
        if depth >= 256 || self.items > 262_144 {
            return Err(SnapshotError::new(
                "associated default expansion exceeds metadata limits",
            ));
        }
        Ok(match expression {
            AssociatedTypeExpr::Required => {
                return Err(SnapshotError::new(
                    "associated type requires an explicit implementation binding",
                ));
            }
            AssociatedTypeExpr::IterationStep { item } => {
                let item = self.expression(item, depth + 1)?;
                self.pool.intern_iteration_step(item)?
            }
            AssociatedTypeExpr::Concrete(ty) => *ty,
            AssociatedTypeExpr::SelfType { .. } => self.implementor,
            AssociatedTypeExpr::Binding { trait_owner, name } => {
                let default = self
                    .declared
                    .iter()
                    .find(|&&(o, n, _)| o == *trait_owner && n == *name)
                    .ok_or_else(|| SnapshotError::new("foreign associated dependency"))?
                    .2;
                self.key((*trait_owner, *name), default, depth + 1)?
            }
            AssociatedTypeExpr::Optional { inner } => {
                let inner = self.expression(inner, depth + 1)?;
                self.pool.intern_structural(TypeKind::Optional { inner })
            }
            AssociatedTypeExpr::Tuple { elements } => {
                let elements = elements
                    .iter()
                    .map(|element| self.expression(element, depth + 1))
                    .collect::<Result<_, _>>()?;
                self.pool.intern_structural(TypeKind::Tuple { elements })
            }
            AssociatedTypeExpr::Function {
                parameters,
                return_type,
            } => {
                let params = parameters
                    .iter()
                    .map(|element| self.expression(element, depth + 1))
                    .collect::<Result<_, _>>()?;
                let ret = self.expression(return_type, depth + 1)?;
                self.pool
                    .intern_structural(TypeKind::Function { params, ret })
            }
        })
    }
}

impl TypePool {
    /// Replace only symbolic associated binders using one exact implementation's
    /// already checked bindings. Equal concrete indices are never binders.
    pub fn specialize_associated_type(
        &mut self,
        ty: TypeIndex,
        bindings: &[crate::AssociatedTypeBinding],
    ) -> Result<TypeIndex, SnapshotError> {
        let mut values = HashMap::new();
        let identity = bindings.first().map(|binding| {
            (
                binding.implementor,
                binding.trait_type,
                binding.visible_scope,
            )
        });
        for binding in bindings {
            if identity
                != Some((
                    binding.implementor,
                    binding.trait_type,
                    binding.visible_scope,
                ))
            {
                return Err(SnapshotError::new(
                    "associated specialization mixes implementation identities",
                ));
            }
            if !self.is_static_associated_type(binding.value) {
                return Err(SnapshotError::new(
                    "associated specialization requires concrete binding values",
                ));
            }
            if values
                .insert((binding.trait_owner, binding.name), binding.value)
                .is_some()
            {
                return Err(SnapshotError::new(
                    "duplicate associated specialization binding",
                ));
            }
        }
        self.specialize_associated_inner(ty, &values, &mut HashMap::new(), 0)
    }
    fn specialize_associated_inner(
        &mut self,
        ty: TypeIndex,
        values: &HashMap<(TypeIndex, StrId), TypeIndex>,
        cache: &mut HashMap<TypeIndex, TypeIndex>,
        depth: usize,
    ) -> Result<TypeIndex, SnapshotError> {
        if let Some(&value) = cache.get(&ty) {
            return Ok(value);
        }
        if depth >= 256 {
            return Err(SnapshotError::new(
                "associated specialization exceeds nesting limit",
            ));
        }
        let canonical = self
            .canonical_type(ty)
            .ok_or_else(|| SnapshotError::new("invalid associated specialization type"))?;
        if !self.contains_associated_type(canonical) {
            return Ok(ty);
        }
        let kind = self.get(canonical).kind.clone();
        let result = match kind {
            TypeKind::IterationStepTemplate { item } => {
                let item = self.specialize_associated_inner(item, values, cache, depth + 1)?;
                if self.is_static_associated_type(item) {
                    self.intern_iteration_step(item)?
                } else {
                    self.intern_iteration_step_template(item)?
                }
            }
            TypeKind::AssociatedType { trait_owner, name } => {
                *values.get(&(trait_owner, name)).ok_or_else(|| {
                    SnapshotError::new("missing exact associated specialization binding")
                })?
            }
            TypeKind::Optional { inner } => {
                let inner = self.specialize_associated_inner(inner, values, cache, depth + 1)?;
                self.intern_structural(TypeKind::Optional { inner })
            }
            TypeKind::Tuple { elements } => {
                let elements = elements
                    .into_iter()
                    .map(|ty| self.specialize_associated_inner(ty, values, cache, depth + 1))
                    .collect::<Result<_, _>>()?;
                self.intern_structural(TypeKind::Tuple { elements })
            }
            TypeKind::Function { params, ret } => {
                let params = params
                    .into_iter()
                    .map(|ty| self.specialize_associated_inner(ty, values, cache, depth + 1))
                    .collect::<Result<_, _>>()?;
                let ret = self.specialize_associated_inner(ret, values, cache, depth + 1)?;
                self.intern_structural(TypeKind::Function { params, ret })
            }
            TypeKind::ErrorQualified { errors, inner } => {
                let errors = errors
                    .into_iter()
                    .map(|ty| self.specialize_associated_inner(ty, values, cache, depth + 1))
                    .collect::<Result<_, _>>()?;
                let inner = self.specialize_associated_inner(inner, values, cache, depth + 1)?;
                self.normalize_error_type(errors, inner)
                    .map_err(|error| SnapshotError::new(error.to_string()))?
            }
            TypeKind::EffectQualified { effects, inner } => {
                let effects = effects
                    .into_iter()
                    .map(|ty| self.specialize_associated_inner(ty, values, cache, depth + 1))
                    .collect::<Result<_, _>>()?;
                let inner = self.specialize_associated_inner(inner, values, cache, depth + 1)?;
                self.intern_structural(TypeKind::EffectQualified { effects, inner })
            }
            _ => {
                return Err(SnapshotError::new(
                    "abstract associated type in a runtime nominal layout",
                ));
            }
        };
        if contains_symbolic_inner(&self.types, result, false) {
            return Err(SnapshotError::new(
                "associated specialization result still contains an abstract binder",
            ));
        }
        cache.insert(ty, result);
        Ok(result)
    }
    /// Fill the method list of an exact record during compiler staging, before
    /// any table can be frozen or exposed to dispatch.
    pub fn complete_trait_impl(
        &mut self,
        record: crate::TraitImplRecord,
    ) -> Result<(), SnapshotError> {
        if self.vtables.iter().any(|table| {
            table.implementor == record.implementor
                && table.trait_type == record.trait_type
                && table.visible_scope == record.visible_scope
        }) {
            return Err(SnapshotError::new(
                "cannot change a published trait implementation",
            ));
        }
        let existing = self
            .trait_impls
            .iter_mut()
            .find(|existing| {
                existing.implementor == record.implementor
                    && existing.trait_type == record.trait_type
                    && existing.visible_scope == record.visible_scope
            })
            .ok_or_else(|| SnapshotError::new("no staged exact trait implementation"))?;
        *existing = record;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Intrinsic, TraitAssociatedPath, TraitMethodKey, TraitMethodSignature, TraitParameterKind,
        TraitSignatureError, TraitTypeStep, TypeId,
    };

    fn trait_with_bindings(pool: &mut TypePool, names: &[&str]) -> (TypeIndex, Vec<StrId>) {
        let owner = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("Defaults"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let names = names
            .iter()
            .map(|name| str_interner::intern(name))
            .collect::<Vec<_>>();
        for &name in &names {
            let marker = pool.register(TypeInfo {
                kind: TypeKind::AssociatedType {
                    trait_owner: owner,
                    name,
                },
                type_id: TypeId::ZERO,
                size: 0,
                align: 0,
            });
            let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind else {
                panic!("trait");
            };
            assoc_types.push((name, marker));
        }
        (owner, names)
    }
    #[test]
    fn required_declarations_need_explicit_bindings_and_cannot_be_nested() {
        let mut pool = TypePool::with_intrinsics();
        let (owner, names) = trait_with_bindings(&mut pool, &["Item"]);
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: names[0],
            expression: AssociatedTypeExpr::Required,
        })
        .unwrap();
        TypePool::restore(pool.snapshot()).unwrap();
        assert!(
            pool.resolve_associated_defaults(Intrinsic::I64.type_index(), owner, None, &[])
                .unwrap_err()
                .to_string()
                .contains("explicit implementation binding")
        );
        let bindings = pool
            .resolve_associated_defaults(
                Intrinsic::I64.type_index(),
                owner,
                None,
                &[(owner, names[0], Intrinsic::Bool.type_index())],
            )
            .unwrap();
        assert_eq!(bindings[0].value, Intrinsic::Bool.type_index());
        let mut snapshot = pool.snapshot();
        snapshot.associated_defaults[0].expression = AssociatedTypeExpr::Optional {
            inner: Box::new(AssociatedTypeExpr::Required),
        };
        assert!(TypePool::restore(snapshot).is_err());
    }

    #[test]
    fn source_binders_specialize_self_and_forward_defaults_without_rewriting_any() {
        let mut pool = TypePool::with_intrinsics();
        let (owner, names) = trait_with_bindings(&mut pool, &["Callback", "Item"]);
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: names[0],
            expression: AssociatedTypeExpr::Function {
                parameters: vec![
                    AssociatedTypeExpr::Binding {
                        trait_owner: owner,
                        name: names[1],
                    },
                    AssociatedTypeExpr::Concrete(Intrinsic::Any.type_index()),
                ],
                return_type: Box::new(AssociatedTypeExpr::Optional {
                    inner: Box::new(AssociatedTypeExpr::SelfType { trait_owner: owner }),
                }),
            },
        })
        .unwrap();
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: names[1],
            expression: AssociatedTypeExpr::Concrete(Intrinsic::I64.type_index()),
        })
        .unwrap();
        for implementor in [Intrinsic::Bool.type_index(), Intrinsic::Str.type_index()] {
            let rows = pool
                .resolve_associated_defaults(
                    implementor,
                    owner,
                    None,
                    &[(owner, names[1], Intrinsic::Char.type_index())],
                )
                .unwrap();
            let ty = rows.iter().find(|row| row.name == names[0]).unwrap().value;
            let TypeKind::Function { params, ret } = &pool.get(ty).kind else {
                panic!("function");
            };
            assert_eq!(
                params,
                &vec![Intrinsic::Char.type_index(), Intrinsic::Any.type_index()]
            );
            assert!(matches!(pool.get(*ret).kind,TypeKind::Optional{inner} if inner==implementor));
            assert!(!pool.contains_associated_type(ty));
        }
        pool.validate().unwrap();
    }
    #[test]
    fn uninstalled_cycles_are_preserved_and_overrides_break_only_the_selected_cycle() {
        let mut pool = TypePool::with_intrinsics();
        let (owner, names) = trait_with_bindings(&mut pool, &["A", "B"]);
        for (name, dependency) in [(names[0], names[1]), (names[1], names[0])] {
            pool.register_associated_default(AssociatedTypeDefault {
                trait_owner: owner,
                name,
                expression: AssociatedTypeExpr::Binding {
                    trait_owner: owner,
                    name: dependency,
                },
            })
            .unwrap();
        }
        pool.validate().unwrap();
        assert!(
            pool.resolve_associated_defaults(Intrinsic::I64.type_index(), owner, None, &[])
                .unwrap_err()
                .to_string()
                .contains("cyclic")
        );
        let rows = pool
            .resolve_associated_defaults(
                Intrinsic::I64.type_index(),
                owner,
                None,
                &[(owner, names[1], Intrinsic::Str.type_index())],
            )
            .unwrap();
        assert!(
            rows.iter()
                .all(|row| row.value == Intrinsic::Str.type_index())
        );
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(
            restored.associated_defaults_snapshot(),
            pool.associated_defaults_snapshot()
        );
        let mut bad = pool.snapshot();
        bad.associated_defaults.clear();
        assert!(TypePool::restore(bad).is_err());
        let mut bad = pool.snapshot();
        bad.associated_defaults[0].expression = AssociatedTypeExpr::Concrete(owner);
        assert!(TypePool::restore(bad).is_err());
    }
    #[test]
    fn abstract_signature_leaves_require_exact_associated_source_paths() {
        let mut pool = TypePool::with_intrinsics();
        let (owner, names) = trait_with_bindings(&mut pool, &["Item"]);
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: names[0],
            expression: AssociatedTypeExpr::SelfType { trait_owner: owner },
        })
        .unwrap();
        let TypeKind::Trait { assoc_types, .. } = &pool.get(owner).kind else {
            panic!("trait");
        };
        let marker = assoc_types[0].1;
        let declaration = pool.intern_structural(TypeKind::Function {
            params: vec![owner],
            ret: marker,
        });
        let mut key = TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("item"),
            signature: Some(TraitMethodSignature {
                declaration,
                self_paths: vec![vec![TraitTypeStep::Parameter(0)]],
                associated_paths: vec![],
                parameter_kinds: vec![TraitParameterKind::Receiver],
            }),
        };
        assert_eq!(
            crate::signatures::validate_signature(&pool.types, &key),
            Err(TraitSignatureError::InvalidAssociatedPath)
        );
        key.signature
            .as_mut()
            .unwrap()
            .associated_paths
            .push(TraitAssociatedPath {
                trait_owner: owner,
                name: names[0],
                path: vec![TraitTypeStep::Return],
            });
        crate::signatures::validate_signature(&pool.types, &key).unwrap();
    }
}
