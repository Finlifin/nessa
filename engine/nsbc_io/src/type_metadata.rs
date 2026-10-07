//! Versioned, checked type-pool payload. Container framing is handled elsewhere.

use std::io;

use str_interner::StrId;
use type_pool::{
    AssociatedTypeBinding, AssociatedTypeDefault, AssociatedTypeExpr, FieldInfo,
    IdentityPathSegment, Intrinsic, MethodAccess, MethodSlot, NominalTypeProvenance,
    PackageTypeContext, ScopeContext, TraitAssociatedPath, TraitDispatchSchema, TraitImplRecord,
    TraitMethodKey, TraitMethodSignature, TraitParameterKind, TraitTypeStep, TypeId,
    TypeIdentityInput, TypeIndex, TypeInfo, TypeKind, TypePool, TypePoolSnapshot, VTable,
    VariantInfo, WellKnownTraits,
};

const MAGIC: &[u8; 4] = b"TPOL";
const VERSION: u32 = 11;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 262_144;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn required_defaults(pool: &TypePool) -> bool {
    pool.associated_defaults_snapshot()
        .iter()
        .any(|default| matches!(default.expression, AssociatedTypeExpr::Required))
}

fn template_data(pool: &TypePool) -> bool {
    let types = (0..pool.len()).any(|index| {
        matches!(
            pool.get(TypeIndex::from_raw(index as u32)).kind,
            TypeKind::IterationStepTemplate { .. }
        )
    });
    let paths = pool
        .trait_schemas_snapshot()
        .iter()
        .flat_map(|schema| &schema.slots)
        .filter_map(|slot| slot.signature.as_ref())
        .any(|signature| {
            signature
                .self_paths
                .iter()
                .chain(signature.associated_paths.iter().map(|marker| &marker.path))
                .any(|path| path.contains(&TraitTypeStep::IterationItem))
        });
    let mut expressions: Vec<_> = pool
        .associated_defaults_snapshot()
        .iter()
        .map(|default| &default.expression)
        .collect();
    while let Some(expression) = expressions.pop() {
        match expression {
            AssociatedTypeExpr::IterationStep { .. } => return true,
            AssociatedTypeExpr::Optional { inner } => expressions.push(inner),
            AssociatedTypeExpr::Tuple { elements } => expressions.extend(elements),
            AssociatedTypeExpr::Function {
                parameters,
                return_type,
            } => {
                expressions.extend(parameters);
                expressions.push(return_type);
            }
            _ => {}
        }
    }
    types || paths
}

pub(crate) fn encode_pool(pool: &TypePool) -> io::Result<Vec<u8>> {
    let associated = !pool.associated_bindings_snapshot().is_empty()
        || pool.trait_schemas_snapshot().iter().any(|schema| {
            schema.slots.iter().any(|slot| {
                slot.signature
                    .as_ref()
                    .is_some_and(|signature| !signature.associated_paths.is_empty())
            })
        });
    let defaults = !pool.associated_defaults_snapshot().is_empty()
        || (0..pool.len()).any(|index| {
            matches!(
                pool.get(TypeIndex::from_raw(index as u32)).kind,
                TypeKind::AssociatedType { .. }
            )
        });
    encode_pool_revision(
        pool,
        if pool.identity_input().is_some() {
            11
        } else if required_defaults(pool) {
            10
        } else if template_data(pool) {
            9
        } else if pool.has_iteration_steps() {
            8
        } else if defaults {
            7
        } else if associated {
            6
        } else {
            5
        },
    )
}

fn encode_pool_revision(pool: &TypePool, revision: u32) -> io::Result<Vec<u8>> {
    // Public registration APIs can hold unfinished descriptors. An archive
    // writer must reject them before producing a persistable pool.
    pool.validate()
        .map_err(|error| invalid(error.to_string()))?;
    if revision < 11 && pool.identity_input().is_some() {
        return Err(invalid(
            "finalized type identity provenance requires TPOL11",
        ));
    }
    if revision >= 11 && pool.identity_input().is_none() {
        return Err(invalid("TPOL11 requires finalized identity provenance"));
    }
    if revision < 10 && required_defaults(pool) {
        return Err(invalid("required associated declarations require TPOL10"));
    }
    if revision < 9 && template_data(pool) {
        return Err(invalid("IterationStep templates require TPOL9"));
    }
    if revision < 8 && pool.has_iteration_steps() {
        return Err(invalid("IterationStep provenance requires TPOL8"));
    }
    let snapshot = pool.snapshot();
    let mut writer = Writer {
        revision,
        bytes: Vec::new(),
        items: 0,
    };
    writer.bytes(MAGIC)?;
    writer.u32(revision)?;
    writer.list(&snapshot.types, |writer, info| {
        writer.u64(info.type_id.hi())?;
        writer.u64(info.type_id.lo())?;
        writer.u32(info.size)?;
        writer.u32(info.align)?;
        writer.kind(&info.kind)
    })?;
    writer.list(&snapshot.structural_types, |writer, ty| writer.index(*ty))?;
    writer.list(&snapshot.methods, |writer, methods| {
        writer.list(methods, Writer::method)
    })?;
    writer.list(&snapshot.trait_impls, |writer, record| {
        writer.index(record.trait_type)?;
        writer.index(record.implementor)?;
        if revision >= 3 {
            writer.option(record.visible_scope)?;
        }
        writer.list(&record.methods, Writer::method)
    })?;
    writer.list(&snapshot.vtables, |writer, table| {
        writer.index(table.trait_type)?;
        writer.index(table.implementor)?;
        if revision >= 3 {
            writer.option(table.visible_scope)?;
        }
        writer.list(&table.entries, |writer, entry| writer.u32(*entry))
    })?;
    for ty in known_traits(snapshot.well_known) {
        writer.index(ty)?;
    }
    writer.index(snapshot.null_type)?;
    if revision >= 2 {
        writer.list(&snapshot.scopes, |writer, scope| {
            writer.option(scope.parent)?;
            writer.u32(scope.package)?;
            writer.option(scope.assoc_type.map(TypeIndex::as_u32))
        })?;
    }
    if revision >= 4 {
        writer.list(&snapshot.trait_schemas, |writer, schema| {
            writer.index(schema.trait_type)?;
            writer.list(&schema.slots, |writer, slot| {
                writer.index(slot.trait_owner)?;
                writer.name(slot.name)?;
                if revision >= 5 {
                    writer.trait_signature(slot.signature.as_ref())?;
                }
                Ok(())
            })
        })?;
    }
    if revision >= 6 {
        writer.list(&snapshot.associated_bindings, |writer, binding| {
            writer.index(binding.implementor)?;
            writer.index(binding.trait_type)?;
            writer.option(binding.visible_scope)?;
            writer.index(binding.trait_owner)?;
            writer.name(binding.name)?;
            writer.index(binding.value)
        })?;
    } else if !snapshot.associated_bindings.is_empty()
        || snapshot.trait_schemas.iter().any(|schema| {
            schema.slots.iter().any(|slot| {
                slot.signature
                    .as_ref()
                    .is_some_and(|signature| !signature.associated_paths.is_empty())
            })
        })
    {
        return Err(invalid("associated type metadata requires revision 6"));
    }
    if revision >= 7 {
        writer.list(&snapshot.associated_defaults, |writer, default| {
            writer.index(default.trait_owner)?;
            writer.name(default.name)?;
            writer.associated_expr(&default.expression, 0)
        })?;
    } else if !snapshot.associated_defaults.is_empty() {
        return Err(invalid("associated default templates require revision 7"));
    }
    if revision >= 11 {
        let input = snapshot
            .identity_input
            .as_ref()
            .ok_or_else(|| invalid("missing finalized identity input"))?;
        writer.u32(input.schema)?;
        writer.list(&input.packages, |writer, package| {
            writer.u32(package.identity_schema)?;
            writer.bytes(&package.identity)?;
            writer.text(&package.qualified_name)?;
            writer.text(&package.version)
        })?;
        writer.list(&input.declarations, |writer, declaration| {
            writer.index(declaration.type_index)?;
            writer.u32(declaration.package)?;
            writer.list(&declaration.path, |writer, segment| match segment {
                IdentityPathSegment::Named(name) => {
                    writer.u8(0)?;
                    writer.text(name)
                }
                IdentityPathSegment::Lexical { kind, ordinal } => {
                    writer.u8(1)?;
                    writer.u8(*kind)?;
                    writer.u32(*ordinal)
                }
            })?;
            writer.text(&declaration.last_stable_version)
        })?;
    }
    Ok(writer.bytes)
}

pub(crate) fn decode_pool(bytes: &[u8]) -> io::Result<TypePool> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("type metadata exceeds the byte limit"));
    }
    let mut reader = Reader {
        revision: VERSION,
        bytes,
        position: 0,
        items: 0,
    };
    if reader.take(4)? != MAGIC {
        return Err(invalid("unsupported type metadata header"));
    }
    reader.revision = reader.u32()?;
    if !(1..=VERSION).contains(&reader.revision) {
        return Err(invalid("unsupported type metadata header"));
    }
    let types = reader.list(25, |reader| {
        let type_id = TypeId::from_pair(reader.u64()?, reader.u64()?);
        let size = reader.u32()?;
        let align = reader.u32()?;
        let kind = reader.kind()?;
        Ok(TypeInfo {
            kind,
            type_id,
            size,
            align,
        })
    })?;
    let structural_types = reader.list(4, Reader::index)?;
    let methods = reader.list(4, |reader| reader.list(10, Reader::method))?;
    let trait_impls = reader.list(if reader.revision >= 3 { 13 } else { 12 }, |reader| {
        let trait_type = reader.index()?;
        let implementor = reader.index()?;
        let scope = if reader.revision >= 3 {
            reader.option()?
        } else {
            None
        };
        let methods = reader.list(10, Reader::method)?;
        let visible_scope = if reader.revision >= 3 {
            scope
        } else {
            let scope = methods.first().and_then(|method| method.visible_scope);
            if methods.iter().any(|method| method.visible_scope != scope) {
                return Err(invalid("legacy trait methods have inconsistent scopes"));
            }
            scope
        };
        Ok(TraitImplRecord {
            trait_type,
            implementor,
            methods,
            visible_scope,
        })
    })?;
    let vtables = reader.list(if reader.revision >= 3 { 13 } else { 12 }, |reader| {
        let trait_type = reader.index()?;
        let implementor = reader.index()?;
        let visible_scope = if reader.revision >= 3 {
            reader.option()?
        } else {
            let mut records = trait_impls.iter().filter(|record| {
                record.trait_type == trait_type && record.implementor == implementor
            });
            let record = records
                .next()
                .ok_or_else(|| invalid("legacy vtable has no matching trait implementation"))?;
            if records.next().is_some() {
                return Err(invalid(
                    "legacy vtable has ambiguous trait implementation scope",
                ));
            }
            record.visible_scope
        };
        Ok(VTable {
            trait_type,
            implementor,
            visible_scope,
            entries: reader.list(4, Reader::u32)?,
        })
    })?;
    let well_known = WellKnownTraits {
        display: reader.index()?,
        hash: reader.index()?,
        eq: reader.index()?,
        ord: reader.index()?,
        partial_eq: reader.index()?,
        partial_ord: reader.index()?,
        iterator: reader.index()?,
        into_iterator: reader.index()?,
    };
    let null_type = reader.index()?;
    let scopes = if reader.revision >= 2 {
        reader.list(6, |reader| {
            Ok(ScopeContext {
                parent: reader.option()?,
                package: reader.u32()?,
                assoc_type: reader.option()?.map(TypeIndex::from_raw),
            })
        })?
    } else {
        Vec::new()
    };
    let trait_schemas = if reader.revision >= 4 {
        reader.list(8, |reader| {
            Ok(TraitDispatchSchema {
                trait_type: reader.index()?,
                slots: reader.list(8, |reader| {
                    Ok(TraitMethodKey {
                        trait_owner: reader.index()?,
                        name: reader.name()?,
                        signature: if reader.revision >= 5 {
                            reader.trait_signature()?
                        } else {
                            None
                        },
                    })
                })?,
            })
        })?
    } else {
        Vec::new()
    };
    let associated_bindings = if reader.revision >= 6 {
        reader.list(21, |reader| {
            Ok(AssociatedTypeBinding {
                implementor: reader.index()?,
                trait_type: reader.index()?,
                visible_scope: reader.option()?,
                trait_owner: reader.index()?,
                name: reader.name()?,
                value: reader.index()?,
            })
        })?
    } else {
        Vec::new()
    };
    let associated_defaults = if reader.revision >= 7 {
        reader.list(9, |reader| {
            Ok(AssociatedTypeDefault {
                trait_owner: reader.index()?,
                name: reader.name()?,
                expression: reader.associated_expr(0)?,
            })
        })?
    } else {
        Vec::new()
    };
    let identity_input = if reader.revision >= 11 {
        let schema = reader.u32()?;
        let packages = reader.list(28, |reader| {
            let identity_schema = reader.u32()?;
            let mut identity = [0; 16];
            identity.copy_from_slice(reader.take(16)?);
            Ok(PackageTypeContext {
                identity_schema,
                identity,
                qualified_name: reader.text()?,
                version: reader.text()?,
            })
        })?;
        let declarations = reader.list(16, |reader| {
            Ok(NominalTypeProvenance {
                type_index: reader.index()?,
                package: reader.u32()?,
                path: reader.bounded_list(2, 256, |reader| match reader.u8()? {
                    0 => Ok(IdentityPathSegment::Named(reader.text()?)),
                    1 => Ok(IdentityPathSegment::Lexical {
                        kind: reader.u8()?,
                        ordinal: reader.u32()?,
                    }),
                    _ => Err(invalid("unknown identity path segment")),
                })?,
                last_stable_version: reader.text()?,
            })
        })?;
        Some(TypeIdentityInput {
            schema,
            packages,
            declarations,
        })
    } else {
        None
    };
    if reader.position != bytes.len() {
        return Err(invalid("trailing bytes in type metadata"));
    }
    if reader.revision < 8
        && structural_types.iter().any(|ty| {
            types
                .get(ty.as_u32() as usize)
                .is_some_and(|info| matches!(info.kind, TypeKind::Enum { .. }))
        })
    {
        return Err(invalid("IterationStep provenance requires TPOL8"));
    }
    TypePool::restore(TypePoolSnapshot {
        types,
        structural_types,
        methods,
        trait_impls,
        vtables,
        well_known,
        null_type,
        scopes,
        trait_schemas,
        associated_bindings,
        associated_defaults,
        identity_input,
    })
    .map_err(|error| invalid(error.to_string()))
}

