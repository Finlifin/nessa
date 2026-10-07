use str_interner::StrId;

mod access;
mod associated;
mod associated_defaults;
mod collections;
mod dispatch;
mod errors;
mod functions;
pub use errors::{ErrorDomain, ErrorShape, ErrorTypeError};
mod identity;
mod identity_encoding;
#[cfg(test)]
mod identity_tests;
mod iteration_step;
mod native_derived;
mod numeric;
mod signatures;
mod snapshot;
mod traits;

pub use access::{MethodAccess, MethodAccessError, ScopeContext};
pub use associated::AssociatedTypeBinding;
pub use associated_defaults::{AssociatedTypeDefault, AssociatedTypeExpr};
pub use collections::{
    CollectionRole, LIST_BUFFER_TYPE_ID, LIST_TYPE_ID, MAP_BUFFER_TYPE_ID, MAP_TYPE_ID,
};
pub use dispatch::{TraitDispatchSchema, TraitMethodDescriptor, TraitMethodKey, TraitProofError};
pub use functions::DerivedMethodError;
pub use identity::{
    IdentityPathSegment, NominalTypeProvenance, PackageTypeContext, TypeIdentityError,
    TypeIdentityInput,
};
pub use iteration_step::{ITERATION_DONE_TAG, ITERATION_YIELDED_TAG};
pub use native_derived::{NativeDerivedError, NativeDerivedMethod};
pub use signatures::{
    TraitAssociatedPath, TraitMethodSignature, TraitParameterKind, TraitSignatureError,
    TraitTypeStep,
};
pub use snapshot::{SnapshotError, TypePoolSnapshot};
pub use traits::TraitLookupError;

/// Sentinel func_id indicating a compiler-derived (synthesized) method.
/// Used by `derive Eq, Ord, ...` — the interpreter generates the
/// implementation on the fly when it encounters this marker.
pub const DERIVE_FUNC_ID: u32 = 0xFFFF_FFFE;

// ---------------------------------------------------------------------------
// Well-known trait indices — built-in traits the language depends on
// ---------------------------------------------------------------------------

/// Type indices for well-known traits that the language deeply depends on.
/// Registered after intrinsic types during engine initialization.
#[derive(Debug, Clone, Copy)]
pub struct WellKnownTraits {
    pub display: TypeIndex,
    pub hash: TypeIndex,
    pub eq: TypeIndex,
    pub ord: TypeIndex,
    pub partial_eq: TypeIndex,
    pub partial_ord: TypeIndex,
    pub iterator: TypeIndex,
    pub into_iterator: TypeIndex,
}

impl WellKnownTraits {
    pub const UNINITIALIZED: Self = Self {
        display: TypeIndex::INVALID,
        hash: TypeIndex::INVALID,
        eq: TypeIndex::INVALID,
        ord: TypeIndex::INVALID,
        partial_eq: TypeIndex::INVALID,
        partial_ord: TypeIndex::INVALID,
        iterator: TypeIndex::INVALID,
        into_iterator: TypeIndex::INVALID,
    };

    /// Well-known trait names in registration order.
    pub const NAMES: &'static [&'static str] = &[
        "Display",
        "Hash",
        "Eq",
        "Ord",
        "PartialEq",
        "PartialOrd",
        "Iterator",
        "IntoIterator",
    ];
}

// ---------------------------------------------------------------------------
// TypeIndex — compact handle into the TypePool
// ---------------------------------------------------------------------------

/// A compact handle representing a registered type. The inner `u32` is the
/// index into `TypePool::types`.  Stored inside heap object headers at runtime.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeIndex(u32);

impl TypeIndex {
    pub const INVALID: Self = Self(u32::MAX);

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl std::fmt::Debug for TypeIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TypeIndex({})", self.0)
    }
}

// ---------------------------------------------------------------------------
// TypeId — 128-bit stable hash identifying a type across compilations
// ---------------------------------------------------------------------------

/// 128-bit stable type identifier computed from package identity, version,
/// layout, and symbol path.  Used at runtime for error-qualified-type tags,
/// `Any` downcasts, and serialization.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeId(pub u64, pub u64);

impl TypeId {
    pub const ZERO: Self = Self(0, 0);

    pub const fn from_pair(hi: u64, lo: u64) -> Self {
        Self(hi, lo)
    }

    pub const fn hi(self) -> u64 {
        self.0
    }

    pub const fn lo(self) -> u64 {
        self.1
    }
}

impl std::fmt::Debug for TypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TypeId({:#018x}_{:#018x})", self.0, self.1)
    }
}

// ---------------------------------------------------------------------------
// Intrinsic type enumeration — built-in types known to the VM
// ---------------------------------------------------------------------------

/// Intrinsic types recognised by the engine.  Each gets a well-known
/// [`TypeIndex`] assigned during initialisation (indices 0..N).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Intrinsic {
    // Unsigned integers
    U8 = 0,
    U16,
    U32,
    U64,
    U128,
    Usize,
    // Signed integers
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    // Floating-point
    F32,
    F64,
    // Other primitives
    Bool,
    Char,
    Str, // String
    // Structural
    Unit,
    // Lattice bounds
    Any,
    NoReturn,
    // Meta
    Type,
    // Runtime-managed
    Closure,
    Continuation,
}

impl Intrinsic {
    pub const COUNT: usize = Self::Continuation as usize + 1;

