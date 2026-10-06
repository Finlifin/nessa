use str_interner::StrId;

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
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
}

impl Intrinsic {
    pub const COUNT: usize = Self::Closure as usize + 1;

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
    /// TypeId → TypeIndex reverse lookup.
    id_to_index: std::collections::HashMap<TypeId, TypeIndex>,
    /// Per-type method tables.
    methods: Vec<Vec<MethodSlot>>,
    /// Trait implementation records.
    trait_impls: Vec<TraitImplRecord>,
    /// Virtual method tables for trait dynamic dispatch.
    vtables: Vec<VTable>,
    /// Well-known trait type indices.
    pub well_known: WellKnownTraits,
}

impl TypePool {
    /// Create a new, empty pool.
    pub fn new() -> Self {
        Self {
            types: Vec::new(),
            id_to_index: std::collections::HashMap::new(),
            methods: Vec::new(),
            trait_impls: Vec::new(),
            vtables: Vec::new(),
            well_known: WellKnownTraits::UNINITIALIZED,
        }
    }

    /// Create a pool pre-populated with all intrinsic types.
    pub fn with_intrinsics() -> Self {
        let mut pool = Self::new();
        pool.register_intrinsics();
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

        let mk = |pool: &mut Self, name: StrId| -> TypeIndex {
            pool.push(TypeInfo {
                kind: TypeKind::Trait {
                    name,
                    parents: Vec::new(),
                    assoc_types: Vec::new(),
                },
                type_id: TypeId(0x4E45_5353_5452_4954, name.as_u32() as u64),
                size: 0,
                align: 0,
            })
        };

        self.well_known = WellKnownTraits {
            display: mk(self, display_name),
            hash: mk(self, hash_name),
            eq: mk(self, eq_name),
            ord: mk(self, ord_name),
            partial_eq: mk(self, partial_eq_name),
            partial_ord: mk(self, partial_ord_name),
            iterator: mk(self, iterator_name),
            into_iterator: mk(self, into_iterator_name),
        };
    }

    /// Register a new type, returning its [`TypeIndex`].
    pub fn register(&mut self, info: TypeInfo) -> TypeIndex {
        self.push(info)
    }

    fn push(&mut self, info: TypeInfo) -> TypeIndex {
        let idx = TypeIndex(self.types.len() as u32);
        self.id_to_index.insert(info.type_id, idx);
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

    /// Find the impl record for `ty` implementing `trait_ty`.
    pub fn find_trait_impl(&self, ty: TypeIndex, trait_ty: TypeIndex) -> Option<&TraitImplRecord> {
        self.trait_impls
            .iter()
            .find(|r| r.implementor == ty && r.trait_type == trait_ty)
    }

    /// All trait impls for a given type.
    pub fn trait_impls_of(&self, ty: TypeIndex) -> impl Iterator<Item = &TraitImplRecord> {
        self.trait_impls.iter().filter(move |r| r.implementor == ty)
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

    /// Look up the vtable for a concrete type implementing a trait.
    pub fn find_vtable(&self, implementor: TypeIndex, trait_type: TypeIndex) -> Option<&VTable> {
        self.vtables
            .iter()
            .find(|v| v.implementor == implementor && v.trait_type == trait_type)
    }

    /// Snapshot of all trait impl records (for vtable construction).
    pub fn trait_impls_snapshot(&self) -> &[TraitImplRecord] {
        &self.trait_impls
    }

    // -- Queries -------------------------------------------------------------

    /// Look up type info by index.
    pub fn get(&self, idx: TypeIndex) -> &TypeInfo {
        &self.types[idx.as_u32() as usize]
    }

    /// Look up type info mutably by index.
    pub fn get_mut(&mut self, idx: TypeIndex) -> &mut TypeInfo {
        &mut self.types[idx.as_u32() as usize]
    }

    /// Look up a type by its 128-bit TypeId.
    pub fn lookup_by_id(&self, id: TypeId) -> Option<TypeIndex> {
        self.id_to_index.get(&id).copied()
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
        if sub == sup {
            return true;
        }
        if sup == Intrinsic::Any.type_index() {
            return true;
        }
        if sub == Intrinsic::NoReturn.type_index() {
            return true;
        }
        if let TypeKind::Typealias { target, .. } = &self.get(sub).kind {
            return self.is_subtype(*target, sup);
        }
        if let TypeKind::Typealias { target, .. } = &self.get(sup).kind {
            return self.is_subtype(sub, *target);
        }
        // Numeric widening: narrower integer/float promotes to wider.
        if let Some(lifted) = self.numeric_lift(sub, sup) {
            if lifted == sup {
                return true;
            }
        }
        // ErrorQualified: !E T — both T and !E T are subtypes of !E T.
        if let TypeKind::ErrorQualified { errors, inner } = &self.get(sup).kind {
            let inner = *inner;
            let errors = errors.clone();
            // The inner (success) type is a subtype of !E T.
            if self.is_subtype(sub, inner) {
                return true;
            }
            // Another !E' T' is a subtype if inner matches and errors are a subset.
            if let TypeKind::ErrorQualified {
                errors: sub_errors,
                inner: sub_inner,
            } = &self.get(sub).kind
            {
                let sub_inner = *sub_inner;
                let sub_errors = sub_errors.clone();
                if self.is_subtype(sub_inner, inner)
                    && sub_errors
                        .iter()
                        .all(|se| errors.iter().any(|e| self.is_subtype(*se, *e)))
                {
                    return true;
                }
            }
        }
        // Trait subtyping: a concrete type is a subtype of a trait type
        // if it implements that trait.  Also handles trait inheritance:
        // trait Bird(Animal) → Bird is a subtype of Animal.
        if matches!(self.get(sup).kind, TypeKind::Trait { .. }) {
            // Concrete type implements the trait.
            if self.has_trait_impl(sub, sup) {
                return true;
            }
            // Sub-trait: check if sub is a trait whose parents include sup.
            if let TypeKind::Trait { parents, .. } = &self.get(sub).kind {
                let parents = parents.clone();
                for parent in &parents {
                    if self.is_subtype(*parent, sup) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Are two types gradually consistent (for gradual typing with Any)?
    pub fn is_gradually_consistent(&self, a: TypeIndex, b: TypeIndex) -> bool {
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
    /// binary expression.  Returns `None` if either is non-numeric.
    pub fn numeric_lift(&self, a: TypeIndex, b: TypeIndex) -> Option<TypeIndex> {
        let a_intr = self.as_intrinsic(a)?;
        let b_intr = self.as_intrinsic(b)?;
        if !a_intr.is_numeric() || !b_intr.is_numeric() {
            return None;
        }
        let a_is_float = matches!(a_intr, Intrinsic::F32 | Intrinsic::F64);
        let b_is_float = matches!(b_intr, Intrinsic::F32 | Intrinsic::F64);
        if a_is_float || b_is_float {
            if a_intr == Intrinsic::F64 || b_intr == Intrinsic::F64 {
                return Some(Intrinsic::F64.type_index());
            }
            return Some(Intrinsic::F32.type_index());
        }
        if (a_intr as u8) >= (b_intr as u8) {
            Some(a)
        } else {
            Some(b)
        }
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
        Intrinsic::Closure => (0, 8), // variable-size heap object, pointer-aligned
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
        // Intrinsics + 8 well-known traits
        assert_eq!(pool.len(), Intrinsic::COUNT + WellKnownTraits::NAMES.len());
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
            (Intrinsic::COUNT + WellKnownTraits::NAMES.len()) as u32
        );
        assert!(matches!(pool.get(idx).kind, TypeKind::Struct { .. }));
    }
}
