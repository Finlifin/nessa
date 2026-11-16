//! Checked TPOL v11 type-pool payload codec. Container framing is handled elsewhere.

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

pub(crate) fn encode_pool(pool: &TypePool) -> io::Result<Vec<u8>> {
    // Public registration APIs can hold unfinished descriptors. An archive
    // writer must reject them before producing a persistable pool.
    pool.validate()
        .map_err(|error| invalid(error.to_string()))?;
    if pool.has_unresolved_derived_methods() {
        return Err(invalid(
            "unresolved derived method sentinel in type metadata",
        ));
    }
    let snapshot = pool.snapshot();
    let input = snapshot
        .identity_input
        .as_ref()
        .ok_or_else(|| invalid("TPOL11 requires finalized identity provenance"))?;
    let mut writer = Writer {
        bytes: Vec::new(),
        items: 0,
    };
    writer.bytes(MAGIC)?;
    writer.u32(VERSION)?;
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
        writer.option(record.visible_scope)?;
        writer.list(&record.methods, Writer::method)
    })?;
    writer.list(&snapshot.vtables, |writer, table| {
        writer.index(table.trait_type)?;
        writer.index(table.implementor)?;
        writer.option(table.visible_scope)?;
        writer.list(&table.entries, |writer, entry| {
            if *entry == type_pool::DERIVE_FUNC_ID {
                return Err(invalid(
                    "unresolved derived method sentinel in type metadata",
                ));
            }
            writer.u32(*entry)
        })
    })?;
    for ty in known_traits(snapshot.well_known) {
        writer.index(ty)?;
    }
    writer.index(snapshot.null_type)?;
    writer.list(&snapshot.scopes, |writer, scope| {
        writer.option(scope.parent)?;
        writer.u32(scope.package)?;
        writer.option(scope.assoc_type.map(TypeIndex::as_u32))
    })?;
    writer.list(&snapshot.trait_schemas, |writer, schema| {
        writer.index(schema.trait_type)?;
        writer.list(&schema.slots, |writer, slot| {
            writer.index(slot.trait_owner)?;
            writer.name(slot.name)?;
            writer.trait_signature(slot.signature.as_ref())
        })
    })?;
    writer.list(&snapshot.associated_bindings, |writer, binding| {
        writer.index(binding.implementor)?;
        writer.index(binding.trait_type)?;
        writer.option(binding.visible_scope)?;
        writer.index(binding.trait_owner)?;
        writer.name(binding.name)?;
        writer.index(binding.value)
    })?;
    writer.list(&snapshot.associated_defaults, |writer, default| {
        writer.index(default.trait_owner)?;
        writer.name(default.name)?;
        writer.associated_expr(&default.expression, 0)
    })?;
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
    Ok(writer.bytes)
}