    /// All intrinsic variants in discriminant order.
    pub const ALL: &'static [Intrinsic] = &[
        Self::U8,
        Self::U16,
        Self::U32,
        Self::U64,
        Self::U128,
        Self::Usize,
        Self::I8,
        Self::I16,
        Self::I32,
        Self::I64,
        Self::I128,
        Self::Isize,
        Self::F32,
        Self::F64,
        Self::Bool,
        Self::Char,
        Self::Str,
        Self::Unit,
        Self::Any,
        Self::NoReturn,
        Self::Type,
        Self::Closure,
        Self::Continuation,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::Usize => "usize",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::I128 => "i128",
            Self::Isize => "isize",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Char => "char",
            Self::Str => "String",
            Self::Unit => "Unit",
            Self::Any => "Any",
            Self::NoReturn => "NoReturn",
            Self::Type => "Type",
            Self::Closure => "Closure",
            Self::Continuation => "Continuation",
        }
    }

    /// Whether this intrinsic is a numeric type eligible for lifting.
    pub const fn is_numeric(self) -> bool {
        (self as u8) <= (Self::F64 as u8)
    }

    /// Whether this is an integer type (signed or unsigned).
    pub const fn is_integer(self) -> bool {
        (self as u8) <= (Self::Isize as u8)
    }

    pub const fn type_index(self) -> TypeIndex {
        TypeIndex(self as u32)
    }
}

// ---------------------------------------------------------------------------
// TypeKind — discriminated union of type shapes
// ---------------------------------------------------------------------------

/// The structural shape of a type registered in the pool.
#[derive(Debug, Clone)]
pub enum TypeKind {
    /// One of the built-in intrinsic types.
    Intrinsic(Intrinsic),

    /// A user-defined struct type.
    Struct { name: StrId, fields: Vec<FieldInfo> },

    /// A user-defined enum (sum) type.
    Enum {
        name: StrId,
        variants: Vec<VariantInfo>,
    },

    /// A type-alias (`typealias`): transparent, same identity as target.
    Typealias { name: StrId, target: TypeIndex },

    /// A newtype wrapper: same layout, distinct identity.
    Newtype { name: StrId, inner: TypeIndex },

    /// A tuple type, e.g. `(i32, String)`.
    Tuple { elements: Vec<TypeIndex> },

    /// A function / closure type: `fn(A, B) -> R`.
    Function {
        params: Vec<TypeIndex>,
        ret: TypeIndex,
    },

    /// An effect type: `async? effect(A) -> R`.
    Effect {
        params: Vec<TypeIndex>,
        ret: TypeIndex,
        is_async: bool,
    },

    /// Optional type `?T`.
    Optional { inner: TypeIndex },

    /// Error-qualified type `!E T`.
    ErrorQualified {
        errors: Vec<TypeIndex>,
        inner: TypeIndex,
    },

    /// Effect-qualified type `#E T`.
    EffectQualified {
        effects: Vec<TypeIndex>,
        inner: TypeIndex,
    },

    /// Module (`mod`) — no instances, just a namespace.
    Module { name: StrId },

    /// Abstract associated type binder; never a runtime value type.
    AssociatedType { trait_owner: TypeIndex, name: StrId },
    /// Compiler-only tagged result before Item/Self specialization; never an object layout.
    IterationStepTemplate { item: TypeIndex },

    /// A trait definition type.
    Trait {
        name: StrId,
        /// Parent (super) traits that implementors must also satisfy.
        parents: Vec<TypeIndex>,
        /// Associated type declarations: (name, default_type).
        assoc_types: Vec<(StrId, TypeIndex)>,
    },
}