fn known_traits(known: WellKnownTraits) -> [TypeIndex; 8] {
    [
        known.display,
        known.hash,
        known.eq,
        known.ord,
        known.partial_eq,
        known.partial_ord,
        known.iterator,
        known.into_iterator,
    ]
}

struct Writer {
    revision: u32,
    bytes: Vec<u8>,
    items: usize,
}

impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.bytes.len()) {
            return Err(invalid("type metadata exceeds the byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn u8(&mut self, value: u8) -> io::Result<()> {
        self.bytes(&[value])
    }
    fn u32(&mut self, value: u32) -> io::Result<()> {
        self.bytes(&value.to_le_bytes())
    }
    fn u64(&mut self, value: u64) -> io::Result<()> {
        self.bytes(&value.to_le_bytes())
    }
    fn index(&mut self, value: TypeIndex) -> io::Result<()> {
        self.u32(value.as_u32())
    }
    fn boolean(&mut self, value: bool) -> io::Result<()> {
        self.u8(u8::from(value))
    }
    fn option(&mut self, value: Option<u32>) -> io::Result<()> {
        self.boolean(value.is_some())?;
        if let Some(value) = value {
            self.u32(value)?;
        }
        Ok(())
    }
    fn text(&mut self, text: &str) -> io::Result<()> {
        self.u32(u32::try_from(text.len()).map_err(|_| invalid("identity string too long"))?)?;
        self.bytes(text.as_bytes())
    }
    fn name(&mut self, name: StrId) -> io::Result<()> {
        let text = str_interner::try_get(name).ok_or_else(|| invalid("invalid string handle"))?;
        self.u32(u32::try_from(text.len()).map_err(|_| invalid("name is too long"))?)?;
        self.bytes(text.as_bytes())
    }
    fn list<T>(
        &mut self,
        values: &[T],
        mut encode: impl FnMut(&mut Self, &T) -> io::Result<()>,
    ) -> io::Result<()> {
        self.items = self
            .items
            .checked_add(values.len())
            .ok_or_else(|| invalid("metadata count overflow"))?;
        if self.items > MAX_ITEMS {
            return Err(invalid("too many type metadata items"));
        }
        self.u32(u32::try_from(values.len()).map_err(|_| invalid("metadata count overflow"))?)?;
        for value in values {
            encode(self, value)?;
        }
        Ok(())
    }
    fn indices(&mut self, values: &[TypeIndex]) -> io::Result<()> {
        self.list(values, |writer, ty| writer.index(*ty))
    }
    fn trait_signature(&mut self, signature: Option<&TraitMethodSignature>) -> io::Result<()> {
        self.boolean(signature.is_some())?;
        let Some(signature) = signature else {
            return Ok(());
        };
        self.index(signature.declaration)?;
        self.list(&signature.self_paths, |writer, path| writer.type_path(path))?;
        self.list(&signature.parameter_kinds, |writer, kind| {
            writer.u8(match kind {
                TraitParameterKind::Receiver => 0,
                TraitParameterKind::Required => 1,
                TraitParameterKind::Optional => 2,
                TraitParameterKind::ListVariadic => 3,
                TraitParameterKind::MapVariadic => 4,
            })
        })?;
        if self.revision >= 6 {
            self.list(&signature.associated_paths, |writer, binding| {
                writer.index(binding.trait_owner)?;
                writer.name(binding.name)?;
                writer.type_path(&binding.path)
            })?;
        }
        Ok(())
    }
    fn type_path(&mut self, path: &[TraitTypeStep]) -> io::Result<()> {
        if path.len() > 256 {
            return Err(invalid("trait Self path exceeds the depth limit"));
        }
        self.list(path, |writer, step| {
            let (tag, index) = match step {
                TraitTypeStep::Parameter(index) => (0, Some(*index)),
                TraitTypeStep::Return => (1, None),
                TraitTypeStep::TupleElement(index) => (2, Some(*index)),
                TraitTypeStep::OptionalInner => (3, None),
                TraitTypeStep::ErrorInner => (4, None),
                TraitTypeStep::ErrorMember(index) => (5, Some(*index)),
                TraitTypeStep::EffectInner => (6, None),
                TraitTypeStep::EffectMember(index) => (7, Some(*index)),
                TraitTypeStep::IterationItem if writer.revision >= 9 => (8, None),
                TraitTypeStep::IterationItem => {
                    return Err(invalid("IterationItem requires TPOL9"));
                }
            };
            writer.u8(tag)?;
            if let Some(index) = index {
                writer.u32(index)?;
            }
            Ok(())
        })
    }
    fn fields(&mut self, fields: &[FieldInfo]) -> io::Result<()> {
        self.list(fields, |writer, field| {
            writer.name(field.name)?;
            writer.index(field.ty)?;
            writer.boolean(field.has_default)?;
            writer.u32(field.offset)
        })
    }
    fn method(&mut self, method: &MethodSlot) -> io::Result<()> {
        self.name(method.name)?;
        self.u32(method.func_id)?;
        self.option(method.trait_impl.map(TypeIndex::as_u32))?;
        self.option(method.visible_scope)?;
        if self.revision >= 2 {
            match method.access {
                MethodAccess::LegacyUnknown => self.u8(0)?,
                MethodAccess::Public => self.u8(1)?,
                MethodAccess::Package(package) => {
                    self.u8(2)?;
                    self.u32(package)?;
                }
                MethodAccess::Private(scope) => {
                    self.u8(3)?;
                    self.u32(scope)?;
                }
            }
        }
        Ok(())
    }
    fn associated_expr(&mut self, expression: &AssociatedTypeExpr, depth: usize) -> io::Result<()> {
        self.items += 1;
        if depth >= 256 || self.items > MAX_ITEMS {
            return Err(invalid(
                "associated default template exceeds nesting or item limits",
            ));
        }
        match expression {
            AssociatedTypeExpr::Required => {
                if self.revision < 10 {
                    return Err(invalid("required associated declarations require TPOL10"));
                }
                self.u8(7)
            }
            AssociatedTypeExpr::IterationStep { item } => {
                if self.revision < 9 {
                    return Err(invalid("IterationStep expression requires TPOL9"));
                }
                self.u8(6)?;
                self.associated_expr(item, depth + 1)
            }
            AssociatedTypeExpr::Concrete(ty) => {
                self.u8(0)?;
                self.index(*ty)
            }
            AssociatedTypeExpr::SelfType { trait_owner } => {
                self.u8(1)?;
                self.index(*trait_owner)
            }
            AssociatedTypeExpr::Binding { trait_owner, name } => {
                self.u8(2)?;
                self.index(*trait_owner)?;
                self.name(*name)
            }
            AssociatedTypeExpr::Optional { inner } => {
                self.u8(3)?;
                self.associated_expr(inner, depth + 1)
            }
            AssociatedTypeExpr::Tuple { elements } => {
                self.u8(4)?;
                self.list(elements, |writer, element| {
                    writer.associated_expr(element, depth + 1)
                })
            }
            AssociatedTypeExpr::Function {
                parameters,
                return_type,
            } => {
                self.u8(5)?;
                self.list(parameters, |writer, parameter| {
                    writer.associated_expr(parameter, depth + 1)
                })?;
                self.associated_expr(return_type, depth + 1)
            }
        }
    }
    fn kind(&mut self, kind: &TypeKind) -> io::Result<()> {
        match kind {
            TypeKind::IterationStepTemplate { item } => {
                if self.revision < 9 {
                    return Err(invalid("IterationStep template requires TPOL9"));
                }
                self.u8(14)?;
                self.index(*item)
            }
            TypeKind::Intrinsic(intrinsic) => {
                self.u8(0)?;
                self.u8(*intrinsic as u8)
            }
            TypeKind::Struct { name, fields } => {
                self.u8(1)?;
                self.name(*name)?;
                self.fields(fields)
            }
            TypeKind::Enum { name, variants } => {
                self.u8(2)?;
                self.name(*name)?;
                self.list(variants, |writer, variant| {
                    writer.name(variant.name)?;
                    writer.u32(variant.tag)?;
                    writer.fields(&variant.fields)
                })
            }
            TypeKind::Typealias { name, target } => {
                self.u8(3)?;
                self.name(*name)?;
                self.index(*target)
            }
            TypeKind::Newtype { name, inner } => {
                self.u8(4)?;
                self.name(*name)?;
                self.index(*inner)
            }
            TypeKind::Tuple { elements } => {
                self.u8(5)?;
                self.indices(elements)
            }
            TypeKind::Function { params, ret } => {
                self.u8(6)?;
                self.indices(params)?;
                self.index(*ret)
            }
            TypeKind::Effect {
                params,
                ret,
                is_async,
            } => {
                self.u8(7)?;
                self.indices(params)?;
                self.index(*ret)?;
                self.boolean(*is_async)
            }
            TypeKind::Optional { inner } => {
                self.u8(8)?;
                self.index(*inner)
            }
            TypeKind::ErrorQualified { errors, inner } => {
                self.u8(9)?;
                self.indices(errors)?;
                self.index(*inner)
            }
            TypeKind::EffectQualified { effects, inner } => {
                self.u8(10)?;
                self.indices(effects)?;
                self.index(*inner)
            }
            TypeKind::Module { name } => {
                self.u8(11)?;
                self.name(*name)
            }
            TypeKind::Trait {
                name,
                parents,
                assoc_types,
            } => {
                self.u8(12)?;
                self.name(*name)?;
                self.indices(parents)?;
                self.list(assoc_types, |writer, (name, default)| {
                    writer.name(*name)?;
                    writer.index(*default)
                })
            }
            TypeKind::AssociatedType { trait_owner, name } => {
                if self.revision < 7 {
                    return Err(invalid("symbolic associated types require revision 7"));
                }
                self.u8(13)?;
                self.index(*trait_owner)?;
                self.name(*name)
            }
        }
    }
}

struct Reader<'a> {
    revision: u32,
    bytes: &'a [u8],
    position: usize,
    items: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| invalid("metadata length overflow"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| invalid("truncated type metadata"))?;
        self.position = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
    fn u64(&mut self) -> io::Result<u64> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
    fn index(&mut self) -> io::Result<TypeIndex> {
        Ok(TypeIndex::from_raw(self.u32()?))
    }
    fn boolean(&mut self) -> io::Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("invalid boolean tag")),
        }
    }
    fn option(&mut self) -> io::Result<Option<u32>> {
        if self.boolean()? {
            Ok(Some(self.u32()?))
        } else {
            Ok(None)
        }
    }
    fn text(&mut self) -> io::Result<String> {
        let length = self.u32()? as usize;
        let text = std::str::from_utf8(self.take(length)?)
            .map_err(|_| invalid("invalid UTF-8 identity string"))?;
        Ok(text.to_owned())
    }
    fn name(&mut self) -> io::Result<StrId> {
        let length = self.u32()? as usize;
        let text =
            std::str::from_utf8(self.take(length)?).map_err(|_| invalid("invalid UTF-8 name"))?;
        Ok(str_interner::intern(text))
    }
    fn list<T>(
        &mut self,
        minimum: usize,
        decode: impl FnMut(&mut Self) -> io::Result<T>,
    ) -> io::Result<Vec<T>> {
        self.bounded_list(minimum, MAX_ITEMS, decode)
    }
    fn bounded_list<T>(
        &mut self,
        minimum: usize,
        maximum: usize,
        mut decode: impl FnMut(&mut Self) -> io::Result<T>,
    ) -> io::Result<Vec<T>> {
        let count = self.u32()? as usize;
        self.items = self
            .items
            .checked_add(count)
            .ok_or_else(|| invalid("metadata count overflow"))?;
        if count > maximum
            || self.items > MAX_ITEMS
            || count > (self.bytes.len() - self.position) / minimum
        {
            return Err(invalid("invalid or excessive type metadata count"));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| invalid("cannot allocate type metadata"))?;
        for _ in 0..count {
            values.push(decode(self)?);
        }
        Ok(values)
    }
    fn indices(&mut self) -> io::Result<Vec<TypeIndex>> {
        self.list(4, Reader::index)
    }
    fn fields(&mut self) -> io::Result<Vec<FieldInfo>> {
        self.list(13, |reader| {
            Ok(FieldInfo {
                name: reader.name()?,
                ty: reader.index()?,
                has_default: reader.boolean()?,
                offset: reader.u32()?,
            })
        })
    }
    fn method(&mut self) -> io::Result<MethodSlot> {
        Ok(MethodSlot {
            name: self.name()?,
            func_id: self.u32()?,
            trait_impl: self.option()?.map(TypeIndex::from_raw),
            visible_scope: self.option()?,
            access: if self.revision == 1 {
                MethodAccess::LegacyUnknown
            } else {
                match self.u8()? {
                    0 => MethodAccess::LegacyUnknown,
                    1 => MethodAccess::Public,
                    2 => MethodAccess::Package(self.u32()?),
                    3 => MethodAccess::Private(self.u32()?),
                    _ => return Err(invalid("invalid method access tag")),
                }
            },
        })
    }
    fn trait_signature(&mut self) -> io::Result<Option<TraitMethodSignature>> {
        if !self.boolean()? {
            return Ok(None);
        }
        let declaration = self.index()?;
        let self_paths = self.bounded_list(4, 256, Reader::type_path)?;
        let parameter_kinds = self.list(1, |reader| {
            Ok(match reader.u8()? {
                0 => TraitParameterKind::Receiver,
                1 => TraitParameterKind::Required,
                2 => TraitParameterKind::Optional,
                3 => TraitParameterKind::ListVariadic,
                4 => TraitParameterKind::MapVariadic,
                _ => return Err(invalid("invalid trait parameter kind tag")),
            })
        })?;
        let associated_paths = if self.revision >= 6 {
            self.bounded_list(12, 256, |reader| {
                Ok(TraitAssociatedPath {
                    trait_owner: reader.index()?,
                    name: reader.name()?,
                    path: reader.type_path()?,
                })
            })?
        } else {
            Vec::new()
        };
        Ok(Some(TraitMethodSignature {
            declaration,
            self_paths,
            parameter_kinds,
            associated_paths,
        }))
    }
    fn type_path(&mut self) -> io::Result<Vec<TraitTypeStep>> {
        self.bounded_list(1, 256, |reader| {
            Ok(match reader.u8()? {
                0 => TraitTypeStep::Parameter(reader.u32()?),
                1 => TraitTypeStep::Return,
                2 => TraitTypeStep::TupleElement(reader.u32()?),
                3 => TraitTypeStep::OptionalInner,
                4 => TraitTypeStep::ErrorInner,
                5 => TraitTypeStep::ErrorMember(reader.u32()?),
                6 => TraitTypeStep::EffectInner,
                7 => TraitTypeStep::EffectMember(reader.u32()?),
                8 if reader.revision >= 9 => TraitTypeStep::IterationItem,
                _ => return Err(invalid("invalid trait Self path step tag")),
            })
        })
    }
    fn associated_expr(&mut self, depth: usize) -> io::Result<AssociatedTypeExpr> {
        self.items += 1;
        if depth >= 256 || self.items > MAX_ITEMS {
            return Err(invalid(
                "associated default template exceeds nesting or item limits",
            ));
        }
        Ok(match self.u8()? {
            7 if self.revision >= 10 => AssociatedTypeExpr::Required,
            6 if self.revision >= 9 => AssociatedTypeExpr::IterationStep {
                item: Box::new(self.associated_expr(depth + 1)?),
            },
            0 => AssociatedTypeExpr::Concrete(self.index()?),
            1 => AssociatedTypeExpr::SelfType {
                trait_owner: self.index()?,
            },
            2 => AssociatedTypeExpr::Binding {
                trait_owner: self.index()?,
                name: self.name()?,
            },
            3 => AssociatedTypeExpr::Optional {
                inner: Box::new(self.associated_expr(depth + 1)?),
            },
            4 => AssociatedTypeExpr::Tuple {
                elements: self.list(1, |reader| reader.associated_expr(depth + 1))?,
            },
            5 => AssociatedTypeExpr::Function {
                parameters: self.list(1, |reader| reader.associated_expr(depth + 1))?,
                return_type: Box::new(self.associated_expr(depth + 1)?),
            },
            _ => return Err(invalid("invalid associated default expression tag")),
        })
    }
    fn kind(&mut self) -> io::Result<TypeKind> {
        Ok(match self.u8()? {
            14 if self.revision >= 9 => TypeKind::IterationStepTemplate {
                item: self.index()?,
            },
            0 => TypeKind::Intrinsic(
                *Intrinsic::ALL
                    .get(self.u8()? as usize)
                    .ok_or_else(|| invalid("invalid intrinsic tag"))?,
            ),
            1 => TypeKind::Struct {
                name: self.name()?,
                fields: self.fields()?,
            },
            2 => TypeKind::Enum {
                name: self.name()?,
                variants: self.list(12, |reader| {
                    Ok(VariantInfo {
                        name: reader.name()?,
                        tag: reader.u32()?,
                        fields: reader.fields()?,
                    })
                })?,
            },
            3 => TypeKind::Typealias {
                name: self.name()?,
                target: self.index()?,
            },
            4 => TypeKind::Newtype {
                name: self.name()?,
                inner: self.index()?,
            },
            5 => TypeKind::Tuple {
                elements: self.indices()?,
            },
            6 => TypeKind::Function {
                params: self.indices()?,
                ret: self.index()?,
            },
            7 => TypeKind::Effect {
                params: self.indices()?,
                ret: self.index()?,
                is_async: self.boolean()?,
            },
            8 => TypeKind::Optional {
                inner: self.index()?,
            },
            9 => TypeKind::ErrorQualified {
                errors: self.indices()?,
                inner: self.index()?,
            },
            10 => TypeKind::EffectQualified {
                effects: self.indices()?,
                inner: self.index()?,
            },
            11 => TypeKind::Module { name: self.name()? },
            12 => TypeKind::Trait {
                name: self.name()?,
                parents: self.indices()?,
                assoc_types: self.list(8, |reader| Ok((reader.name()?, reader.index()?)))?,
            },
            13 if self.revision >= 7 => TypeKind::AssociatedType {
                trait_owner: self.index()?,
                name: self.name()?,
            },
            _ => return Err(invalid("invalid type kind tag")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependent_iteration_templates_paths_and_defaults_are_checked_and_versioned() {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("StepSource"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let name = str_interner::intern("Item");
        let result_name = str_interner::intern("Result");
        let item = pool.register(TypeInfo {
            kind: TypeKind::AssociatedType {
                trait_owner: owner,
                name,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let result = pool.register(TypeInfo {
            kind: TypeKind::AssociatedType {
                trait_owner: owner,
                name: result_name,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind else {
            panic!()
        };
        *assoc_types = vec![(name, item), (result_name, result)];
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name,
            expression: AssociatedTypeExpr::Concrete(Intrinsic::I64.type_index()),
        })
        .unwrap();
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: result_name,
            expression: AssociatedTypeExpr::IterationStep {
                item: Box::new(AssociatedTypeExpr::Binding {
                    trait_owner: owner,
                    name,
                }),
            },
        })
        .unwrap();
        let template = pool.intern_iteration_step_template(item).unwrap();
        let declaration = pool.intern_structural(TypeKind::Function {
            params: vec![owner],
            ret: template,
        });
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: owner,
            slots: vec![TraitMethodKey {
                trait_owner: owner,
                name: str_interner::intern("next"),
                signature: Some(TraitMethodSignature {
                    declaration,
                    self_paths: vec![vec![TraitTypeStep::Parameter(0)]],
                    parameter_kinds: vec![TraitParameterKind::Receiver],
                    associated_paths: vec![TraitAssociatedPath {
                        trait_owner: owner,
                        name,
                        path: vec![TraitTypeStep::Return, TraitTypeStep::IterationItem],
                    }],
                }),
            }],
        })
        .unwrap();
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[4..8], &9u32.to_le_bytes());
        let restored = decode_pool(&bytes).unwrap();
        assert_eq!(encode_pool(&restored).unwrap(), bytes);
        for revision in 1..=8 {
            assert!(encode_pool_revision(&pool, revision).is_err());
            let mut downgraded = bytes.clone();
            downgraded[4..8].copy_from_slice(&revision.to_le_bytes());
            assert!(decode_pool(&downgraded).is_err());
        }
        for corruption in 0..3 {
            let mut snapshot = pool.snapshot();
            let signature = snapshot.trait_schemas.last_mut().unwrap().slots[0]
                .signature
                .as_mut()
                .unwrap();
            match corruption {
                0 => signature.associated_paths.clear(),
                1 => signature.associated_paths[0].name = result_name,
                _ => signature.associated_paths[0].path = vec![TraitTypeStep::Return],
            }
            assert!(
                TypePool::restore(snapshot).is_err(),
                "corruption {corruption}"
            );
        }
        // New expression syntax alone needs revision 9, even without template kinds/paths.
        let mut expression_only = pool.snapshot();
        expression_only.trait_schemas.clear();
        expression_only
            .structural_types
            .retain(|&ty| ty != template && ty != declaration);
        expression_only.types[template.as_u32() as usize].kind =
            TypeKind::Tuple { elements: vec![] };
        expression_only.types[declaration.as_u32() as usize].kind =
            TypeKind::Tuple { elements: vec![] };
        let expression_only = TypePool::restore(expression_only).unwrap();
        let bytes = encode_pool(&expression_only).unwrap();
        assert_eq!(&bytes[4..8], &9u32.to_le_bytes());
        assert!(encode_pool_revision(&expression_only, 8).is_err());
        let mut downgraded = bytes;
        downgraded[4..8].copy_from_slice(&8u32.to_le_bytes());
        assert!(decode_pool(&downgraded).is_err());
        let mut required = expression_only.snapshot();
        required.associated_defaults[0].expression = AssociatedTypeExpr::Required;
        let required = TypePool::restore(required).unwrap();
        let bytes = encode_pool(&required).unwrap();
        assert_eq!(&bytes[4..8], &10u32.to_le_bytes());
        assert_eq!(encode_pool(&decode_pool(&bytes).unwrap()).unwrap(), bytes);
        for revision in 1..=9 {
            assert!(encode_pool_revision(&required, revision).is_err());
            let mut downgraded = bytes.clone();
            downgraded[4..8].copy_from_slice(&revision.to_le_bytes());
            assert!(decode_pool(&downgraded).is_err());
        }
    }

    #[test]
    fn iteration_step_revision_identity_and_downgrade_are_checked() {
        let mut pool = TypePool::with_intrinsics();
        assert_eq!(&encode_pool(&pool).unwrap()[4..8], &5u32.to_le_bytes());
        let step = pool
            .intern_iteration_step(Intrinsic::I64.type_index())
            .unwrap();
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[4..8], &8u32.to_le_bytes());
        let mut restored = decode_pool(&bytes).unwrap();
        assert_eq!(
            restored
                .intern_iteration_step(Intrinsic::I64.type_index())
                .unwrap(),
            step
        );
        assert_eq!(
            restored.display_name(step).as_deref(),
            Some("IterationStep(i64)")
        );
        for revision in 1..=7 {
            assert!(encode_pool_revision(&pool, revision).is_err());
            let mut downgraded = bytes.clone();
            downgraded[4..8].copy_from_slice(&revision.to_le_bytes());
            assert!(decode_pool(&downgraded).is_err());
        }
        let mut nominal = TypePool::with_intrinsics();
        let ordinary = nominal.register(pool.get(step).clone());
        for revision in 1..=7 {
            let old = decode_pool(&encode_pool_revision(&nominal, revision).unwrap()).unwrap();
            assert_eq!(old.checked_iteration_step_item(ordinary).unwrap(), None);
            assert!(!old.has_iteration_steps());
        }
        let mut damaged = pool.snapshot();
        let TypeKind::Enum { variants, .. } = &mut damaged.types[step.as_u32() as usize].kind
        else {
            panic!()
        };
        variants[1].fields[0].ty = TypeIndex::INVALID;
        assert!(TypePool::restore(damaged).is_err());
    }

    fn rich_pool() -> TypePool {
        let mut pool = TypePool::with_intrinsics();
        let i64_type = Intrinsic::I64.type_index();
        let field = FieldInfo {
            name: str_interner::intern("число"),
            ty: i64_type,
            has_default: true,
            offset: 0,
        };
        let nominal = pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("Record"),
                fields: vec![field.clone()],
            },
            type_id: TypeId::from_pair(100, 200),
            size: 8,
            align: 8,
        });
        let kinds = [
            TypeKind::Enum {
                name: str_interner::intern("Choice"),
                variants: vec![VariantInfo {
                    name: str_interner::intern("Yes"),
                    tag: 17,
                    fields: vec![field],
                }],
            },
            TypeKind::Typealias {
                name: str_interner::intern("Alias"),
                target: nominal,
            },
            TypeKind::Newtype {
                name: str_interner::intern("Wrapped"),
                inner: i64_type,
            },
            TypeKind::Tuple {
                elements: vec![nominal, i64_type],
            },
            TypeKind::Function {
                params: vec![nominal],
                ret: i64_type,
            },
            TypeKind::Effect {
                params: vec![nominal],
                ret: i64_type,
                is_async: true,
            },
            TypeKind::Optional { inner: nominal },
            TypeKind::ErrorQualified {
                errors: vec![nominal],
                inner: i64_type,
            },
            TypeKind::EffectQualified {
                effects: vec![nominal],
                inner: i64_type,
            },
            TypeKind::Module {
                name: str_interner::intern("Module"),
            },
            TypeKind::Trait {
                name: str_interner::intern("CustomTrait"),
                parents: vec![pool.well_known.display],
                assoc_types: vec![
                    (str_interner::intern("Item"), TypeIndex::INVALID),
                    (str_interner::intern("Other"), i64_type),
                ],
            },
        ];
        for kind in kinds {
            pool.intern_structural(kind);
        }
        // A nominal operation deliberately shares an interned signature shape.
        pool.register(TypeInfo {
            kind: TypeKind::Effect {
                params: vec![nominal],
                ret: i64_type,
                is_async: true,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let trait_type = pool.well_known.display;
        let method = MethodSlot {
            name: str_interner::intern("display"),
            func_id: type_pool::DERIVE_FUNC_ID,
            trait_impl: Some(trait_type),
            visible_scope: Some(27),
            access: MethodAccess::LegacyUnknown,
        };
        pool.add_method(nominal, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            trait_type,
            implementor: nominal,
            visible_scope: Some(27),
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            trait_type,
            implementor: nominal,
            visible_scope: Some(27),
            entries: vec![type_pool::DERIVE_FUNC_ID],
        });
        pool
    }

    #[test]
    fn every_type_shape_and_dispatch_metadata_round_trip() {
        let pool = rich_pool();
        let bytes = encode_pool(&pool).unwrap();
        let decoded = decode_pool(&bytes).unwrap();
        assert_eq!(decoded.len(), pool.len());
        for index in 0..pool.len() {
            let index = TypeIndex::from_raw(index as u32);
            assert_eq!(decoded.display_name(index), pool.display_name(index));
            assert_eq!(decoded.get(index).type_id, pool.get(index).type_id);
            assert_eq!(decoded.get(index).size, pool.get(index).size);
            assert_eq!(decoded.get(index).align, pool.get(index).align);
        }
        assert_eq!(
            decoded.snapshot().structural_types,
            pool.snapshot().structural_types
        );
        assert_eq!(
            decoded.snapshot().vtables[0].entries,
            vec![type_pool::DERIVE_FUNC_ID]
        );
        assert_eq!(
            decoded.snapshot().trait_impls[0].methods[0].visible_scope,
            Some(27)
        );
        assert_eq!(encode_pool(&decoded).unwrap(), bytes);
    }

    #[test]
    fn inline_names_are_interned_without_persistent_process_ids() {
        // A valid pool payload with a fresh, never-interned module name. Changing
        // its inline bytes proves the decoder interns content rather than using
        // the source process's original handle.
        let mut pool = TypePool::with_intrinsics();
        let module = pool.register(TypeInfo {
            kind: TypeKind::Module {
                name: str_interner::intern("CodecOldModule"),
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let mut bytes = encode_pool(&pool).unwrap();
        let old = b"CodecOldModule";
        let position = bytes
            .windows(old.len())
            .position(|window| window == old)
            .unwrap();
        bytes[position..position + old.len()].copy_from_slice(b"CodecNewModule");
        for index in 0..100 {
            str_interner::intern(&format!("metadata_noise_{index}"));
        }
        let decoded = decode_pool(&bytes).unwrap();
        assert_eq!(
            decoded.display_name(module).as_deref(),
            Some("CodecNewModule")
        );
    }

    #[test]
    fn truncation_tags_counts_utf8_and_trailing_bytes_are_rejected() {
        let bytes = encode_pool(&rich_pool()).unwrap();
        for length in 0..bytes.len() {
            assert!(
                decode_pool(&bytes[..length]).is_err(),
                "prefix length {length}"
            );
        }
        let mut damaged = bytes.clone();
        damaged.push(0);
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes.clone();
        damaged[4..8].copy_from_slice(&8u32.to_le_bytes());
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes.clone();
        damaged[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes.clone();
        damaged[36] = 255;
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes.clone();
        damaged[37] = 255;
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes.clone();
        let position = damaged
            .windows(6)
            .position(|window| window == b"Record")
            .unwrap();
        damaged[position] = 255;
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes;
        let length = damaged.len();
        damaged[length - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_pool(&damaged).is_err());
    }

    #[test]
    fn malformed_optional_boolean_and_writer_string_handles_are_rejected() {
        let mut writer = Writer {
            revision: VERSION,
            bytes: Vec::new(),
            items: 0,
        };
        assert!(writer.name(StrId::from_raw(u32::MAX)).is_err());
        let mut reader = Reader {
            revision: VERSION,
            bytes: &[2],
            position: 0,
            items: 0,
        };
        assert!(reader.boolean().is_err());
        let mut reader = Reader {
            revision: VERSION,
            bytes: &[3],
            position: 0,
            items: 0,
        };
        assert!(reader.option().is_err());
        let mut pool = TypePool::with_intrinsics();
        pool.register(TypeInfo {
            kind: TypeKind::Module {
                name: StrId::from_raw(u32::MAX),
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        assert!(encode_pool(&pool).is_err());
    }
    #[test]
    fn tpol_three_preserves_access_levels_extend_scopes_and_associated_types() {
        let mut pool = TypePool::with_intrinsics();
        let owner = Intrinsic::I64.type_index();
        let scopes = vec![
            ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: Some(owner),
            },
            ScopeContext {
                parent: None,
                package: 1,
                assoc_type: None,
            },
        ];
        pool.install_scopes(scopes.clone()).unwrap();
        for (index, access) in [
            MethodAccess::Public,
            MethodAccess::Package(0),
            MethodAccess::Private(1),
            MethodAccess::LegacyUnknown,
        ]
        .into_iter()
        .enumerate()
        {
            pool.add_method(
                owner,
                MethodSlot {
                    name: str_interner::intern(&format!("codec_method_{index}")),
                    func_id: type_pool::DERIVE_FUNC_ID,
                    trait_impl: None,
                    visible_scope: Some(1),
                    access,
                },
            );
        }
        let bytes = encode_pool_revision(&pool, 3).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x03\x00\x00\x00");
        let decoded = decode_pool(&bytes).unwrap();
        assert_eq!(decoded.scopes(), scopes);
        for (original, restored) in pool.methods_of(owner).iter().zip(decoded.methods_of(owner)) {
            assert_eq!(restored.access, original.access);
            assert_eq!(restored.visible_scope, original.visible_scope);
            assert_eq!(restored.func_id, original.func_id);
        }
        assert_eq!(encode_pool_revision(&decoded, 3).unwrap(), bytes);
    }

    #[test]
    fn tpol_one_retains_unknown_visibility_and_raw_extend_ids() {
        let pool = rich_pool();
        let bytes = encode_pool_revision(&pool, 1).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x01\x00\x00\x00");
        let decoded = decode_pool(&bytes).unwrap();
        assert!(decoded.scopes().is_empty());
        let method = &decoded.snapshot().trait_impls[0].methods[0];
        assert_eq!(method.access, MethodAccess::LegacyUnknown);
        assert_eq!(method.visible_scope, Some(27));
        assert_ne!(method.access, MethodAccess::Public);
        assert_eq!(encode_pool_revision(&decoded, 1).unwrap(), bytes);
        let upgraded = encode_pool(&decoded).unwrap();
        assert_eq!(
            decode_pool(&upgraded).unwrap().snapshot().trait_impls[0].methods[0].access,
            MethodAccess::LegacyUnknown
        );
    }

    #[test]
    fn access_tags_and_scope_payloads_reject_corrupt_metadata() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        }])
        .unwrap();
        let name = "ScopeCodecPrivate";
        pool.add_method(
            Intrinsic::I64.type_index(),
            MethodSlot {
                name: str_interner::intern(name),
                func_id: type_pool::DERIVE_FUNC_ID,
                trait_impl: None,
                visible_scope: None,
                access: MethodAccess::Private(0),
            },
        );
        let bytes = encode_pool(&pool).unwrap();
        let name_offset = bytes
            .windows(name.len())
            .position(|slice| slice == name.as_bytes())
            .unwrap();
        let access_offset = name_offset + name.len() + 6;
        assert_eq!(bytes[access_offset], 3);
        let mut damaged = bytes.clone();
        damaged[access_offset] = 9;
        assert!(
            decode_pool(&damaged)
                .err()
                .unwrap()
                .to_string()
                .contains("access tag")
        );
        let mut damaged = bytes.clone();
        damaged[access_offset + 1..access_offset + 5].copy_from_slice(&99_u32.to_le_bytes());
        assert!(
            decode_pool(&damaged)
                .err()
                .unwrap()
                .to_string()
                .contains("private scope")
        );
        damaged[access_offset] = 2;
        assert!(
            decode_pool(&damaged)
                .err()
                .unwrap()
                .to_string()
                .contains("package")
        );
        // The single root scope has a six-byte encoding: absent parent, package,
        // absent associated type, followed by the empty schema count.
        // Insert a parent without corrupting framing.
        let scope_offset = bytes.len() - 10;
        for (parent, message) in [(99_u32, "out of range"), (0, "cyclic")] {
            let mut damaged = bytes.clone();
            let mut parent_bytes = vec![1];
            parent_bytes.extend(parent.to_le_bytes());
            damaged.splice(scope_offset..scope_offset + 1, parent_bytes);
            assert!(
                decode_pool(&damaged)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains(message)
            );
        }
        let mut damaged = bytes.clone();
        damaged[scope_offset] = 2;
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes.clone();
        damaged[scope_offset - 4..scope_offset].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_pool(&damaged).is_err());
        let mut damaged = bytes.clone();
        let mut associated_type = vec![1];
        associated_type.extend(u32::MAX.to_le_bytes());
        damaged.splice(scope_offset + 5..scope_offset + 6, associated_type);
        assert!(decode_pool(&damaged).is_err());
    }
    fn scoped_trait_pool() -> TypePool {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![
            ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
        ])
        .unwrap();
        let owner = Intrinsic::I64.type_index();
        for (scope, function) in [(1, 40), (2, 2)] {
            let method = MethodSlot {
                name: str_interner::intern("eq"),
                func_id: function,
                trait_impl: Some(pool.well_known.eq),
                visible_scope: Some(scope),
                access: MethodAccess::Public,
            };
            pool.add_method(owner, method.clone());
            pool.add_trait_impl(TraitImplRecord {
                trait_type: pool.well_known.eq,
                implementor: owner,
                visible_scope: Some(scope),
                methods: vec![method],
            });
            pool.add_vtable(VTable {
                trait_type: pool.well_known.eq,
                implementor: owner,
                visible_scope: Some(scope),
                entries: vec![function],
            });
        }
        pool
    }

    #[test]
    fn scoped_trait_record_and_vtable_identities_survive_resaving() {
        let mut pool = scoped_trait_pool();
        let marker = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("CodecMarker"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId(880, 2),
            size: 0,
            align: 0,
        });
        for scope in [1, 2] {
            pool.add_trait_impl(TraitImplRecord {
                trait_type: marker,
                implementor: Intrinsic::I64.type_index(),
                visible_scope: Some(scope),
                methods: vec![],
            });
            pool.add_vtable(VTable {
                trait_type: marker,
                implementor: Intrinsic::I64.type_index(),
                visible_scope: Some(scope),
                entries: vec![],
            });
        }
        let mut bytes = encode_pool(&pool).unwrap();
        for _ in 0..3 {
            assert_eq!(&bytes[..8], b"TPOL\x05\x00\x00\x00");
            let decoded = decode_pool(&bytes).unwrap();
            for (scope, function) in [(1, 40), (2, 2)] {
                let record = decoded
                    .find_trait_impl_scoped(
                        Intrinsic::I64.type_index(),
                        decoded.well_known.eq,
                        scope,
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(record.visible_scope, Some(scope));
                assert_eq!(record.methods[0].func_id, function);
                let table = decoded
                    .find_vtable_scoped(Intrinsic::I64.type_index(), decoded.well_known.eq, scope)
                    .unwrap()
                    .unwrap();
                assert_eq!(table.visible_scope, Some(scope));
                assert_eq!(table.entries, vec![function]);
                assert_eq!(
                    decoded
                        .find_trait_impl_scoped(Intrinsic::I64.type_index(), marker, scope)
                        .unwrap()
                        .unwrap()
                        .visible_scope,
                    Some(scope)
                );
                assert_eq!(
                    decoded
                        .find_vtable_scoped(Intrinsic::I64.type_index(), marker, scope)
                        .unwrap()
                        .unwrap()
                        .visible_scope,
                    Some(scope)
                );
            }
            assert!(
                decoded
                    .find_trait_impl_scoped(Intrinsic::I64.type_index(), decoded.well_known.eq, 0)
                    .unwrap()
                    .is_none()
            );
            assert!(
                decoded
                    .find_trait_impl(Intrinsic::I64.type_index(), decoded.well_known.eq)
                    .is_none()
            );
            assert_eq!(decoded.scopes(), pool.scopes());
            bytes = encode_pool(&decoded).unwrap();
        }
        let mut duplicate = scoped_trait_pool();
        duplicate.add_trait_impl(duplicate.snapshot().trait_impls[0].clone());
        assert!(encode_pool(&duplicate).is_err());
        let mut duplicate = scoped_trait_pool();
        duplicate.add_vtable(duplicate.snapshot().vtables[0].clone());
        assert!(encode_pool(&duplicate).is_err());
        let mut overlapping = scoped_trait_pool();
        let snapshot = overlapping.snapshot();
        let mut global = snapshot.trait_impls[0].clone();
        global.visible_scope = None;
        global.methods[0].visible_scope = None;
        overlapping.add_trait_impl(global);
        let mut table = snapshot.vtables[0].clone();
        table.visible_scope = None;
        overlapping.add_vtable(table);
        let decoded = decode_pool(&encode_pool(&overlapping).unwrap()).unwrap();
        assert!(
            decoded
                .find_trait_impl_scoped(Intrinsic::I64.type_index(), decoded.well_known.eq, 1)
                .is_err()
        );
    }

    #[test]
    fn old_trait_formats_infer_only_a_unique_consistent_scope() {
        for revision in [1, 2] {
            let bytes = encode_pool_revision(&rich_pool(), revision).unwrap();
            let decoded = decode_pool(&bytes).unwrap();
            let snapshot = decoded.snapshot();
            assert_eq!(snapshot.trait_impls[0].visible_scope, Some(27));
            assert_eq!(snapshot.vtables[0].visible_scope, Some(27));
            let owner = snapshot.trait_impls[0].implementor;
            assert!(
                decoded
                    .find_trait_impl_scoped(owner, snapshot.trait_impls[0].trait_type, 0)
                    .is_err()
            );
            assert_eq!(
                decode_pool(&encode_pool(&decoded).unwrap())
                    .unwrap()
                    .snapshot()
                    .trait_impls[0]
                    .visible_scope,
                Some(27)
            );
        }
        let pool = scoped_trait_pool();
        for revision in [1, 2] {
            let bytes = encode_pool_revision(&pool, revision).unwrap();
            assert!(
                decode_pool(&bytes)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("ambiguous")
            );
        }
    }
    #[test]
    fn trait_scope_corruption_and_legacy_mixed_method_scopes_are_rejected() {
        let pool = scoped_trait_pool();
        let bytes = encode_pool(&pool).unwrap();
        let mut prefix = Vec::new();
        prefix.extend(pool.well_known.eq.as_u32().to_le_bytes());
        prefix.extend(Intrinsic::I64.type_index().as_u32().to_le_bytes());
        prefix.push(1);
        prefix.extend(1_u32.to_le_bytes());
        prefix.extend(1_u32.to_le_bytes());
        let offsets: Vec<_> = bytes
            .windows(prefix.len())
            .enumerate()
            .filter_map(|(index, window)| (window == prefix).then_some(index))
            .collect();
        assert_eq!(offsets.len(), 2);
        for &offset in &offsets {
            let mut damaged = bytes.clone();
            damaged[offset + 8] = 2;
            assert!(decode_pool(&damaged).is_err());
            let mut damaged = bytes.clone();
            damaged[offset + 9..offset + 13].copy_from_slice(&99_u32.to_le_bytes());
            assert!(decode_pool(&damaged).is_err());
        }
        let mut snapshot = pool.snapshot();
        snapshot.trait_impls.truncate(1);
        snapshot.vtables.truncate(1);
        let name = "legacy_second";
        let mut method = snapshot.trait_impls[0].methods[0].clone();
        method.name = str_interner::intern(name);
        snapshot.trait_impls[0].methods.push(method.clone());
        snapshot.methods[Intrinsic::I64.type_index().as_u32() as usize].push(method);
        snapshot.vtables[0].entries.push(40);
        let pool = TypePool::restore(snapshot).unwrap();
        for revision in [1, 2] {
            let mut bytes = encode_pool_revision(&pool, revision).unwrap();
            let offset = bytes
                .windows(name.len())
                .rposition(|window| window == name.as_bytes())
                .unwrap()
                + name.len();
            assert_eq!(bytes[offset + 9], 1);
            bytes[offset + 10..offset + 14].copy_from_slice(&2_u32.to_le_bytes());
            assert!(
                decode_pool(&bytes)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("inconsistent scopes")
            );
        }
    }

    fn dispatch_schema_pool() -> TypePool {
        let mut pool = scoped_trait_pool();
        let parent = pool.well_known.eq;
        let child = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("CodecDispatchChild"),
                parents: vec![parent],
                assoc_types: vec![],
            },
            type_id: TypeId(880, 3),
            size: 0,
            align: 0,
        });
        for (scope, inherited, function) in [(1, 40, 41), (2, 2, 3)] {
            let method = MethodSlot {
                name: str_interner::intern("extra"),
                func_id: function,
                trait_impl: Some(child),
                visible_scope: Some(scope),
                access: MethodAccess::Public,
            };
            pool.add_method(Intrinsic::I64.type_index(), method.clone());
            pool.add_trait_impl(TraitImplRecord {
                trait_type: child,
                implementor: Intrinsic::I64.type_index(),
                visible_scope: Some(scope),
                methods: vec![method],
            });
            pool.add_vtable(VTable {
                trait_type: child,
                implementor: Intrinsic::I64.type_index(),
                visible_scope: Some(scope),
                entries: vec![inherited, function],
            });
        }
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: parent,
            slots: vec![TraitMethodKey {
                trait_owner: parent,
                name: str_interner::intern("eq"),
                signature: None,
            }],
        })
        .unwrap();
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: child,
            slots: vec![
                TraitMethodKey {
                    trait_owner: parent,
                    name: str_interner::intern("eq"),
                    signature: None,
                },
                TraitMethodKey {
                    trait_owner: child,
                    name: str_interner::intern("extra"),
                    signature: None,
                },
            ],
        })
        .unwrap();
        pool
    }

    #[test]
    fn tpol_four_preserves_dispatch_schema_indices_order_and_scoped_tables() {
        let pool = dispatch_schema_pool();
        let expected = pool.snapshot().trait_schemas;
        let mut bytes = encode_pool_revision(&pool, 4).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x04\x00\x00\x00");
        for _ in 0..3 {
            let decoded = decode_pool(&bytes).unwrap();
            assert_eq!(decoded.snapshot().trait_schemas, expected);
            for (before, after) in pool
                .vtables_snapshot()
                .iter()
                .zip(decoded.vtables_snapshot())
            {
                assert_eq!(before.trait_type, after.trait_type);
                assert_eq!(before.implementor, after.implementor);
                assert_eq!(before.visible_scope, after.visible_scope);
                assert_eq!(before.entries, after.entries);
            }
            assert_eq!(decoded.checked_vtable_descriptors(2).unwrap().len(), 2);
            let next = encode_pool_revision(&decoded, 4).unwrap();
            assert_eq!(next, bytes);
            bytes = next;
        }
    }

    #[test]
    fn legacy_pool_revisions_do_not_fabricate_dispatch_schemas() {
        let pool = dispatch_schema_pool();
        // Revision one cannot encode this pool's access graph. Its ordinary
        // empty-schema fixture still exercises the unchanged legacy layout.
        let legacy = rich_pool();
        // TPOL2 had no independent table scope and requires a unique owner/
        // trait record when recovering that scope from its method metadata.
        let mut snapshot = pool.snapshot();
        snapshot
            .trait_impls
            .retain(|record| record.visible_scope == Some(1));
        snapshot
            .vtables
            .retain(|table| table.visible_scope == Some(1));
        let single_scope = TypePool::restore(snapshot).unwrap();
        for (revision, original) in [(1, &legacy), (2, &single_scope), (3, &pool)] {
            let decoded = decode_pool(&encode_pool_revision(original, revision).unwrap()).unwrap();
            assert!(decoded.snapshot().trait_schemas.is_empty());
            assert!(decoded.checked_vtable_descriptors(0).is_err());
            let upgraded = decode_pool(&encode_pool(&decoded).unwrap()).unwrap();
            assert!(upgraded.snapshot().trait_schemas.is_empty());
        }
    }

    #[test]
    fn dispatch_schema_corruption_rejects_wrong_names_slots_and_counts() {
        let pool = dispatch_schema_pool();
        let bytes = encode_pool(&pool).unwrap();
        let mut damaged = bytes.clone();
        let name = damaged
            .windows(5)
            .rposition(|bytes| bytes == b"extra")
            .unwrap();
        damaged[name..name + 5].copy_from_slice(b"other");
        assert!(decode_pool(&damaged).is_err());
        damaged[name] = 255;
        assert!(decode_pool(&damaged).is_err());
        // TPOL4 appends schemas after the unchanged TPOL3 payload. The parent
        // schema is 18 bytes; child keys occupy 10 and 13 bytes respectively.
        let schema_offset = encode_pool_revision(&pool, 3).unwrap().len();
        let child_keys = schema_offset + 4 + 18 + 8;
        let keys_only = encode_pool_revision(&pool, 4).unwrap();
        let mut reversed = keys_only.clone();
        let first = keys_only[child_keys..child_keys + 10].to_vec();
        let second = keys_only[child_keys + 10..child_keys + 23].to_vec();
        reversed.splice(child_keys..child_keys + 23, second.into_iter().chain(first));
        assert!(decode_pool(&reversed).is_err());
        let mut snapshot = pool.snapshot();
        snapshot.vtables[2].entries.swap(0, 1);
        assert!(TypePool::restore(snapshot).is_err());
        let mut snapshot = pool.snapshot();
        snapshot.vtables[2].entries[1] = 40;
        assert!(TypePool::restore(snapshot).is_err());
        let mut snapshot = pool.snapshot();
        snapshot.trait_schemas[1].slots[0].trait_owner = Intrinsic::I64.type_index();
        assert!(TypePool::restore(snapshot).is_err());
        let mut snapshot = pool.snapshot();
        snapshot
            .trait_schemas
            .push(snapshot.trait_schemas[0].clone());
        assert!(TypePool::restore(snapshot).is_err());
        let mut reader = Reader {
            revision: VERSION,
            bytes: &[],
            position: 0,
            items: MAX_ITEMS,
        };
        let mut count = Vec::new();
        count.extend(1u32.to_le_bytes());
        count.extend([0; 8]);
        reader.bytes = &count;
        assert!(reader.list(8, |reader| reader.take(8)).is_err());
    }

    fn signature_schema_pool() -> TypePool {
        let mut pool = dispatch_schema_pool();
        let schemas = pool.snapshot().trait_schemas;
        let parent = schemas[0].trait_type;
        let child = schemas[1].trait_type;
        let parent_declaration = pool.intern_structural(TypeKind::Function {
            params: vec![parent, parent],
            ret: Intrinsic::Bool.type_index(),
        });
        let nested = pool.intern_structural(TypeKind::Tuple {
            elements: vec![child, parent],
        });
        let child_declaration = pool.intern_structural(TypeKind::Function {
            params: vec![child, nested],
            ret: child,
        });
        let parent_signature = TraitMethodSignature {
            associated_paths: vec![],
            declaration: parent_declaration,
            self_paths: vec![
                vec![TraitTypeStep::Parameter(0)],
                vec![TraitTypeStep::Parameter(1)],
            ],
            parameter_kinds: vec![TraitParameterKind::Receiver, TraitParameterKind::Required],
        };
        let mut snapshot = pool.snapshot();
        snapshot.trait_schemas[0].slots[0].signature = Some(parent_signature.clone());
        snapshot.trait_schemas[1].slots[0].signature = Some(parent_signature);
        snapshot.trait_schemas[1].slots[1].signature = Some(TraitMethodSignature {
            associated_paths: vec![],
            declaration: child_declaration,
            self_paths: vec![
                vec![TraitTypeStep::Parameter(0)],
                vec![TraitTypeStep::Parameter(1), TraitTypeStep::TupleElement(0)],
                vec![TraitTypeStep::Return],
            ],
            parameter_kinds: vec![TraitParameterKind::Receiver, TraitParameterKind::Required],
        });
        TypePool::restore(snapshot).unwrap()
    }

    #[test]
    fn tpol_five_preserves_nested_self_paths_without_replacing_explicit_traits() {
        let mut pool = signature_schema_pool();
        let child_key = pool.snapshot().trait_schemas[1].slots[1].clone();
        let explicit_parent = pool.snapshot().trait_schemas[0].trait_type;
        let owner = Intrinsic::I64.type_index();
        let nested = pool.intern_structural(TypeKind::Tuple {
            elements: vec![owner, explicit_parent],
        });
        let actual = pool.intern_structural(TypeKind::Function {
            params: vec![owner, nested],
            ret: owner,
        });
        pool.check_trait_method_signature(&child_key, owner, actual)
            .unwrap();
        let wrong_nested = pool.intern_structural(TypeKind::Tuple {
            elements: vec![owner, owner],
        });
        let wrong = pool.intern_structural(TypeKind::Function {
            params: vec![owner, wrong_nested],
            ret: owner,
        });
        assert!(
            pool.check_trait_method_signature(&child_key, owner, wrong)
                .is_err()
        );
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x05\x00\x00\x00");
        let decoded = decode_pool(&bytes).unwrap();
        assert_eq!(
            decoded.snapshot().trait_schemas,
            pool.snapshot().trait_schemas
        );
        assert_eq!(encode_pool(&decoded).unwrap(), bytes);
        decoded
            .check_trait_method_signature(&child_key, owner, actual)
            .unwrap();
        let old = decode_pool(&encode_pool_revision(&pool, 4).unwrap()).unwrap();
        assert!(
            old.snapshot()
                .trait_schemas
                .iter()
                .flat_map(|schema| &schema.slots)
                .all(|key| key.signature.is_none())
        );
        let upgraded = decode_pool(&encode_pool(&old).unwrap()).unwrap();
        assert_eq!(
            upgraded.snapshot().trait_schemas,
            old.snapshot().trait_schemas
        );
    }

    #[test]
    fn signature_tags_paths_kinds_and_declarations_reject_corruption() {
        let pool = signature_schema_pool();
        let signature = pool.snapshot().trait_schemas[1].slots[1]
            .signature
            .clone()
            .unwrap();
        let mut writer = Writer {
            revision: VERSION,
            bytes: Vec::new(),
            items: 0,
        };
        writer.trait_signature(Some(&signature)).unwrap();
        let bytes = writer.bytes;
        let mut reader = Reader {
            revision: VERSION,
            bytes: &bytes,
            position: 0,
            items: 0,
        };
        assert_eq!(reader.trait_signature().unwrap(), Some(signature));
        // Presence (1), declaration (4), path count (4), first step count (4).
        for (offset, value) in [(0, 2), (13, 255), (bytes.len() - 1, 255)] {
            let mut damaged = bytes.clone();
            damaged[offset] = value;
            let mut reader = Reader {
                revision: VERSION,
                bytes: &damaged,
                position: 0,
                items: 0,
            };
            assert!(reader.trait_signature().is_err());
        }
        let mut damaged = bytes.clone();
        damaged[9..13].copy_from_slice(&257u32.to_le_bytes());
        damaged.resize(1024, 1);
        let mut reader = Reader {
            revision: VERSION,
            bytes: &damaged,
            position: 0,
            items: 0,
        };
        assert!(reader.trait_signature().is_err());
        for invalid_path in [
            vec![TraitTypeStep::Parameter(9)],
            vec![TraitTypeStep::Parameter(1), TraitTypeStep::TupleElement(1)],
        ] {
            let mut snapshot = pool.snapshot();
            snapshot.trait_schemas[1].slots[1]
                .signature
                .as_mut()
                .unwrap()
                .self_paths
                .push(invalid_path);
            assert!(TypePool::restore(snapshot).is_err());
        }
        let mut snapshot = pool.snapshot();
        snapshot.trait_schemas[1].slots[1]
            .signature
            .as_mut()
            .unwrap()
            .declaration = Intrinsic::I64.type_index();
        assert!(TypePool::restore(snapshot).is_err());
        let mut snapshot = pool.snapshot();
        snapshot.trait_schemas[1].slots[1]
            .signature
            .as_mut()
            .unwrap()
            .parameter_kinds
            .pop();
        assert!(TypePool::restore(snapshot).is_err());
    }

    #[test]
    fn signature_codec_preserves_all_path_steps_and_parameter_kinds() {
        // Codec tokens are preserved independently of shape validation; the
        // pool validator separately checks whether a path fits its declaration.
        let signature = TraitMethodSignature {
            associated_paths: vec![],
            declaration: TypeIndex::from_raw(42),
            self_paths: vec![vec![
                TraitTypeStep::Parameter(7),
                TraitTypeStep::Return,
                TraitTypeStep::TupleElement(9),
                TraitTypeStep::OptionalInner,
                TraitTypeStep::ErrorInner,
                TraitTypeStep::ErrorMember(3),
                TraitTypeStep::EffectInner,
                TraitTypeStep::EffectMember(4),
            ]],
            parameter_kinds: vec![
                TraitParameterKind::Receiver,
                TraitParameterKind::Required,
                TraitParameterKind::Optional,
                TraitParameterKind::ListVariadic,
                TraitParameterKind::MapVariadic,
            ],
        };
        let mut writer = Writer {
            revision: VERSION,
            bytes: Vec::new(),
            items: 0,
        };
        writer.trait_signature(Some(&signature)).unwrap();
        let mut reader = Reader {
            revision: VERSION,
            bytes: &writer.bytes,
            position: 0,
            items: 0,
        };
        assert_eq!(reader.trait_signature().unwrap(), Some(signature));
        assert_eq!(reader.position, writer.bytes.len());
    }

    #[test]
    fn fixed_bootstrap_signatures_roundtrip_without_upgrading_legacy_none() {
        let mut pool = TypePool::with_intrinsics();
        for (trait_type, names, parameters, ret) in [
            (
                pool.well_known.eq,
                vec!["eq"],
                2,
                Intrinsic::Bool.type_index(),
            ),
            (
                pool.well_known.partial_eq,
                vec!["eq"],
                2,
                Intrinsic::Bool.type_index(),
            ),
            (
                pool.well_known.display,
                vec!["to_string"],
                1,
                Intrinsic::Str.type_index(),
            ),
            (
                pool.well_known.iterator,
                vec!["has_next", "next"],
                1,
                Intrinsic::Bool.type_index(),
            ),
        ] {
            let declaration = pool.intern_structural(TypeKind::Function {
                params: vec![trait_type; parameters],
                ret,
            });
            let signature = TraitMethodSignature {
                associated_paths: vec![],
                declaration,
                self_paths: (0..parameters)
                    .map(|index| vec![TraitTypeStep::Parameter(index as u32)])
                    .collect(),
                parameter_kinds: (0..parameters)
                    .map(|index| {
                        if index == 0 {
                            TraitParameterKind::Receiver
                        } else {
                            TraitParameterKind::Required
                        }
                    })
                    .collect(),
            };
            pool.register_trait_schema(TraitDispatchSchema {
                trait_type,
                slots: names
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| TraitMethodKey {
                        trait_owner: trait_type,
                        name: str_interner::intern(name),
                        signature: (index == 0).then(|| signature.clone()),
                    })
                    .collect(),
            })
            .unwrap();
        }
        let schemas = pool.snapshot().trait_schemas;
        let decoded = decode_pool(&encode_pool(&pool).unwrap()).unwrap();
        assert_eq!(decoded.snapshot().trait_schemas, schemas);
        for schema in &schemas {
            let signature = schema.slots[0].signature.as_ref().unwrap();
            let TypeKind::Function { params, ret } = &decoded.get(signature.declaration).kind
            else {
                panic!("expected fixed Function declaration")
            };
            assert_eq!(params[0], schema.trait_type);
            assert_eq!(signature.parameter_kinds[0], TraitParameterKind::Receiver);
            assert_eq!(signature.self_paths[0], vec![TraitTypeStep::Parameter(0)]);
            if schema.trait_type == decoded.well_known.display {
                assert_eq!(*ret, Intrinsic::Str.type_index());
            } else {
                assert_eq!(*ret, Intrinsic::Bool.type_index());
            }
        }
        assert!(
            decoded
                .trait_schema(decoded.well_known.iterator)
                .unwrap()
                .slots[1]
                .signature
                .is_none()
        );
        let legacy = decode_pool(&encode_pool_revision(&pool, 4).unwrap()).unwrap();
        assert!(
            legacy
                .trait_schemas_snapshot()
                .iter()
                .flat_map(|schema| &schema.slots)
                .all(|slot| slot.signature.is_none())
        );
        let upgraded = decode_pool(&encode_pool(&legacy).unwrap()).unwrap();
        assert_eq!(
            upgraded.snapshot().trait_schemas,
            legacy.snapshot().trait_schemas
        );
    }
    #[test]
    fn tpol_six_signature_paths_preserve_names_steps_and_reject_corruption() {
        let signature = TraitMethodSignature {
            declaration: TypeIndex::from_raw(42),
            self_paths: vec![],
            parameter_kinds: vec![TraitParameterKind::Receiver],
            associated_paths: vec![TraitAssociatedPath {
                trait_owner: TypeIndex::from_raw(11),
                name: str_interner::intern("Item"),
                path: vec![TraitTypeStep::Return, TraitTypeStep::OptionalInner],
            }],
        };
        let mut writer = Writer {
            revision: 6,
            bytes: vec![],
            items: 0,
        };
        writer.trait_signature(Some(&signature)).unwrap();
        let bytes = writer.bytes;
        let mut reader = Reader {
            revision: 6,
            bytes: &bytes,
            position: 0,
            items: 0,
        };
        assert_eq!(reader.trait_signature().unwrap(), Some(signature));
        assert_eq!(reader.position, bytes.len());
        let read_bad = |bytes: &[u8]| {
            let mut reader = Reader {
                revision: 6,
                bytes,
                position: 0,
                items: 0,
            };
            assert!(reader.trait_signature().is_err());
        };
        // Presence, declaration, zero Self paths, one parameter kind.
        let associated_offset = 1 + 4 + 4 + 4 + 1;
        let owner_offset = associated_offset + 4;
        let name_offset = owner_offset + 4 + 4;
        let path_count_offset = name_offset + 4;
        for (offset, value) in [(bytes.len() - 1, 255), (name_offset, 255)] {
            let mut bad = bytes.clone();
            bad[offset] = value;
            read_bad(&bad);
        }
        for (offset, value) in [(associated_offset, 257u32), (path_count_offset, 257u32)] {
            let mut bad = bytes.clone();
            bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            read_bad(&bad);
        }
        read_bad(&bytes[..bytes.len() - 1]);
        let mut reader = Reader {
            revision: 5,
            bytes: &bytes,
            position: 0,
            items: 0,
        };
        let old = reader.trait_signature().unwrap().unwrap();
        assert!(old.associated_paths.is_empty());
        assert!(
            reader.position < bytes.len(),
            "revision five must not reinterpret new trailing metadata"
        );
    }
    fn associated_schema_pool() -> TypePool {
        let mut pool = TypePool::with_intrinsics();
        let implementor = Intrinsic::I64.type_index();
        pool.install_scopes(vec![
            ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
        ])
        .unwrap();
        let name = str_interner::intern("Item");
        let parent = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("AssociatedParent"),
                parents: vec![],
                assoc_types: vec![(name, Intrinsic::Any.type_index())],
            },
            type_id: TypeId(906, 1),
            size: 0,
            align: 0,
        });
        let child = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("AssociatedChild"),
                parents: vec![parent],
                assoc_types: vec![],
            },
            type_id: TypeId(906, 2),
            size: 0,
            align: 0,
        });
        let result = pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index(), Intrinsic::Any.type_index()],
        });
        let declaration = pool.intern_structural(TypeKind::Function {
            params: vec![parent],
            ret: result,
        });
        let key = TraitMethodKey {
            trait_owner: parent,
            name: str_interner::intern("get"),
            signature: Some(TraitMethodSignature {
                declaration,
                self_paths: vec![vec![TraitTypeStep::Parameter(0)]],
                parameter_kinds: vec![TraitParameterKind::Receiver],
                associated_paths: vec![TraitAssociatedPath {
                    trait_owner: parent,
                    name,
                    path: vec![TraitTypeStep::Return, TraitTypeStep::TupleElement(0)],
                }],
            }),
        };
        let implementations = [
            (parent, None, 7, Intrinsic::I64),
            (child, Some(1), 8, Intrinsic::Str),
            (child, Some(2), 9, Intrinsic::U64),
        ];
        for (view, visible_scope, func_id, value) in implementations {
            let method = MethodSlot {
                name: key.name,
                func_id,
                trait_impl: Some(view),
                visible_scope,
                access: MethodAccess::Public,
            };
            pool.add_method(implementor, method.clone());
            pool.add_trait_impl(TraitImplRecord {
                trait_type: view,
                implementor,
                visible_scope,
                methods: vec![method],
            });
            pool.register_associated_binding(AssociatedTypeBinding {
                implementor,
                trait_type: view,
                visible_scope,
                trait_owner: parent,
                name,
                value: value.type_index(),
            })
            .unwrap();
        }
        for (view, visible_scope, func_id, _) in implementations {
            pool.add_vtable(VTable {
                trait_type: view,
                implementor,
                visible_scope,
                entries: vec![func_id],
            });
        }
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: parent,
            slots: vec![key.clone()],
        })
        .unwrap();
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: child,
            slots: vec![key],
        })
        .unwrap();
        pool
    }

    #[test]
    fn tpol_six_preserves_exact_scoped_inherited_bindings_and_source_paths() {
        let pool = associated_schema_pool();
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x06\x00\x00\x00");
        let decoded = decode_pool(&bytes).unwrap();
        assert_eq!(
            decoded.associated_bindings_snapshot(),
            pool.associated_bindings_snapshot()
        );
        assert_eq!(
            decoded.trait_schemas_snapshot(),
            pool.trait_schemas_snapshot()
        );
        assert_eq!(encode_pool(&decoded).unwrap(), bytes);
        assert!(encode_pool_revision(&pool, 5).is_err());
        let key = &decoded.trait_schemas_snapshot()[0].slots[0];
        for binding in decoded.associated_bindings_snapshot() {
            let mut restored = decode_pool(&bytes).unwrap();
            let result = restored.intern_structural(TypeKind::Tuple {
                elements: vec![binding.value, Intrinsic::Any.type_index()],
            });
            let actual = restored.intern_structural(TypeKind::Function {
                params: vec![binding.implementor],
                ret: result,
            });
            restored
                .check_trait_method_signature_in_impl(
                    key,
                    binding.implementor,
                    actual,
                    binding.trait_type,
                    binding.visible_scope,
                )
                .unwrap();
        }
        let old = signature_schema_pool();
        let oldbytes = encode_pool_revision(&old, 5).unwrap();
        let old = decode_pool(&oldbytes).unwrap();
        assert!(old.associated_bindings_snapshot().is_empty());
        assert!(
            old.trait_schemas_snapshot()
                .iter()
                .flat_map(|schema| &schema.slots)
                .all(|key| key
                    .signature
                    .as_ref()
                    .is_none_or(|signature| signature.associated_paths.is_empty()))
        );
        assert_eq!(encode_pool(&old).unwrap(), oldbytes);
    }

    #[test]
    fn tpol_six_rejects_invalid_associated_identity_scope_paths_and_bindings() {
        let pool = associated_schema_pool();
        let bytes = encode_pool(&pool).unwrap();
        for truncate in [1, 4, 9] {
            assert!(decode_pool(&bytes[..bytes.len() - truncate]).is_err());
        }
        let mut malformed = bytes.clone();
        malformed[4..8].copy_from_slice(&5u32.to_le_bytes());
        assert!(decode_pool(&malformed).is_err());
        // The final scoped binding is four indices, scope presence/value,
        // UTF-8 name length/data, and a value index. Mutations retain framing.
        for (offset, value) in [
            (bytes.len() - 4, u32::MAX),
            (bytes.len() - 20, 999),
            (bytes.len() - 87, u32::MAX),
        ] {
            let mut malformed = bytes.clone();
            malformed[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(decode_pool(&malformed).is_err());
        }
        for (offset, value) in [(bytes.len() - 21, 2), (bytes.len() - 8, 255)] {
            let mut malformed = bytes.clone();
            malformed[offset] = value;
            assert!(decode_pool(&malformed).is_err());
        }
        for alter in 0..7 {
            let mut snapshot = pool.snapshot();
            match alter {
                0 => snapshot
                    .associated_bindings
                    .push(snapshot.associated_bindings[0].clone()),
                1 => snapshot.associated_bindings[0].value = TypeIndex::INVALID,
                2 => snapshot.associated_bindings[0].visible_scope = Some(999),
                3 => snapshot.associated_bindings[0].trait_owner = Intrinsic::I64.type_index(),
                4 => snapshot.associated_bindings[0].name = str_interner::intern("Missing"),
                5 => {
                    snapshot.trait_schemas[0].slots[0]
                        .signature
                        .as_mut()
                        .unwrap()
                        .associated_paths[0]
                        .path = vec![TraitTypeStep::Return, TraitTypeStep::TupleElement(99)]
                }
                _ => {
                    snapshot.trait_schemas[0].slots[0]
                        .signature
                        .as_mut()
                        .unwrap()
                        .associated_paths[0]
                        .path = vec![TraitTypeStep::Parameter(0)]
                }
            }
            assert!(
                TypePool::restore(snapshot).is_err(),
                "malformed case {alter}"
            );
        }
    }

    #[test]
    fn associated_default_expression_codec_preserves_dependency_identity_and_limits() {
        let owner = Intrinsic::I64.type_index();
        let expression = AssociatedTypeExpr::Function {
            parameters: vec![
                AssociatedTypeExpr::SelfType { trait_owner: owner },
                AssociatedTypeExpr::Binding {
                    trait_owner: owner,
                    name: str_interner::intern("Item"),
                },
            ],
            return_type: Box::new(AssociatedTypeExpr::Optional {
                inner: Box::new(AssociatedTypeExpr::Tuple {
                    elements: vec![
                        AssociatedTypeExpr::Concrete(Intrinsic::Str.type_index()),
                        AssociatedTypeExpr::SelfType { trait_owner: owner },
                    ],
                }),
            }),
        };
        let mut writer = Writer {
            revision: 7,
            bytes: vec![],
            items: 0,
        };
        writer.associated_expr(&expression, 0).unwrap();
        let mut reader = Reader {
            revision: 7,
            bytes: &writer.bytes,
            position: 0,
            items: 0,
        };
        assert_eq!(reader.associated_expr(0).unwrap(), expression);
        assert_eq!(reader.position, writer.bytes.len());
        for bytes in [
            vec![6],
            vec![4, 255, 255, 255, 255],
            vec![5, 0, 0, 0, 0],
            vec![2, 0, 0, 0, 0, 1, 0, 0, 0, 255],
        ] {
            let mut reader = Reader {
                revision: 7,
                bytes: &bytes,
                position: 0,
                items: 0,
            };
            assert!(reader.associated_expr(0).is_err());
        }
        let mut bytes = vec![3; 256];
        bytes.push(0);
        bytes.extend_from_slice(&Intrinsic::I64.type_index().as_u32().to_le_bytes());
        let mut reader = Reader {
            revision: 7,
            bytes: &bytes,
            position: 0,
            items: 0,
        };
        assert!(reader.associated_expr(0).is_err());
        let mut reader = Reader {
            revision: 7,
            bytes: &writer.bytes,
            position: 0,
            items: MAX_ITEMS,
        };
        assert!(reader.associated_expr(0).is_err());
        let mut writer = Writer {
            revision: 7,
            bytes: vec![],
            items: MAX_ITEMS,
        };
        assert!(writer.associated_expr(&expression, 0).is_err());
    }

    fn dependent_default_pool() -> TypePool {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("Dependent"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId(933, 1),
            size: 0,
            align: 0,
        });
        let item = str_interner::intern("Item");
        let output = str_interner::intern("Output");
        let item_type = pool.intern_structural(TypeKind::AssociatedType {
            trait_owner: owner,
            name: item,
        });
        let output_type = pool.intern_structural(TypeKind::AssociatedType {
            trait_owner: owner,
            name: output,
        });
        if let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind {
            *assoc_types = vec![(item, item_type), (output, output_type)];
        }
        for (name, expression) in [
            (item, AssociatedTypeExpr::SelfType { trait_owner: owner }),
            (
                output,
                AssociatedTypeExpr::Function {
                    parameters: vec![AssociatedTypeExpr::Binding {
                        trait_owner: owner,
                        name: item,
                    }],
                    return_type: Box::new(AssociatedTypeExpr::Optional {
                        inner: Box::new(AssociatedTypeExpr::SelfType { trait_owner: owner }),
                    }),
                },
            ),
        ] {
            pool.register_associated_default(AssociatedTypeDefault {
                trait_owner: owner,
                name,
                expression,
            })
            .unwrap();
        }
        for implementor in [Intrinsic::I64.type_index(), Intrinsic::Str.type_index()] {
            pool.add_trait_impl(TraitImplRecord {
                trait_type: owner,
                implementor,
                visible_scope: None,
                methods: vec![],
            });
            let bindings = pool
                .resolve_associated_defaults(implementor, owner, None, &[])
                .unwrap();
            for binding in bindings {
                pool.register_associated_binding(binding).unwrap();
            }
        }
        pool
    }

    #[test]
    fn tpol_seven_preserves_symbolic_defaults_and_concrete_exact_bindings() {
        let pool = dependent_default_pool();
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x07\x00\x00\x00");
        let decoded = decode_pool(&bytes).unwrap();
        assert_eq!(decoded.len(), pool.len());
        for index in 0..pool.len() {
            let ty = TypeIndex::from_raw(index as u32);
            let original = pool.get(ty);
            let restored = decoded.get(ty);
            assert_eq!(
                std::mem::discriminant(&restored.kind),
                std::mem::discriminant(&original.kind)
            );
            if let TypeKind::AssociatedType { trait_owner, name } = original.kind {
                assert!(
                    matches!(restored.kind, TypeKind::AssociatedType { trait_owner: owner, name: restored_name } if owner == trait_owner && restored_name == name)
                );
            }
            assert_eq!(restored.type_id, original.type_id);
            assert_eq!(
                (restored.size, restored.align),
                (original.size, original.align)
            );
        }
        assert_eq!(
            decoded.associated_defaults_snapshot(),
            pool.associated_defaults_snapshot()
        );
        assert_eq!(
            decoded.associated_bindings_snapshot(),
            pool.associated_bindings_snapshot()
        );
        assert_eq!(encode_pool(&decoded).unwrap(), bytes);
        assert!(encode_pool_revision(&pool, 6).is_err());
        for binding in decoded.associated_bindings_snapshot() {
            assert!(!decoded.contains_associated_type(binding.value));
            if str_interner::get(binding.name) == "Item" {
                assert_eq!(binding.value, binding.implementor);
            } else {
                let TypeKind::Function { params, ret } = &decoded.get(binding.value).kind else {
                    panic!("expected concrete Function binding")
                };
                assert_eq!(params, &vec![binding.implementor]);
                assert!(
                    matches!(decoded.get(*ret).kind,TypeKind::Optional{inner} if inner==binding.implementor)
                );
            }
        }
        let legacy = associated_schema_pool();
        let bytes = encode_pool(&legacy).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x06\x00\x00\x00");
        let legacy = decode_pool(&bytes).unwrap();
        assert!(legacy.associated_defaults_snapshot().is_empty());
        assert_eq!(encode_pool(&legacy).unwrap(), bytes);
    }

    #[test]
    fn tpol_seven_rejects_invalid_default_identity_symbols_and_dependencies() {
        let pool = dependent_default_pool();
        let bytes = encode_pool(&pool).unwrap();
        assert!(decode_pool(&bytes[..bytes.len() - 1]).is_err());
        let mut old = bytes.clone();
        old[4..8].copy_from_slice(&6u32.to_le_bytes());
        assert!(decode_pool(&old).is_err());
        for case in 0..7 {
            let mut snapshot = pool.snapshot();
            let owner = snapshot.associated_defaults[0].trait_owner;
            let name = snapshot.associated_defaults[0].name;
            match case {
                0 => snapshot
                    .associated_defaults
                    .push(snapshot.associated_defaults[0].clone()),
                1 => snapshot.associated_defaults.clear(),
                2 => snapshot.associated_defaults[0].trait_owner = Intrinsic::I64.type_index(),
                3 => {
                    snapshot.associated_defaults[0].expression = AssociatedTypeExpr::SelfType {
                        trait_owner: Intrinsic::I64.type_index(),
                    }
                }
                4 => {
                    snapshot.associated_defaults[0].expression = AssociatedTypeExpr::Binding {
                        trait_owner: owner,
                        name: str_interner::intern("missing"),
                    }
                }
                5 => {
                    snapshot.associated_defaults[0].expression =
                        AssociatedTypeExpr::Concrete(TypeIndex::INVALID)
                }
                _ => {
                    let marker=snapshot.types.iter().position(|info|matches!(info.kind,TypeKind::AssociatedType{trait_owner,name:n} if trait_owner==owner && n==name)).unwrap();
                    snapshot.associated_defaults[0].expression =
                        AssociatedTypeExpr::Concrete(TypeIndex::from_raw(marker as u32));
                }
            }
            assert!(
                TypePool::restore(snapshot).is_err(),
                "invalid dependent default case {case}"
            );
        }
    }
    fn finalized_identity_fixture() -> (TypePool, TypeIndex) {
        let mut pool = TypePool::with_intrinsics();
        let ty = pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("IdentityFixture"),
                fields: vec![FieldInfo {
                    name: str_interner::intern("payload"),
                    ty: Intrinsic::I64.type_index(),
                    offset: 0,
                    has_default: false,
                }],
            },
            type_id: TypeId::ZERO,
            size: 8,
            align: 8,
        });
        pool.finalize_type_identities(TypeIdentityInput {
            schema: 1,
            packages: vec![PackageTypeContext {
                identity_schema: 1,
                identity: [0x42; 16],
                qualified_name: "org.test/identity".into(),
                version: "1.2.3".into(),
            }],
            declarations: vec![NominalTypeProvenance {
                type_index: ty,
                package: 0,
                path: vec![IdentityPathSegment::Named("IdentityFixture".into())],
                last_stable_version: "1.2.3".into(),
            }],
        })
        .unwrap();
        (pool, ty)
    }
    #[test]
    fn tpol11_preserves_provenance_and_rejects_full_identity_and_layout_tampering() {
        let (pool, ty) = finalized_identity_fixture();
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[4..8], &11u32.to_le_bytes());
        let restored = decode_pool(&bytes).unwrap();
        assert_eq!(restored.identity_input(), pool.identity_input());
        assert_eq!(
            restored.stable_type_id(ty).unwrap(),
            pool.stable_type_id(ty).unwrap()
        );
        let id = pool.get(ty).type_id;
        let pair = [id.hi().to_le_bytes(), id.lo().to_le_bytes()].concat();
        let offset = bytes.windows(16).position(|bytes| bytes == pair).unwrap();
        for relative in [0, 8] {
            let mut changed = bytes.clone();
            changed[offset + relative] ^= 1;
            assert_eq!(
                decode_pool(&changed).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for (needle, relative) in [
            (b"payload".as_slice(), 0),
            (b"IdentityFixture".as_slice(), 0),
        ] {
            let mut changed = bytes.clone();
            let offset = bytes
                .windows(needle.len())
                .rposition(|bytes| bytes == needle)
                .unwrap();
            changed[offset + relative] ^= 1;
            assert!(decode_pool(&changed).is_err());
        }
        let mut changed = bytes.clone();
        let offset = bytes
            .windows(5)
            .rposition(|bytes| bytes == b"1.2.3")
            .unwrap();
        changed[offset + 4] = b'4';
        assert!(decode_pool(&changed).is_err());
        for end in [offset + 1, bytes.len() - 1] {
            assert!(decode_pool(&bytes[..end]).is_err());
        }
        for revision in 1..=10 {
            assert!(encode_pool_revision(&pool, revision).is_err());
        }
    }
    #[test]
    fn tpol11_recomputes_abstract_and_concrete_step_provenance_and_rejects_bad_records() {
        let (mut pool, _) = finalized_identity_fixture();
        let input = pool.identity_input().unwrap().clone();
        let step = pool
            .intern_iteration_step(Intrinsic::I64.type_index())
            .unwrap();
        // Adding a descriptor requires re-finalization; stale published state
        // cannot be archived or used as a stable type lookup.
        assert!(encode_pool(&pool).is_err());
        pool.finalize_type_identities(input).unwrap();
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(
            decode_pool(&bytes).unwrap().stable_type_id(step).unwrap(),
            pool.stable_type_id(step).unwrap()
        );
        for mutation in 0..6 {
            let mut snapshot = pool.snapshot();
            let input = snapshot.identity_input.as_mut().unwrap();
            match mutation {
                0 => input.schema = 2,
                1 => input.packages[0].identity_schema = 2,
                2 => input.declarations[0].package = 99,
                3 => input.declarations.clear(),
                4 => input.declarations.push(input.declarations[0].clone()),
                _ => input.declarations[0].last_stable_version = "invalid".into(),
            };
            assert!(TypePool::restore(snapshot).is_err());
        }
    }
    #[test]
    fn legacy_revisions_preserve_legacy_bootstrap_values_without_identity_provenance() {
        let mut pool = TypePool::with_intrinsics();
        for ty in known_traits(pool.well_known) {
            let name = pool.get(ty).kind.trait_name().unwrap();
            pool.get_mut(ty).type_id = TypeId(0x4e45535354524954, name.as_u32().into());
        }
        for revision in 1..=10 {
            let bytes = encode_pool_revision(&pool, revision).unwrap();
            let restored = decode_pool(&bytes).unwrap();
            assert!(restored.identity_input().is_none());
            for ty in known_traits(pool.well_known) {
                assert_eq!(restored.get(ty).type_id, pool.get(ty).type_id);
                assert!(restored.stable_type_id(ty).is_err());
            }
            assert_eq!(restored.get(restored.null_type()).type_id, TypeId::ZERO);
        }
    }
}