pub(crate) fn decode_pool(bytes: &[u8]) -> io::Result<TypePool> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("type metadata exceeds the byte limit"));
    }
    let mut reader = Reader {
        bytes,
        position: 0,
        items: 0,
    };
    if reader.take(4)? != MAGIC {
        return Err(invalid("unsupported type metadata header"));
    }
    let version = reader.u32()?;
    if version != VERSION {
        return Err(invalid(format!(
            "unsupported type metadata version {version}; expected {VERSION}"
        )));
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
    let methods = reader.list(4, |reader| reader.list(11, Reader::method))?;
    let trait_impls = reader.list(13, |reader| {
        let trait_type = reader.index()?;
        let implementor = reader.index()?;
        let visible_scope = reader.option()?;
        let methods = reader.list(11, Reader::method)?;
        Ok(TraitImplRecord {
            trait_type,
            implementor,
            methods,
            visible_scope,
        })
    })?;
    let vtables = reader.list(13, |reader| {
        let trait_type = reader.index()?;
        let implementor = reader.index()?;
        let visible_scope = reader.option()?;
        let entries = reader.list(4, |reader| {
            let entry = reader.u32()?;
            if entry == type_pool::DERIVE_FUNC_ID {
                return Err(invalid(
                    "unresolved derived method sentinel in type metadata",
                ));
            }
            Ok(entry)
        })?;
        Ok(VTable {
            trait_type,
            implementor,
            visible_scope,
            entries,
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
        json: reader.index()?,
    };
    let null_type = reader.index()?;
    let scopes = reader.list(6, |reader| {
        Ok(ScopeContext {
            parent: reader.option()?,
            package: reader.u32()?,
            assoc_type: reader.option()?.map(TypeIndex::from_raw),
        })
    })?;
    let trait_schemas = reader.list(8, |reader| {
        Ok(TraitDispatchSchema {
            trait_type: reader.index()?,
            slots: reader.list(9, |reader| {
                Ok(TraitMethodKey {
                    trait_owner: reader.index()?,
                    name: reader.name()?,
                    signature: reader.trait_signature()?,
                })
            })?,
        })
    })?;
    let associated_bindings = reader.list(21, |reader| {
        Ok(AssociatedTypeBinding {
            implementor: reader.index()?,
            trait_type: reader.index()?,
            visible_scope: reader.option()?,
            trait_owner: reader.index()?,
            name: reader.name()?,
            value: reader.index()?,
        })
    })?;
    let associated_defaults = reader.list(9, |reader| {
        Ok(AssociatedTypeDefault {
            trait_owner: reader.index()?,
            name: reader.name()?,
            expression: reader.associated_expr(0)?,
        })
    })?;
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
    let identity_input = Some(TypeIdentityInput {
        schema,
        packages,
        declarations,
    });
    if reader.position != bytes.len() {
        return Err(invalid("trailing bytes in type metadata"));
    }
    let pool = TypePool::restore(TypePoolSnapshot {
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
    .map_err(|error| invalid(error.to_string()))?;
    if pool.has_unresolved_derived_methods() {
        return Err(invalid(
            "unresolved derived method sentinel in type metadata",
        ));
    }
    Ok(pool)
}

fn known_traits(known: WellKnownTraits) -> [TypeIndex; 9] {
    [
        known.display,
        known.hash,
        known.eq,
        known.ord,
        known.partial_eq,
        known.partial_ord,
        known.iterator,
        known.into_iterator,
        known.json,
    ]
}

struct Writer {
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
        self.list(&signature.associated_paths, |writer, binding| {
            writer.index(binding.trait_owner)?;
            writer.name(binding.name)?;
            writer.type_path(&binding.path)
        })
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
                TraitTypeStep::IterationItem => (8, None),
                TraitTypeStep::FutureInner => (9, None),
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
        if method.func_id == type_pool::DERIVE_FUNC_ID {
            return Err(invalid(
                "unresolved derived method sentinel in type metadata",
            ));
        }
        self.name(method.name)?;
        self.u32(method.func_id)?;
        self.option(method.trait_impl.map(TypeIndex::as_u32))?;
        self.option(method.visible_scope)?;
        match method.access {
            MethodAccess::Public => self.u8(1),
            MethodAccess::Package(package) => {
                self.u8(2)?;
                self.u32(package)
            }
            MethodAccess::Private(scope) => {
                self.u8(3)?;
                self.u32(scope)
            }
        }
    }
    fn associated_expr(&mut self, expression: &AssociatedTypeExpr, depth: usize) -> io::Result<()> {
        self.items += 1;
        if depth >= 256 || self.items > MAX_ITEMS {
            return Err(invalid(
                "associated default template exceeds nesting or item limits",
            ));
        }
        match expression {
            AssociatedTypeExpr::Required => self.u8(7),
            AssociatedTypeExpr::IterationStep { item } => {
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
            AssociatedTypeExpr::Future { inner } => {
                self.u8(8)?;
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
            TypeKind::Future { inner } => {
                self.u8(15)?;
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
                self.u8(13)?;
                self.index(*trait_owner)?;
                self.name(*name)
            }
        }
    }
}

struct Reader<'a> {
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
        let name = self.name()?;
        let func_id = self.u32()?;
        if func_id == type_pool::DERIVE_FUNC_ID {
            return Err(invalid(
                "unresolved derived method sentinel in type metadata",
            ));
        }
        let trait_impl = self.option()?.map(TypeIndex::from_raw);
        let visible_scope = self.option()?;
        let access = match self.u8()? {
            1 => MethodAccess::Public,
            2 => MethodAccess::Package(self.u32()?),
            3 => MethodAccess::Private(self.u32()?),
            _ => return Err(invalid("invalid method access tag")),
        };
        Ok(MethodSlot {
            name,
            func_id,
            trait_impl,
            visible_scope,
            access,
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
        let associated_paths = self.bounded_list(12, 256, |reader| {
            Ok(TraitAssociatedPath {
                trait_owner: reader.index()?,
                name: reader.name()?,
                path: reader.type_path()?,
            })
        })?;
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
                8 => TraitTypeStep::IterationItem,
                9 => TraitTypeStep::FutureInner,
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
            7 => AssociatedTypeExpr::Required,
            8 => AssociatedTypeExpr::Future {
                inner: Box::new(self.associated_expr(depth + 1)?),
            },
            6 => AssociatedTypeExpr::IterationStep {
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
            14 => TypeKind::IterationStepTemplate {
                item: self.index()?,
            },
            15 => TypeKind::Future {
                inner: self.index()?,
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
            13 => TypeKind::AssociatedType {
                trait_owner: self.index()?,
                name: self.name()?,
            },
            _ => return Err(invalid("invalid type kind tag")),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn finalize_test_pool(pool: &mut TypePool) {
        let snapshot = pool.snapshot();
        let structural: BTreeSet<_> = snapshot.structural_types.iter().copied().collect();
        let mut declarations = Vec::new();
        for (index, info) in snapshot.types.iter().enumerate() {
            let ty = TypeIndex::from_raw(index as u32);
            if pool.is_reserved_type(ty) {
                continue;
            }
            let path = match &info.kind {
                TypeKind::Struct { name, .. }
                | TypeKind::Newtype { name, .. }
                | TypeKind::Trait { name, .. }
                | TypeKind::Module { name } => Some(vec![
                    IdentityPathSegment::Lexical {
                        kind: 0,
                        ordinal: index as u32,
                    },
                    IdentityPathSegment::Named(str_interner::get(*name)),
                ]),
                TypeKind::Enum { name, .. } if !structural.contains(&ty) => Some(vec![
                    IdentityPathSegment::Lexical {
                        kind: 0,
                        ordinal: index as u32,
                    },
                    IdentityPathSegment::Named(str_interner::get(*name)),
                ]),
                TypeKind::Effect { .. } if !structural.contains(&ty) => {
                    Some(vec![IdentityPathSegment::Lexical {
                        kind: 6,
                        ordinal: index as u32,
                    }])
                }
                _ => None,
            };
            if let Some(path) = path {
                declarations.push(NominalTypeProvenance {
                    type_index: ty,
                    package: 0,
                    path,
                    last_stable_version: "1.0.0".into(),
                });
            }
        }
        pool.finalize_type_identities(TypeIdentityInput {
            schema: 1,
            packages: vec![PackageTypeContext {
                identity_schema: 1,
                identity: [0x11; 16],
                qualified_name: "org.test/codec".into(),
                version: "1.0.0".into(),
            }],
            declarations,
        })
        .unwrap();
    }

    fn rich_pool() -> TypePool {
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
                assoc_type: Some(Intrinsic::I64.type_index()),
            },
        ])
        .unwrap();
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
            type_id: TypeId::ZERO,
            size: 8,
            align: 8,
        });
        pool.register(TypeInfo {
            kind: TypeKind::Enum {
                name: str_interner::intern("Choice"),
                variants: vec![VariantInfo {
                    name: str_interner::intern("Yes"),
                    tag: 17,
                    fields: vec![field],
                }],
            },
            type_id: TypeId::ZERO,
            size: 16,
            align: 8,
        });
        pool.register(TypeInfo {
            kind: TypeKind::Newtype {
                name: str_interner::intern("Wrapped"),
                inner: i64_type,
            },
            type_id: TypeId::ZERO,
            size: 8,
            align: 8,
        });
        pool.register(TypeInfo {
            kind: TypeKind::Module {
                name: str_interner::intern("Module"),
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("CustomTrait"),
                parents: vec![pool.well_known.display],
                assoc_types: vec![
                    (str_interner::intern("Item"), TypeIndex::INVALID),
                    (str_interner::intern("Other"), i64_type),
                ],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        for kind in [
            TypeKind::Typealias {
                name: str_interner::intern("Alias"),
                target: nominal,
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
        ] {
            pool.intern_structural(kind);
        }
        pool.intern_iteration_step(i64_type).unwrap();
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
            func_id: 7,
            trait_impl: Some(trait_type),
            visible_scope: Some(1),
            access: MethodAccess::Public,
        };
        pool.add_method(nominal, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            trait_type,
            implementor: nominal,
            visible_scope: Some(1),
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            trait_type,
            implementor: nominal,
            visible_scope: Some(1),
            entries: vec![7],
        });
        finalize_test_pool(&mut pool);
        pool
    }

    #[test]
    fn every_type_shape_and_dispatch_metadata_round_trip_on_tpol11() {
        let pool = rich_pool();
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[..8], b"TPOL\x0b\x00\x00\x00");
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
        assert_eq!(decoded.snapshot().vtables[0].entries, vec![7]);
        assert_eq!(
            decoded.snapshot().trait_impls[0].methods[0].visible_scope,
            Some(1)
        );
        assert_eq!(encode_pool(&decoded).unwrap(), bytes);
    }

    #[test]
    fn rejects_unfinalized_pools_and_non_v11_versions() {
        let unfinalized = TypePool::with_intrinsics();
        let error = encode_pool(&unfinalized).unwrap_err();
        assert!(error.to_string().contains("TPOL11 requires finalized"));

        let bytes = encode_pool(&rich_pool()).unwrap();
        for version in [0u32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, u32::MAX] {
            let mut bad = bytes.clone();
            bad[4..8].copy_from_slice(&version.to_le_bytes());
            let error = decode_pool(&bad).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                error
                    .to_string()
                    .contains("unsupported type metadata version"),
                "version {version}: {error}"
            );
        }
    }

    #[test]
    fn rejects_unresolved_derive_sentinel_in_methods_trait_impls_and_vtables() {
        let sentinel_bytes = type_pool::DERIVE_FUNC_ID.to_le_bytes();

        // 1. Inherent method with DERIVE_FUNC_ID rejected on encode and decode.
        let mut pool = TypePool::with_intrinsics();
        let owner = Intrinsic::I64.type_index();
        let method_marker = "CodecSentinelMethod";
        pool.add_method(
            owner,
            MethodSlot {
                name: str_interner::intern(method_marker),
                func_id: 42,
                trait_impl: None,
                visible_scope: None,
                access: MethodAccess::Public,
            },
        );
        finalize_test_pool(&mut pool);
        let valid_method_bytes = encode_pool(&pool).unwrap();

        let mut bad_method_pool = TypePool::restore(pool.snapshot()).unwrap();
        bad_method_pool.add_method(
            owner,
            MethodSlot {
                name: str_interner::intern("UnresolvedInherent"),
                func_id: type_pool::DERIVE_FUNC_ID,
                trait_impl: None,
                visible_scope: None,
                access: MethodAccess::Public,
            },
        );
        finalize_test_pool(&mut bad_method_pool);
        assert!(
            encode_pool(&bad_method_pool)
                .unwrap_err()
                .to_string()
                .contains("unresolved derived method sentinel")
        );

        let pos = valid_method_bytes
            .windows(method_marker.len())
            .position(|w| w == method_marker.as_bytes())
            .unwrap()
            + method_marker.len();
        assert_eq!(&valid_method_bytes[pos..pos + 4], &42u32.to_le_bytes());
        let mut bad_wire = valid_method_bytes;
        bad_wire[pos..pos + 4].copy_from_slice(&sentinel_bytes);
        assert!(
            decode_pool(&bad_wire)
                .err()
                .unwrap()
                .to_string()
                .contains("unresolved derived method sentinel")
        );

        // 2. Trait impl and vtable with DERIVE_FUNC_ID rejected on encode and decode.
        let mut trait_pool = TypePool::with_intrinsics();
        let trait_type = trait_pool.well_known.display;
        let trait_marker = "CodecTraitMethod";
        let slot = MethodSlot {
            name: str_interner::intern(trait_marker),
            func_id: 55,
            trait_impl: Some(trait_type),
            visible_scope: None,
            access: MethodAccess::Public,
        };
        trait_pool.add_trait_impl(TraitImplRecord {
            trait_type,
            implementor: owner,
            visible_scope: None,
            methods: vec![slot],
        });
        trait_pool.add_vtable(VTable {
            trait_type,
            implementor: owner,
            visible_scope: None,
            entries: vec![55],
        });
        finalize_test_pool(&mut trait_pool);
        let valid_trait_bytes = encode_pool(&trait_pool).unwrap();

        // Corrupt trait_impl method func_id on wire.
        let tpos = valid_trait_bytes
            .windows(trait_marker.len())
            .position(|w| w == trait_marker.as_bytes())
            .unwrap()
            + trait_marker.len();
        assert_eq!(&valid_trait_bytes[tpos..tpos + 4], &55u32.to_le_bytes());
        let mut bad_trait_wire = valid_trait_bytes.clone();
        bad_trait_wire[tpos..tpos + 4].copy_from_slice(&sentinel_bytes);
        assert!(
            decode_pool(&bad_trait_wire)
                .err()
                .unwrap()
                .to_string()
                .contains("unresolved derived method sentinel")
        );

        // Corrupt vtable entry func_id on wire.
        let mut vtable_header = Vec::new();
        vtable_header.extend_from_slice(&trait_type.as_u32().to_le_bytes());
        vtable_header.extend_from_slice(&owner.as_u32().to_le_bytes());
        vtable_header.push(0); // visible_scope = None
        vtable_header.extend_from_slice(&1u32.to_le_bytes()); // entries len = 1
        vtable_header.extend_from_slice(&55u32.to_le_bytes());
        let vpos = valid_trait_bytes
            .windows(vtable_header.len())
            .rposition(|w| w == vtable_header.as_slice())
            .unwrap()
            + vtable_header.len()
            - 4;
        let mut bad_vtable_wire = valid_trait_bytes;
        bad_vtable_wire[vpos..vpos + 4].copy_from_slice(&sentinel_bytes);
        assert!(
            decode_pool(&bad_vtable_wire)
                .err()
                .unwrap()
                .to_string()
                .contains("unresolved derived method sentinel")
        );

        // Encode-side trait_impl DERIVE_FUNC_ID rejection.
        let mut bad_trait_pool = TypePool::with_intrinsics();
        bad_trait_pool.add_trait_impl(TraitImplRecord {
            trait_type,
            implementor: owner,
            visible_scope: None,
            methods: vec![MethodSlot {
                name: str_interner::intern(trait_marker),
                func_id: type_pool::DERIVE_FUNC_ID,
                trait_impl: Some(trait_type),
                visible_scope: None,
                access: MethodAccess::Public,
            }],
        });
        bad_trait_pool.add_vtable(VTable {
            trait_type,
            implementor: owner,
            visible_scope: None,
            entries: vec![55],
        });
        finalize_test_pool(&mut bad_trait_pool);
        assert!(
            encode_pool(&bad_trait_pool)
                .unwrap_err()
                .to_string()
                .contains("unresolved derived method sentinel")
        );

        // Encode-side vtable DERIVE_FUNC_ID rejection.
        let mut bad_vtable_pool = TypePool::with_intrinsics();
        bad_vtable_pool.add_trait_impl(TraitImplRecord {
            trait_type,
            implementor: owner,
            visible_scope: None,
            methods: vec![MethodSlot {
                name: str_interner::intern(trait_marker),
                func_id: 55,
                trait_impl: Some(trait_type),
                visible_scope: None,
                access: MethodAccess::Public,
            }],
        });
        bad_vtable_pool.add_vtable(VTable {
            trait_type,
            implementor: owner,
            visible_scope: None,
            entries: vec![type_pool::DERIVE_FUNC_ID],
        });
        finalize_test_pool(&mut bad_vtable_pool);
        assert!(
            encode_pool(&bad_vtable_pool)
                .unwrap_err()
                .to_string()
                .contains("unresolved derived method sentinel")
        );
    }

    #[test]
    fn access_tags_and_scope_payloads_reject_retired_tag_zero_and_corrupt_metadata() {
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
        ]
        .into_iter()
        .enumerate()
        {
            pool.add_method(
                owner,
                MethodSlot {
                    name: str_interner::intern(&format!("codec_method_{index}")),
                    func_id: index as u32 + 1,
                    trait_impl: None,
                    visible_scope: Some(1),
                    access,
                },
            );
        }
        finalize_test_pool(&mut pool);
        let bytes = encode_pool(&pool).unwrap();
        let decoded = decode_pool(&bytes).unwrap();
        assert_eq!(decoded.scopes(), scopes);
        for (original, restored) in pool.methods_of(owner).iter().zip(decoded.methods_of(owner)) {
            assert_eq!(restored.access, original.access);
            assert_eq!(restored.visible_scope, original.visible_scope);
            assert_eq!(restored.func_id, original.func_id);
        }
        assert_eq!(encode_pool(&decoded).unwrap(), bytes);

        // Single-scope pool for targeted access tag and scope byte corruption.
        let mut single = TypePool::with_intrinsics();
        single
            .install_scopes(vec![ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            }])
            .unwrap();
        let name = "ScopeCodecPrivate";
        single.add_method(
            owner,
            MethodSlot {
                name: str_interner::intern(name),
                func_id: 9,
                trait_impl: None,
                visible_scope: None,
                access: MethodAccess::Private(0),
            },
        );
        finalize_test_pool(&mut single);
        let bytes = encode_pool(&single).unwrap();
        let name_offset = bytes
            .windows(name.len())
            .position(|slice| slice == name.as_bytes())
            .unwrap();
        let access_offset = name_offset + name.len() + 6;
        assert_eq!(bytes[access_offset], 3);
        for bad_tag in [0u8, 4, 9, 255] {
            let mut damaged = bytes.clone();
            if bad_tag == 0 {
                // Tag 0 (retired LegacyUnknown) had no trailing u32 payload; remove the 4-byte scope payload too.
                damaged[access_offset] = 0;
                damaged.drain(access_offset + 1..access_offset + 5);
            } else {
                damaged[access_offset] = bad_tag;
            }
            let error = decode_pool(&damaged).err().unwrap();
            assert!(error.to_string().contains("access tag"), "{error}");
        }
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

        // Locate the single root scope (count=1 followed by [0, 0,0,0,0, 0]) before empty schemas/bindings/defaults.
        let mut scope_pattern = Vec::new();
        scope_pattern.extend_from_slice(&1u32.to_le_bytes());
        scope_pattern.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        scope_pattern.extend_from_slice(&0u32.to_le_bytes());
        scope_pattern.extend_from_slice(&0u32.to_le_bytes());
        scope_pattern.extend_from_slice(&0u32.to_le_bytes());
        let scope_count_offset = bytes
            .windows(scope_pattern.len())
            .rposition(|w| w == scope_pattern.as_slice())
            .unwrap();
        let scope_offset = scope_count_offset + 4;
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
        damaged[scope_count_offset..scope_offset].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_pool(&damaged).is_err());
    }

    #[test]
    fn inline_names_are_interned_without_persistent_process_ids() {
        let mut pool = TypePool::with_intrinsics();
        let alias = pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("CodecOldAlias"),
                target: Intrinsic::I64.type_index(),
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        finalize_test_pool(&mut pool);
        let mut bytes = encode_pool(&pool).unwrap();
        let old = b"CodecOldAlias";
        let position = bytes
            .windows(old.len())
            .position(|window| window == old)
            .unwrap();
        bytes[position..position + old.len()].copy_from_slice(b"CodecNewAlias");
        for index in 0..50 {
            str_interner::intern(&format!("metadata_noise_{index}"));
        }
        let decoded = decode_pool(&bytes).unwrap();
        let TypeKind::Typealias { name, .. } = decoded.get(alias).kind else {
            panic!("expected typealias")
        };
        assert_eq!(str_interner::get(name), "CodecNewAlias");
    }

    #[test]
    fn dependent_iteration_templates_associated_schemas_and_defaults_roundtrip() {
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
        let item_name = str_interner::intern("Item");
        let result_name = str_interner::intern("Result");
        let req_name = str_interner::intern("RequiredAssoc");
        let item = pool.intern_structural(TypeKind::AssociatedType {
            trait_owner: owner,
            name: item_name,
        });
        let result = pool.intern_structural(TypeKind::AssociatedType {
            trait_owner: owner,
            name: result_name,
        });
        let req = pool.intern_structural(TypeKind::AssociatedType {
            trait_owner: owner,
            name: req_name,
        });
        if let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind {
            *assoc_types = vec![(item_name, item), (result_name, result), (req_name, req)];
        }
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: item_name,
            expression: AssociatedTypeExpr::Concrete(Intrinsic::I64.type_index()),
        })
        .unwrap();
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: result_name,
            expression: AssociatedTypeExpr::IterationStep {
                item: Box::new(AssociatedTypeExpr::Binding {
                    trait_owner: owner,
                    name: item_name,
                }),
            },
        })
        .unwrap();
        pool.register_associated_default(AssociatedTypeDefault {
            trait_owner: owner,
            name: req_name,
            expression: AssociatedTypeExpr::Required,
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
                        name: item_name,
                        path: vec![TraitTypeStep::Return, TraitTypeStep::IterationItem],
                    }],
                }),
            }],
        })
        .unwrap();
        finalize_test_pool(&mut pool);
        let bytes = encode_pool(&pool).unwrap();
        assert_eq!(&bytes[4..8], &11u32.to_le_bytes());
        let restored = decode_pool(&bytes).unwrap();
        assert_eq!(
            restored.associated_defaults_snapshot(),
            pool.associated_defaults_snapshot()
        );
        assert_eq!(
            restored.trait_schemas_snapshot(),
            pool.trait_schemas_snapshot()
        );
        assert_eq!(encode_pool(&restored).unwrap(), bytes);
    }

    #[test]
    fn signature_and_default_expression_codecs_preserve_all_variants_and_reject_corruption() {
        let signature = TraitMethodSignature {
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
                TraitTypeStep::IterationItem,
            ]],
            parameter_kinds: vec![
                TraitParameterKind::Receiver,
                TraitParameterKind::Required,
                TraitParameterKind::Optional,
                TraitParameterKind::ListVariadic,
                TraitParameterKind::MapVariadic,
            ],
            associated_paths: vec![TraitAssociatedPath {
                trait_owner: TypeIndex::from_raw(11),
                name: str_interner::intern("Item"),
                path: vec![TraitTypeStep::Return, TraitTypeStep::OptionalInner],
            }],
        };
        let mut writer = Writer {
            bytes: Vec::new(),
            items: 0,
        };
        writer.trait_signature(Some(&signature)).unwrap();
        let mut reader = Reader {
            bytes: &writer.bytes,
            position: 0,
            items: 0,
        };
        assert_eq!(reader.trait_signature().unwrap(), Some(signature));
        assert_eq!(reader.position, writer.bytes.len());

        for (offset, value) in [(0, 2), (13, 255), (writer.bytes.len() - 1, 255)] {
            let mut damaged = writer.bytes.clone();
            damaged[offset] = value;
            let mut reader = Reader {
                bytes: &damaged,
                position: 0,
                items: 0,
            };
            assert!(reader.trait_signature().is_err());
        }

        let owner = Intrinsic::I64.type_index();
        let expression = AssociatedTypeExpr::Function {
            parameters: vec![
                AssociatedTypeExpr::SelfType { trait_owner: owner },
                AssociatedTypeExpr::Binding {
                    trait_owner: owner,
                    name: str_interner::intern("Item"),
                },
                AssociatedTypeExpr::Required,
            ],
            return_type: Box::new(AssociatedTypeExpr::Optional {
                inner: Box::new(AssociatedTypeExpr::Tuple {
                    elements: vec![
                        AssociatedTypeExpr::Concrete(Intrinsic::Str.type_index()),
                        AssociatedTypeExpr::IterationStep {
                            item: Box::new(AssociatedTypeExpr::SelfType { trait_owner: owner }),
                        },
                    ],
                }),
            }),
        };
        let mut expr_writer = Writer {
            bytes: Vec::new(),
            items: 0,
        };
        expr_writer.associated_expr(&expression, 0).unwrap();
        let mut expr_reader = Reader {
            bytes: &expr_writer.bytes,
            position: 0,
            items: 0,
        };
        assert_eq!(expr_reader.associated_expr(0).unwrap(), expression);
        assert_eq!(expr_reader.position, expr_writer.bytes.len());

        let mut deep = vec![3; 256];
        deep.push(0);
        deep.extend_from_slice(&owner.as_u32().to_le_bytes());
        let mut deep_reader = Reader {
            bytes: &deep,
            position: 0,
            items: 0,
        };
        assert!(deep_reader.associated_expr(0).is_err());
    }

    #[test]
    fn truncation_tags_counts_utf8_trailing_bytes_and_identity_tampering_are_rejected() {
        let pool = rich_pool();
        let bytes = encode_pool(&pool).unwrap();
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

        // Tamper with a finalized nominal TypeId or provenance version.
        let nominal = (0..pool.len())
            .map(|i| TypeIndex::from_raw(i as u32))
            .find(|&ty| pool.display_name(ty).as_deref() == Some("Record"))
            .unwrap();
        let id = pool.get(nominal).type_id;
        let pair = [id.hi().to_le_bytes(), id.lo().to_le_bytes()].concat();
        let id_offset = bytes.windows(16).position(|w| w == pair).unwrap();
        for relative in [0, 8] {
            let mut changed = bytes.clone();
            changed[id_offset + relative] ^= 1;
            assert_eq!(
                decode_pool(&changed).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
        }
        let ver_offset = bytes.windows(5).rposition(|w| w == b"1.0.0").unwrap();
        let mut changed = bytes;
        changed[ver_offset + 4] = b'9';
        assert!(decode_pool(&changed).is_err());

        // Invalid writer string handles and reader boolean/option tags.
        let mut writer = Writer {
            bytes: Vec::new(),
            items: 0,
        };
        assert!(writer.name(StrId::from_raw(u32::MAX)).is_err());
        let mut reader = Reader {
            bytes: &[2],
            position: 0,
            items: 0,
        };
        assert!(reader.boolean().is_err());
        let mut reader = Reader {
            bytes: &[3],
            position: 0,
            items: 0,
        };
        assert!(reader.option().is_err());
    }
}