impl TypeKind {
    /// Return the trait name if this is a Trait kind.
    pub fn trait_name(&self) -> Option<StrId> {
        match self {
            TypeKind::Trait { name, .. } => Some(*name),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Field / Variant descriptors
// ---------------------------------------------------------------------------

/// A field inside a struct type.
#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: StrId,
    pub ty: TypeIndex,
    pub has_default: bool,
    pub offset: u32,
}

/// A variant inside an enum type.
#[derive(Debug, Clone)]
pub struct VariantInfo {
    pub name: StrId,
    pub tag: u32,
    pub fields: Vec<FieldInfo>,
}

// ---------------------------------------------------------------------------
// TypeInfo — full metadata for a single type
// ---------------------------------------------------------------------------

/// Complete metadata record for a registered type.
#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub kind: TypeKind,
    pub type_id: TypeId,
    /// Size in bytes of an instance of this type (0 for unsized / zero-sized).
    pub size: u32,
    /// Alignment in bytes.
    pub align: u32,
}

// ---------------------------------------------------------------------------
// MethodSlot — an entry in a type's method table
// ---------------------------------------------------------------------------

/// Identifies a method attached to a type (via impl / extend).
#[derive(Debug, Clone)]
pub struct MethodSlot {
    /// Declaration visibility, independent of the lexical `extend` restriction.
    pub access: MethodAccess,
    pub name: StrId,
    /// Index of the function in the bytecode store.
    pub func_id: u32,
    /// If this method belongs to a trait impl, which trait.
    pub trait_impl: Option<TypeIndex>,
    /// Scope restriction for `extend`-introduced methods.
    /// `None` means globally visible (from `impl`).
    /// `Some(scope_id)` means only visible within that scope and its children.
    pub visible_scope: Option<u32>,
}

// ---------------------------------------------------------------------------
// TraitImpl — records that a type implements a trait
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TraitImplRecord {
    /// Independent lexical identity of an extension implementation.
    pub visible_scope: Option<u32>,
    pub trait_type: TypeIndex,
    pub implementor: TypeIndex,
    pub methods: Vec<MethodSlot>,
}

// ---------------------------------------------------------------------------
// VTable — virtual method table for trait dynamic dispatch
// ---------------------------------------------------------------------------

/// A vtable maps trait method slot indices to concrete func_ids.
/// `entries[i]` is the func_id for the `i`-th method of the trait
/// (including inherited methods from parent traits).
#[derive(Debug, Clone)]
pub struct VTable {
    /// Must exactly match the corresponding trait implementation scope.
    pub visible_scope: Option<u32>,
    /// The trait this vtable is for.
    pub trait_type: TypeIndex,
    /// The concrete type that implements the trait.
    pub implementor: TypeIndex,
    /// Ordered func_ids, one per method slot in the trait.
    pub entries: Vec<u32>,
}

// ---------------------------------------------------------------------------
// TypePool — the central type registry
// ---------------------------------------------------------------------------

/// Central registry for all types in the program.  Populated during
/// compilation (resolution phase) and serialised into NSBC archives.
/// At runtime the pool is read-only.
pub struct TypePool {
    /// All registered type descriptors, indexed by `TypeIndex`.
    types: Vec<TypeInfo>,
    /// Indices created by structural interning. Raw registrations may represent
    /// nominal declarations even when their descriptor resembles a signature.
    structural_types: Vec<TypeIndex>,
    /// TypeId → TypeIndex reverse lookup.
    id_to_index: std::collections::HashMap<TypeId, TypeIndex>,
    /// Per-type method tables.
    methods: Vec<Vec<MethodSlot>>,
    /// Archive-local lexical scopes and package identities.
    scopes: Vec<ScopeContext>,
    scope_packages: std::collections::HashSet<u32>,
    /// Trait implementation records.
    trait_impls: Vec<TraitImplRecord>,
    /// Virtual method tables for trait dynamic dispatch.
    vtables: Vec<VTable>,
    trait_schemas: Vec<TraitDispatchSchema>,
    associated_bindings: Vec<AssociatedTypeBinding>,
    associated_defaults: Vec<AssociatedTypeDefault>,
    /// Well-known trait type indices.
    pub well_known: WellKnownTraits,
    /// Canonical null-only type, initialized after the intrinsic trait prefix.
    null_type: TypeIndex,
    identity_input: Option<TypeIdentityInput>,
    identity_dirty: std::sync::atomic::AtomicBool,
    identity_known: [TypeIndex; 8],
}

impl TypePool {
    /// Create a new, empty pool.
    pub fn new() -> Self {
        Self {
            types: Vec::new(),
            structural_types: Vec::new(),
            id_to_index: std::collections::HashMap::new(),
            methods: Vec::new(),
            scopes: Vec::new(),
            scope_packages: std::collections::HashSet::new(),
            trait_impls: Vec::new(),
            vtables: Vec::new(),
            trait_schemas: Vec::new(),
            associated_bindings: Vec::new(),
            associated_defaults: Vec::new(),
            well_known: WellKnownTraits::UNINITIALIZED,
            null_type: TypeIndex::INVALID,
            identity_input: None,
            identity_dirty: std::sync::atomic::AtomicBool::new(false),
            identity_known: [TypeIndex::INVALID; 8],
        }
    }

    /// Create a pool pre-populated with all intrinsic types.
    pub fn with_intrinsics() -> Self {
        let mut pool = Self::new();
        pool.register_intrinsics();
        pool.register_collection_roles();
        pool
    }

    // -- Registration --------------------------------------------------------

    /// Register intrinsic types at well-known indices 0..Intrinsic::COUNT.
    fn register_intrinsics(&mut self) {
        debug_assert!(self.types.is_empty(), "intrinsics must be first");
        for &intr in Intrinsic::ALL {
            let (size, align) = intrinsic_layout(intr);
            let type_id = intrinsic_type_id(intr);
            let info = TypeInfo {
                kind: TypeKind::Intrinsic(intr),
                type_id,
                size,
                align,
            };
            let idx = self.push(info);
            debug_assert_eq!(idx.as_u32(), intr as u32);
        }
        self.register_well_known_traits();
        self.null_type = self.intern_structural(TypeKind::Optional {
            inner: Intrinsic::NoReturn.type_index(),
        });
    }

    /// Register well-known trait types immediately after intrinsics.
    fn register_well_known_traits(&mut self) {
        let display_name = str_interner::intern("Display");
        let hash_name = str_interner::intern("Hash");
        let eq_name = str_interner::intern("Eq");
        let ord_name = str_interner::intern("Ord");
        let partial_eq_name = str_interner::intern("PartialEq");
        let partial_ord_name = str_interner::intern("PartialOrd");
        let iterator_name = str_interner::intern("Iterator");
        let into_iterator_name = str_interner::intern("IntoIterator");

        let mk = |pool: &mut Self, name: StrId, ordinal: u64| -> TypeIndex {
            pool.push(TypeInfo {
                kind: TypeKind::Trait {
                    name,
                    parents: Vec::new(),
                    assoc_types: Vec::new(),
                },
                type_id: identity::bootstrap_id(ordinal),
                size: 0,
                align: 0,
            })
        };

        self.well_known = WellKnownTraits {
            display: mk(self, display_name, 1),
            hash: mk(self, hash_name, 2),
            eq: mk(self, eq_name, 3),
            ord: mk(self, ord_name, 4),
            partial_eq: mk(self, partial_eq_name, 5),
            partial_ord: mk(self, partial_ord_name, 6),
            iterator: mk(self, iterator_name, 7),
            into_iterator: mk(self, into_iterator_name, 8),
        };
    }

    /// Register a new type, returning its [`TypeIndex`].
    pub fn register(&mut self, info: TypeInfo) -> TypeIndex {
        self.push(info)
    }

    /// Reuse structural types by shape, resolving valid alias references first.
    /// Nominal kinds always receive a fresh index. Named effect declarations
    /// must use `register`: their operation identity is distinct from a signature.
    /// Invalid references are retained for later validation, never dereferenced.
    pub fn intern_structural(&mut self, kind: TypeKind) -> TypeIndex {
        let kind = self.normalize_structural(kind);
        let structural = matches!(
            kind,
            TypeKind::IterationStepTemplate { .. }
                | TypeKind::Tuple { .. }
                | TypeKind::Function { .. }
                | TypeKind::Effect { .. }
                | TypeKind::Optional { .. }
                | TypeKind::ErrorQualified { .. }
                | TypeKind::EffectQualified { .. }
        );
        if structural {
            let discriminant = std::mem::discriminant(&kind);
            if let Some(index) = self.structural_types.iter().copied().find(|index| {
                let info = &self.types[index.as_u32() as usize];
                std::mem::discriminant(&info.kind) == discriminant
                    && same_structural_shape(&self.normalize_structural(info.kind.clone()), &kind)
            }) {
                return index;
            }
        }
        let index = self.register(TypeInfo {
            kind,
            // Stable identities require package identity, version and layout.
            // Structural interning must not invent a substitute persistent ID.
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        if structural {
            self.structural_types.push(index);
        }
        index
    }

    fn normalize_structural(&self, mut kind: TypeKind) -> TypeKind {
        let step = iteration_step::step_payload(&kind).is_some();
        let normalize = |ty: &mut TypeIndex| {
            *ty = self.canonical_type(*ty).unwrap_or(*ty);
        };
        match &mut kind {
            TypeKind::IterationStepTemplate { item } => normalize(item),
            TypeKind::Enum { variants, .. } if step => normalize(&mut variants[1].fields[0].ty),
            TypeKind::Tuple { elements } => elements.iter_mut().for_each(normalize),
            TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. } => {
                params.iter_mut().for_each(normalize);
                normalize(ret);
            }
            TypeKind::Optional { inner } => normalize(inner),
            TypeKind::ErrorQualified { errors, inner } => {
                errors.iter_mut().for_each(normalize);
                errors.sort_unstable_by_key(|ty| ty.as_u32());
                errors.dedup();
                normalize(inner);
            }
            TypeKind::EffectQualified { effects, inner } => {
                effects.iter_mut().for_each(normalize);
                effects.sort_unstable_by_key(|ty| ty.as_u32());
                effects.dedup();
                normalize(inner);
            }
            _ => {}
        }
        kind
    }

    /// Follow aliases with checked indices and a bound that rejects cycles.
    pub fn canonical_type(&self, mut ty: TypeIndex) -> Option<TypeIndex> {
        for _ in 0..self.types.len() {
            match &self.types.get(ty.as_u32() as usize)?.kind {
                TypeKind::Typealias { target, .. } => ty = *target,
                _ => return Some(ty),
            }
        }
        None
    }

    /// The `?NoReturn` descriptor for null. Returns INVALID on an empty pool
    /// that has not initialized its standard intrinsic prefix.
    pub fn null_type(&self) -> TypeIndex {
        self.null_type
    }

    /// Display a type using its canonical alias target. Invalid indices or
    /// recursive structural descriptors, or nesting beyond 256 levels, return
    /// None. Nominal names must have
    /// been interned by registration; archive loading validates them separately.
    pub fn display_name(&self, ty: TypeIndex) -> Option<String> {
        self.display_name_inner(ty, &mut Vec::new())
    }

    fn display_name_inner(&self, ty: TypeIndex, active: &mut Vec<TypeIndex>) -> Option<String> {
        let ty = self.canonical_type(ty)?;
        if active.len() >= 256 || active.contains(&ty) {
            return None;
        }
        active.push(ty);
        let name = match &self.types.get(ty.as_u32() as usize)?.kind {
            TypeKind::IterationStepTemplate { item } => Some(format!(
                "IterationStep({})",
                self.display_name_inner(*item, active)?
            )),
            TypeKind::Enum { .. } if self.structural_types.contains(&ty) => {
                let item = self.checked_iteration_step_item(ty).ok()??;
                Some(format!(
                    "IterationStep({})",
                    self.display_name_inner(item, active)?
                ))
            }
            TypeKind::Intrinsic(kind) => Some(kind.name().to_owned()),
            TypeKind::Struct { name, .. }
            | TypeKind::Enum { name, .. }
            | TypeKind::Newtype { name, .. }
            | TypeKind::Module { name }
            | TypeKind::Trait { name, .. } => Some(str_interner::get(*name)),
            TypeKind::Tuple { elements } => {
                let names = self.display_names(elements, active)?;
                Some(format!("({})", names.join(", ")))
            }
            TypeKind::Function { params, ret } => {
                let names = self.display_names(params, active)?;
                let result = self.display_name_inner(*ret, active)?;
                Some(format!("fn({}) -> {result}", names.join(", ")))
            }
            TypeKind::Effect {
                params,
                ret,
                is_async,
            } => {
                let names = self.display_names(params, active)?;
                let result = self.display_name_inner(*ret, active)?;
                let prefix = if *is_async { "async effect" } else { "effect" };
                Some(format!("{prefix}({}) -> {result}", names.join(", ")))
            }
            TypeKind::Optional { inner } => {
                Some(format!("?{}", self.display_name_inner(*inner, active)?))
            }
            TypeKind::ErrorQualified { errors, inner } => {
                self.display_qualified("!", errors, *inner, active)
            }
            TypeKind::EffectQualified { effects, inner } => {
                self.display_qualified("#", effects, *inner, active)
            }
            TypeKind::AssociatedType { trait_owner, name } => Some(format!(
                "{}.{}",
                self.display_name_inner(*trait_owner, active)?,
                str_interner::get(*name)
            )),
            TypeKind::Typealias { .. } => None,
        };
        active.pop();
        name
    }

    fn display_names(
        &self,
        types: &[TypeIndex],
        active: &mut Vec<TypeIndex>,
    ) -> Option<Vec<String>> {
        types
            .iter()
            .map(|&ty| self.display_name_inner(ty, active))
            .collect()
    }

    fn display_qualified(
        &self,
        prefix: &str,
        members: &[TypeIndex],
        inner: TypeIndex,
        active: &mut Vec<TypeIndex>,
    ) -> Option<String> {
        let members = self.display_names(members, active)?;
        let inner = self.display_name_inner(inner, active)?;
        if members.len() == 1 {
            Some(format!("{prefix}{} {inner}", members[0]))
        } else {
            Some(format!("{prefix}[{}] {inner}", members.join(", ")))
        }
    }

    fn push(&mut self, info: TypeInfo) -> TypeIndex {
        let idx = TypeIndex(self.types.len() as u32);
        if info.type_id != TypeId::ZERO {
            self.id_to_index.insert(info.type_id, idx);
        }
        self.invalidate_type_identities();
        self.types.push(info);
        self.methods.push(Vec::new());
        idx
    }

    // -- Method table --------------------------------------------------------

    /// Add a method to a type's method table.
    pub fn add_method(&mut self, ty: TypeIndex, slot: MethodSlot) {
        self.methods[ty.as_u32() as usize].push(slot);
    }

    /// Get all methods for a type.
    pub fn methods_of(&self, ty: TypeIndex) -> &[MethodSlot] {
        &self.methods[ty.as_u32() as usize]
    }

    /// Find a method by name on a type.
    pub fn find_method(&self, ty: TypeIndex, name: StrId) -> Option<&MethodSlot> {
        self.methods_of(ty).iter().find(|m| m.name == name)
    }

    // -- Trait impls ---------------------------------------------------------

    /// Record that `implementor` implements `trait_type`.
    pub fn add_trait_impl(&mut self, record: TraitImplRecord) {
        self.trait_impls.push(record);
    }

    /// Find only a global implementation, preserving transparent type aliases.
    pub fn find_trait_impl(&self, ty: TypeIndex, trait_ty: TypeIndex) -> Option<&TraitImplRecord> {
        let ty = self.canonical_type(ty)?;
        let trait_ty = self.canonical_type(trait_ty)?;
        let mut records = self.trait_impls.iter().filter(|record| {
            record.visible_scope.is_none()
                && self.canonical_type(record.implementor) == Some(ty)
                && self.canonical_type(record.trait_type) == Some(trait_ty)
        });
        let result = records.next()?;
        records.next().is_none().then_some(result)
    }

    /// Global trait implementations for a canonical type. Metadata consumers
    /// that need scoped records can use `trait_impls_snapshot` explicitly.
    pub fn trait_impls_of(&self, ty: TypeIndex) -> impl Iterator<Item = &TraitImplRecord> {
        let owner = self.canonical_type(ty);
        self.trait_impls.iter().filter(move |record| {
            record.visible_scope.is_none()
                && owner.is_some()
                && self.canonical_type(record.implementor) == owner
        })
    }

    /// Check if a type has a specific trait implementation.
    pub fn has_trait_impl(&self, ty: TypeIndex, trait_ty: TypeIndex) -> bool {
        self.find_trait_impl(ty, trait_ty).is_some()
    }

    /// Find a method from a trait impl for a given type and method name.
    pub fn find_trait_method(
        &self,
        ty: TypeIndex,
        trait_ty: TypeIndex,
        method_name: StrId,
    ) -> Option<&MethodSlot> {
        self.find_trait_impl(ty, trait_ty)
            .and_then(|r| r.methods.iter().find(|m| m.name == method_name))
    }

    // -- VTable management ---------------------------------------------------

    /// Register a vtable for a specific trait+implementor pair.
    pub fn add_vtable(&mut self, vtable: VTable) {
        self.vtables.push(vtable);
    }

    /// Look up only a global vtable. Scoped extensions require an explicit caller.
    pub fn find_vtable(&self, implementor: TypeIndex, trait_type: TypeIndex) -> Option<&VTable> {
        let implementor = self.canonical_type(implementor)?;
        let trait_type = self.canonical_type(trait_type)?;
        let mut tables = self.vtables.iter().filter(|table| {
            table.visible_scope.is_none()
                && self.canonical_type(table.implementor) == Some(implementor)
                && self.canonical_type(table.trait_type) == Some(trait_type)
        });
        let result = tables.next()?;
        tables.next().is_none().then_some(result)
    }

    /// Snapshot of all trait impl records (for vtable construction).
    pub fn trait_impls_snapshot(&self) -> &[TraitImplRecord] {
        &self.trait_impls
    }

    /// Borrow the complete dispatch table registry without copying descriptors.
    pub fn vtables_snapshot(&self) -> &[VTable] {
        &self.vtables
    }

    // -- Queries -------------------------------------------------------------

    /// Look up type info by index.
    pub fn get(&self, idx: TypeIndex) -> &TypeInfo {
        &self.types[idx.as_u32() as usize]
    }

    /// Look up type info mutably by index.
    pub fn get_mut(&mut self, idx: TypeIndex) -> &mut TypeInfo {
        self.invalidate_type_identities();
        &mut self.types[idx.as_u32() as usize]
    }

    /// Look up a type by its 128-bit TypeId.
    pub fn lookup_by_id(&self, id: TypeId) -> Option<TypeIndex> {
        if id == TypeId::ZERO {
            return None;
        }
        if self.identities_need_validation() && self.validate_type_identities().is_err() {
            return None;
        }
        self.id_to_index.get(&id).copied().filter(|&ty| {
            self.types
                .get(ty.as_u32() as usize)
                .is_some_and(|info| info.type_id == id)
        })
    }

    /// Total number of registered types.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// Helper: get intrinsic type index by enum.
    pub fn intrinsic(&self, intr: Intrinsic) -> TypeIndex {
        debug_assert!((intr as u32) < self.types.len() as u32);
        TypeIndex(intr as u32)
    }

    // -- Type lattice queries ------------------------------------------------

    /// Is `sub` a subtype of `sup` in the type lattice?
    pub fn is_subtype(&self, sub: TypeIndex, sup: TypeIndex) -> bool {
        self.is_subtype_bounded(sub, sup, 256)
    }

    fn is_subtype_bounded(&self, sub: TypeIndex, sup: TypeIndex, remaining: usize) -> bool {
        // The legacy boolean API treats invalid descriptors as unrelated types.
        self.subtype_with_scope(sub, sup, remaining, None)
            .unwrap_or(false)
    }

    /// Are two types gradually consistent (for gradual typing with Any)?
    pub fn is_gradually_consistent(&self, a: TypeIndex, b: TypeIndex) -> bool {
        let (Some(a), Some(b)) = (self.canonical_type(a), self.canonical_type(b)) else {
            return false;
        };
        if a == b {
            return true;
        }
        let any_idx = Intrinsic::Any.type_index();
        if a == any_idx || b == any_idx {
            return true;
        }
        let a_info = self.get(a);
        let b_info = self.get(b);
        match (&a_info.kind, &b_info.kind) {
            (
                TypeKind::ErrorQualified {
                    errors: ae,
                    inner: ai,
                },
                TypeKind::ErrorQualified {
                    errors: be,
                    inner: bi,
                },
            ) if ae.contains(&any_idx) || be.contains(&any_idx) => {
                *ai == Intrinsic::NoReturn.type_index()
                    || *bi == Intrinsic::NoReturn.type_index()
                    || self.is_gradually_consistent(*ai, *bi)
                    || self.is_subtype(*ai, *bi)
            }
            (TypeKind::Optional { inner: ai }, TypeKind::Optional { inner: bi }) => {
                self.is_gradually_consistent(*ai, *bi)
            }
            (TypeKind::Tuple { elements: ae }, TypeKind::Tuple { elements: be }) => {
                ae.len() == be.len()
                    && ae
                        .iter()
                        .zip(be.iter())
                        .all(|(a, b)| self.is_gradually_consistent(*a, *b))
            }
            (
                TypeKind::Function {
                    params: ap,
                    ret: ar,
                },
                TypeKind::Function {
                    params: bp,
                    ret: br,
                },
            ) => {
                ap.len() == bp.len()
                    && ap
                        .iter()
                        .zip(bp.iter())
                        .all(|(a, b)| self.is_gradually_consistent(*a, *b))
                    && self.is_gradually_consistent(*ar, *br)
            }
            _ => false,
        }
    }

    // -- Numeric lifting queries ---------------------------------------------

    /// Determine the wider numeric type when two numeric types meet in a
    /// binary expression. Returns None for non-numeric operands or integer
    /// ranges that have no common fixed-width integer type (u128 with signed).
    pub fn numeric_lift(&self, a: TypeIndex, b: TypeIndex) -> Option<TypeIndex> {
        let a_intr = self.as_intrinsic(a)?;
        let b_intr = self.as_intrinsic(b)?;
        numeric::lift(a_intr, b_intr).map(Intrinsic::type_index)
    }

    /// If the type at `idx` is an intrinsic, return which one.
    pub fn as_intrinsic(&self, idx: TypeIndex) -> Option<Intrinsic> {
        match &self.get(idx).kind {
            TypeKind::Intrinsic(i) => Some(*i),
            TypeKind::Typealias { target, .. } => self.as_intrinsic(*target),
            _ => None,
        }
    }

    /// Resolve through typealias chains to the underlying type.
    pub fn resolve_alias(&self, idx: TypeIndex) -> TypeIndex {
        let mut cur = idx;
        loop {
            match &self.get(cur).kind {
                TypeKind::Typealias { target, .. } => cur = *target,
                _ => return cur,
            }
        }
    }
}

/// Nominal type definitions never compare equal through this shape predicate.
fn same_structural_shape(left: &TypeKind, right: &TypeKind) -> bool {
    match (left, right) {
        (
            TypeKind::IterationStepTemplate { item: left },
            TypeKind::IterationStepTemplate { item: right },
        ) => left == right,
        (TypeKind::Tuple { elements: left }, TypeKind::Tuple { elements: right }) => left == right,
        (
            TypeKind::Function {
                params: left,
                ret: left_ret,
            },
            TypeKind::Function {
                params: right,
                ret: right_ret,
            },
        ) => left == right && left_ret == right_ret,
        (
            TypeKind::Effect {
                params: left,
                ret: left_ret,
                is_async: left_async,
            },
            TypeKind::Effect {
                params: right,
                ret: right_ret,
                is_async: right_async,
            },
        ) => left == right && left_ret == right_ret && left_async == right_async,
        (TypeKind::Optional { inner: left }, TypeKind::Optional { inner: right }) => left == right,
        (
            TypeKind::ErrorQualified {
                errors: left,
                inner: left_inner,
            },
            TypeKind::ErrorQualified {
                errors: right,
                inner: right_inner,
            },
        ) => left == right && left_inner == right_inner,
        (
            TypeKind::EffectQualified {
                effects: left,
                inner: left_inner,
            },
            TypeKind::EffectQualified {
                effects: right,
                inner: right_inner,
            },
        ) => left == right && left_inner == right_inner,
        _ => false,
    }
}

impl Default for TypePool {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Intrinsic helpers
// ---------------------------------------------------------------------------

const fn intrinsic_layout(intr: Intrinsic) -> (u32, u32) {
    match intr {
        Intrinsic::U8 | Intrinsic::I8 | Intrinsic::Bool => (1, 1),
        Intrinsic::Char => (4, 4), // Unicode code point
        Intrinsic::U16 | Intrinsic::I16 => (2, 2),
        Intrinsic::U32 | Intrinsic::I32 | Intrinsic::F32 => (4, 4),
        Intrinsic::U64 | Intrinsic::I64 | Intrinsic::F64 | Intrinsic::Usize | Intrinsic::Isize => {
            (8, 8)
        }
        Intrinsic::U128 | Intrinsic::I128 => (16, 16),
        Intrinsic::Str => (0, 8),
        Intrinsic::Unit => (0, 1),
        Intrinsic::Any => (0, 8),
        Intrinsic::NoReturn => (0, 1),
        Intrinsic::Type => (0, 8),
        Intrinsic::Closure | Intrinsic::Continuation => (0, 8), // runtime-managed heap objects
    }
}

const fn intrinsic_type_id(intr: Intrinsic) -> TypeId {
    // Deterministic well-known IDs: hi = magic, lo = discriminant
    TypeId(0x4E45_5353_4149_4E00, intr as u64)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intrinsics_populated() {
        let pool = TypePool::with_intrinsics();
        // Intrinsics, well-known traits, null, and two collection role pairs.
        assert_eq!(
            pool.len(),
            Intrinsic::COUNT + WellKnownTraits::NAMES.len() + 5
        );
        assert_eq!(
            pool.intrinsic(Intrinsic::U32).as_u32(),
            Intrinsic::U32 as u32
        );
    }

    #[test]
    fn subtype_any_noreturn() {
        let pool = TypePool::with_intrinsics();
        let any = Intrinsic::Any.type_index();
        let noret = Intrinsic::NoReturn.type_index();
        let u32_t = Intrinsic::U32.type_index();

        assert!(pool.is_subtype(u32_t, any));
        assert!(pool.is_subtype(noret, u32_t));
        assert!(pool.is_subtype(noret, any));
        assert!(!pool.is_subtype(any, u32_t));
    }

    #[test]
    fn gradual_consistency() {
        let pool = TypePool::with_intrinsics();
        let any = Intrinsic::Any.type_index();
        let u32_t = Intrinsic::U32.type_index();
        let i32_t = Intrinsic::I32.type_index();

        assert!(pool.is_gradually_consistent(u32_t, any));
        assert!(pool.is_gradually_consistent(any, i32_t));
        assert!(!pool.is_gradually_consistent(u32_t, i32_t));
    }

    #[test]
    fn numeric_lift_int_float() {
        let pool = TypePool::with_intrinsics();
        let i32_t = Intrinsic::I32.type_index();
        let f64_t = Intrinsic::F64.type_index();
        let result = pool.numeric_lift(i32_t, f64_t).unwrap();
        assert_eq!(result, f64_t);
    }

    #[test]
    fn register_struct() {
        let mut pool = TypePool::with_intrinsics();
        let name = str_interner::intern("Point");
        let info = TypeInfo {
            kind: TypeKind::Struct {
                name,
                fields: vec![
                    FieldInfo {
                        name: str_interner::intern("x"),
                        ty: Intrinsic::F64.type_index(),
                        has_default: false,
                        offset: 0,
                    },
                    FieldInfo {
                        name: str_interner::intern("y"),
                        ty: Intrinsic::F64.type_index(),
                        has_default: false,
                        offset: 8,
                    },
                ],
            },
            type_id: TypeId(1, 1),
            size: 16,
            align: 8,
        };
        let idx = pool.register(info);
        assert_eq!(
            idx.as_u32(),
            (Intrinsic::COUNT + WellKnownTraits::NAMES.len() + 5) as u32
        );
        assert!(matches!(pool.get(idx).kind, TypeKind::Struct { .. }));
    }

    fn alias(pool: &mut TypePool, name: &str, target: TypeIndex) -> TypeIndex {
        pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern(name),
                target,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        })
    }

    #[test]
    fn optional_subtyping_lifts_inner_and_null_with_transparent_aliases() {
        let mut pool = TypePool::with_intrinsics();
        let integer = Intrinsic::I64.type_index();
        let inner_alias = alias(&mut pool, "Count", integer);
        let optional = pool.intern_structural(TypeKind::Optional { inner: inner_alias });
        let optional_alias = alias(&mut pool, "MaybeCount", optional);
        assert!(pool.is_subtype(integer, optional_alias));
        assert!(pool.is_subtype(inner_alias, optional_alias));
        assert!(pool.is_subtype(pool.null_type(), optional_alias));
        assert!(!pool.is_subtype(optional_alias, integer));
        assert!(!pool.is_subtype(Intrinsic::Bool.type_index(), optional_alias));
        let narrow = pool.intern_structural(TypeKind::Optional {
            inner: Intrinsic::I32.type_index(),
        });
        assert!(pool.is_subtype(narrow, optional_alias));
        assert!(!pool.is_subtype(optional_alias, narrow));
        let cycle = pool.intern_structural(TypeKind::Optional {
            inner: Intrinsic::Bool.type_index(),
        });
        pool.get_mut(cycle).kind = TypeKind::Optional { inner: cycle };
        assert!(!pool.is_subtype(integer, cycle));
    }

    #[test]
    fn structural_types_share_alias_canonical_shapes() {
        let mut pool = TypePool::with_intrinsics();
        let integer = Intrinsic::I64.type_index();
        let first_alias = alias(&mut pool, "Count", integer);
        let second_alias = alias(&mut pool, "Total", first_alias);
        let optional = pool.intern_structural(TypeKind::Optional { inner: integer });
        assert_eq!(
            optional,
            pool.intern_structural(TypeKind::Optional {
                inner: second_alias
            })
        );
        assert_eq!(pool.canonical_type(second_alias), Some(integer));
        assert_eq!(pool.display_name(second_alias).as_deref(), Some("i64"));
        assert_eq!(pool.display_name(optional).as_deref(), Some("?i64"));
        let function = pool.intern_structural(TypeKind::Function {
            params: vec![integer, optional],
            ret: integer,
        });
        assert_eq!(
            function,
            pool.intern_structural(TypeKind::Function {
                params: vec![second_alias, optional],
                ret: first_alias,
            })
        );
        assert_eq!(
            pool.display_name(function).as_deref(),
            Some("fn(i64, ?i64) -> i64")
        );
        let tuple = pool.intern_structural(TypeKind::Tuple {
            elements: vec![first_alias, optional],
        });
        assert_eq!(
            tuple,
            pool.intern_structural(TypeKind::Tuple {
                elements: vec![integer, optional]
            })
        );
        assert_eq!(pool.display_name(tuple).as_deref(), Some("(i64, ?i64)"));
    }

    #[test]
    fn qualified_sets_ignore_order_duplicates_and_aliases() {
        let mut pool = TypePool::with_intrinsics();
        let integer = Intrinsic::I64.type_index();
        let boolean = Intrinsic::Bool.type_index();
        let integer_alias = alias(&mut pool, "Value", integer);
        let first = pool.intern_structural(TypeKind::ErrorQualified {
            errors: vec![boolean, integer_alias, boolean],
            inner: integer_alias,
        });
        let second = pool.intern_structural(TypeKind::ErrorQualified {
            errors: vec![integer, boolean],
            inner: integer,
        });
        assert_eq!(first, second);
        assert_eq!(
            pool.display_name(first).as_deref(),
            Some("![i64, bool] i64")
        );
        let first = pool.intern_structural(TypeKind::EffectQualified {
            effects: vec![boolean, integer_alias, boolean],
            inner: integer_alias,
        });
        let second = pool.intern_structural(TypeKind::EffectQualified {
            effects: vec![integer, boolean],
            inner: integer,
        });
        assert_eq!(first, second);
        assert_eq!(
            pool.display_name(first).as_deref(),
            Some("#[i64, bool] i64")
        );
    }

    #[test]
    fn effect_signatures_reuse_shapes_but_keep_async_distinct() {
        let mut pool = TypePool::with_intrinsics();
        let integer = Intrinsic::I64.type_index();
        let signature = TypeKind::Effect {
            params: vec![integer],
            ret: integer,
            is_async: false,
        };
        let first = pool.intern_structural(signature.clone());
        assert_eq!(first, pool.intern_structural(signature));
        let asynchronous = pool.intern_structural(TypeKind::Effect {
            params: vec![integer],
            ret: integer,
            is_async: true,
        });
        assert_ne!(first, asynchronous);
        assert_eq!(
            pool.display_name(asynchronous).as_deref(),
            Some("async effect(i64) -> i64")
        );
    }

    #[test]
    fn effect_signature_interning_does_not_reuse_a_named_operation() {
        let mut pool = TypePool::with_intrinsics();
        let kind = TypeKind::Effect {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::Unit.type_index(),
            is_async: false,
        };
        let operation = pool.register(TypeInfo {
            kind: kind.clone(),
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let signature = pool.intern_structural(kind.clone());
        assert_ne!(operation, signature);
        assert_eq!(signature, pool.intern_structural(kind));
    }

    #[test]
    fn nominal_types_are_distinct_even_with_identical_names_and_layouts() {
        let mut pool = TypePool::with_intrinsics();
        let kind = TypeKind::Struct {
            name: str_interner::intern("Point"),
            fields: vec![],
        };
        let first = pool.intern_structural(kind.clone());
        let second = pool.intern_structural(kind);
        assert_ne!(first, second);
        assert_eq!(pool.display_name(first).as_deref(), Some("Point"));
        let kind = TypeKind::Newtype {
            name: str_interner::intern("Count"),
            inner: Intrinsic::I64.type_index(),
        };
        let first = pool.intern_structural(kind.clone());
        let second = pool.intern_structural(kind);
        assert_ne!(first, second);
        assert_eq!(pool.canonical_type(first), Some(first));
    }

    #[test]
    fn null_type_is_a_stable_optional_noreturn_descriptor() {
        let mut pool = TypePool::with_intrinsics();
        let null = pool.null_type();
        assert_eq!(
            null,
            pool.intern_structural(TypeKind::Optional {
                inner: Intrinsic::NoReturn.type_index()
            })
        );
        assert_eq!(pool.display_name(null).as_deref(), Some("?NoReturn"));
        assert_ne!(null, Intrinsic::Unit.type_index());
        assert_eq!(TypePool::new().null_type(), TypeIndex::INVALID);
    }

    #[test]
    fn invalid_references_and_cycles_are_checked_without_panicking() {
        let mut pool = TypePool::with_intrinsics();
        assert_eq!(pool.canonical_type(TypeIndex::INVALID), None);
        assert_eq!(pool.display_name(TypeIndex::INVALID), None);
        let invalid = pool.intern_structural(TypeKind::Optional {
            inner: TypeIndex::INVALID,
        });
        assert!(matches!(
            pool.get(invalid).kind,
            TypeKind::Optional {
                inner: TypeIndex::INVALID
            }
        ));
        assert_eq!(pool.display_name(invalid), None);
        let cycle_index = TypeIndex::from_raw(pool.len() as u32);
        let cycle = alias(&mut pool, "Cycle", cycle_index);
        assert_eq!(pool.canonical_type(cycle), None);
        assert_eq!(pool.display_name(cycle), None);
        let nested = pool.intern_structural(TypeKind::Tuple {
            elements: vec![cycle],
        });
        assert_eq!(pool.display_name(nested), None);
        let recursive = TypeIndex::from_raw(pool.len() as u32);
        pool.register(TypeInfo {
            kind: TypeKind::Tuple {
                elements: vec![recursive],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        assert_eq!(pool.display_name(recursive), None);
    }
}
