//! Builtin catalog — shared name→id metadata for compiler and runtime.
//!
//! Function pointers live on the VM (`interpreter`); this module owns stable
//! IDs, names, and the compile-time lookup table used by `'builtin` views.

use std::collections::HashMap;

use type_pool::TypeIndex;

/// Stable identifier for a builtin function (encoded in `CallBuiltin`).
pub type BuiltinFnId = u32;

/// Version of the stable builtin IDs and calling semantics stored in artifacts.
/// Increment when an ID changes meaning or compilation requires new native capabilities.
pub const BUILTIN_ABI_VERSION: u32 = 8;

/// What a `'builtin` view resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinKind {
    Fn(BuiltinFnId),
    Type(TypeIndex),
    /// Reserved for future effect builtins.
    #[allow(dead_code)]
    Effect(u32),
}

// ---------------------------------------------------------------------------
// Stable builtin function IDs (sparse ranges by domain)
// ---------------------------------------------------------------------------

pub mod ids {
    use super::BuiltinFnId;

    // ── I/O ────────────────────────────────────────────────────────
    pub const PRINT: BuiltinFnId = 0;
    pub const PRINTLN: BuiltinFnId = 1;

    // ── Convert / introspection ────────────────────────────────────
    pub const TYPE_OF: BuiltinFnId = 2;
    pub const TO_I64: BuiltinFnId = 3;
    pub const TO_F64: BuiltinFnId = 4;
    pub const TO_STRING: BuiltinFnId = 5;

    // ── Math ───────────────────────────────────────────────────────
    pub const ABS: BuiltinFnId = 10;
    pub const SIN: BuiltinFnId = 11;
    pub const COS: BuiltinFnId = 12;
    pub const SQRT: BuiltinFnId = 13;
    pub const FLOOR: BuiltinFnId = 14;
    pub const CEIL: BuiltinFnId = 15;
    pub const ROUND: BuiltinFnId = 16;
    pub const POW: BuiltinFnId = 17;
    pub const LOG: BuiltinFnId = 18;

    // ── String ─────────────────────────────────────────────────────
    pub const STR_LEN: BuiltinFnId = 30;
    pub const STR_CONCAT: BuiltinFnId = 31;

    // ── Process ────────────────────────────────────────────────────
    pub const EXIT: BuiltinFnId = 50;
    pub const PANIC: BuiltinFnId = 51;

    // ── List ───────────────────────────────────────────────────────
    pub const LIST_INIT: BuiltinFnId = 100;
    pub const LIST_LEN: BuiltinFnId = 101;
    pub const LIST_GET: BuiltinFnId = 102;
    pub const LIST_SET: BuiltinFnId = 103;
    pub const LIST_PUSH: BuiltinFnId = 104;
    pub const LIST_POP: BuiltinFnId = 105;

    // ── String-keyed Map ────────────────────────────────────────────
    pub const MAP_INIT: BuiltinFnId = 110;
    pub const MAP_LEN: BuiltinFnId = 111;
    pub const MAP_GET: BuiltinFnId = 112;
    pub const MAP_SET: BuiltinFnId = 113;
    pub const MAP_REMOVE: BuiltinFnId = 114;
    pub const MAP_CONTAINS: BuiltinFnId = 115;
    /// Appended after ABI7 capabilities; earlier IDs retain their meanings.
    pub const MAP_KEYS: BuiltinFnId = 126;

    // Compiler-generated Display adapter; retains the legacy field formatter.
    pub const DERIVED_DISPLAY: BuiltinFnId = 120;
    /// Scalar equality used by the standard library trait implementations.
    pub const SCALAR_EQ: BuiltinFnId = 121;
    /// Partial scalar comparison; std maps its optional sign to Ordering.
    pub const SCALAR_CMP: BuiltinFnId = 122;
    pub const DISPLAY_QUOTE: BuiltinFnId = 123;
    pub const DISPLAY_ENTER_TUPLE: BuiltinFnId = 124;
    pub const DISPLAY_EXIT_TUPLE: BuiltinFnId = 125;
}

/// Static metadata for a builtin function (no function pointer).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinFnMeta {
    pub id: BuiltinFnId,
    pub name: &'static str,
}

/// All builtin functions known to the compiler and runtime.
pub const BUILTIN_FN_META: &[BuiltinFnMeta] = &[
    BuiltinFnMeta {
        id: ids::DISPLAY_QUOTE,
        name: "__display_quote",
    },
    BuiltinFnMeta {
        id: ids::DISPLAY_ENTER_TUPLE,
        name: "__display_enter_tuple",
    },
    BuiltinFnMeta {
        id: ids::DISPLAY_EXIT_TUPLE,
        name: "__display_exit_tuple",
    },
    BuiltinFnMeta {
        id: ids::SCALAR_CMP,
        name: "__scalar_cmp",
    },
    BuiltinFnMeta {
        id: ids::SCALAR_EQ,
        name: "__scalar_eq",
    },
    BuiltinFnMeta {
        id: ids::DERIVED_DISPLAY,
        name: "__derived_display",
    },
    BuiltinFnMeta {
        id: ids::PRINT,
        name: "print",
    },
    BuiltinFnMeta {
        id: ids::PRINTLN,
        name: "println",
    },
    BuiltinFnMeta {
        id: ids::TYPE_OF,
        name: "type_of",
    },
    BuiltinFnMeta {
        id: ids::TO_I64,
        name: "to_i64",
    },
    BuiltinFnMeta {
        id: ids::TO_F64,
        name: "to_f64",
    },
    BuiltinFnMeta {
        id: ids::TO_STRING,
        name: "to_string",
    },
    BuiltinFnMeta {
        id: ids::ABS,
        name: "abs",
    },
    BuiltinFnMeta {
        id: ids::SIN,
        name: "sin",
    },
    BuiltinFnMeta {
        id: ids::COS,
        name: "cos",
    },
    BuiltinFnMeta {
        id: ids::SQRT,
        name: "sqrt",
    },
    BuiltinFnMeta {
        id: ids::FLOOR,
        name: "floor",
    },
    BuiltinFnMeta {
        id: ids::CEIL,
        name: "ceil",
    },
    BuiltinFnMeta {
        id: ids::ROUND,
        name: "round",
    },
    BuiltinFnMeta {
        id: ids::POW,
        name: "pow",
    },
    BuiltinFnMeta {
        id: ids::LOG,
        name: "log",
    },
    BuiltinFnMeta {
        id: ids::STR_LEN,
        name: "str_len",
    },
    BuiltinFnMeta {
        id: ids::STR_CONCAT,
        name: "str_concat",
    },
    BuiltinFnMeta {
        id: ids::EXIT,
        name: "exit",
    },
    BuiltinFnMeta {
        id: ids::PANIC,
        name: "panic",
    },
    BuiltinFnMeta {
        id: ids::LIST_INIT,
        name: "__list_init",
    },
    BuiltinFnMeta {
        id: ids::LIST_LEN,
        name: "__list_len",
    },
    BuiltinFnMeta {
        id: ids::LIST_GET,
        name: "__list_get",
    },
    BuiltinFnMeta {
        id: ids::LIST_SET,
        name: "__list_set",
    },
    BuiltinFnMeta {
        id: ids::LIST_PUSH,
        name: "__list_push",
    },
    BuiltinFnMeta {
        id: ids::LIST_POP,
        name: "__list_pop",
    },
    BuiltinFnMeta {
        id: ids::MAP_INIT,
        name: "__map_init",
    },
    BuiltinFnMeta {
        id: ids::MAP_LEN,
        name: "__map_len",
    },
    BuiltinFnMeta {
        id: ids::MAP_GET,
        name: "__map_get",
    },
    BuiltinFnMeta {
        id: ids::MAP_SET,
        name: "__map_set",
    },
    BuiltinFnMeta {
        id: ids::MAP_REMOVE,
        name: "__map_remove",
    },
    BuiltinFnMeta {
        id: ids::MAP_CONTAINS,
        name: "__map_contains",
    },
    BuiltinFnMeta {
        id: ids::MAP_KEYS,
        name: "__map_keys",
    },
];

/// Look up a builtin function id by its language-facing name.
pub fn lookup_builtin_fn_id(name: &str) -> Option<BuiltinFnId> {
    BUILTIN_FN_META
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.id)
}

/// Look up builtin function metadata by id.
pub fn lookup_builtin_fn_meta(id: BuiltinFnId) -> Option<&'static BuiltinFnMeta> {
    BUILTIN_FN_META.iter().find(|m| m.id == id)
}

// ---------------------------------------------------------------------------
// BuiltinCatalog — runtime/compiler shared name → kind table
// ---------------------------------------------------------------------------

/// Shared builtin catalog: name → [`BuiltinKind`].
///
/// Function entries are filled from [`BUILTIN_FN_META`] at construction.
/// Type entries are registered from the TypePool during engine init.
#[derive(Debug, Clone, Default)]
pub struct BuiltinCatalog {
    by_name: HashMap<String, BuiltinKind>,
}

impl BuiltinCatalog {
    /// Catalog with all known builtin functions; types empty until registered.
    pub fn with_fns() -> Self {
        let mut cat = Self::default();
        for meta in BUILTIN_FN_META {
            cat.by_name
                .insert(meta.name.to_string(), BuiltinKind::Fn(meta.id));
        }
        cat
    }

    pub fn register_fn(&mut self, id: BuiltinFnId, name: &str) {
        self.by_name.insert(name.to_string(), BuiltinKind::Fn(id));
    }

    pub fn register_type(&mut self, name: &str, type_index: TypeIndex) {
        self.by_name
            .insert(name.to_string(), BuiltinKind::Type(type_index));
    }

    pub fn lookup(&self, name: &str) -> Option<BuiltinKind> {
        self.by_name.get(name).copied()
    }

    pub fn lookup_fn(&self, name: &str) -> Option<BuiltinFnId> {
        match self.lookup(name)? {
            BuiltinKind::Fn(id) => Some(id),
            _ => None,
        }
    }

    pub fn lookup_type(&self, name: &str) -> Option<TypeIndex> {
        match self.lookup(name)? {
            BuiltinKind::Type(idx) => Some(idx),
            _ => None,
        }
    }
}

/// Global process-wide catalog for the compiler (name → kind).
///
/// Initialized once with function metadata; types filled when the engine
/// (or test harness) registers intrinsic TypeIndexes.
use std::sync::{OnceLock, RwLock};

static GLOBAL_CATALOG: OnceLock<RwLock<BuiltinCatalog>> = OnceLock::new();

fn global_catalog() -> &'static RwLock<BuiltinCatalog> {
    GLOBAL_CATALOG.get_or_init(|| RwLock::new(BuiltinCatalog::with_fns()))
}

/// Snapshot lookup used by the resolver (functions always available).
pub fn catalog_lookup(name: &str) -> Option<BuiltinKind> {
    // Prefer static fn table so resolution works before engine init.
    if let Some(id) = lookup_builtin_fn_id(name) {
        return Some(BuiltinKind::Fn(id));
    }
    global_catalog().read().ok().and_then(|g| g.lookup(name))
}

/// Register a builtin type name into the global catalog (engine init).
pub fn catalog_register_type(name: &str, type_index: TypeIndex) {
    if let Ok(mut g) = global_catalog().write() {
        g.register_type(name, type_index);
    }
}

/// Ensure intrinsic type names are present in the global catalog.
pub fn catalog_register_intrinsic_types(type_pool: &type_pool::TypePool) {
    use type_pool::Intrinsic;
    for &intr in Intrinsic::ALL {
        if matches!(intr, Intrinsic::Closure) {
            continue;
        }
        let idx = type_pool.intrinsic(intr);
        catalog_register_type(intr.name(), idx);
    }
    if let Some(list) = type_pool.list_type() {
        catalog_register_type("List", list);
    }
    if let Some(map) = type_pool.map_type() {
        catalog_register_type("Map", map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_ids_are_stable_and_named() {
        assert_eq!(lookup_builtin_fn_id("print"), Some(ids::PRINT));
        assert_eq!(lookup_builtin_fn_id("str_concat"), Some(ids::STR_CONCAT));
        assert_eq!(lookup_builtin_fn_id("nope"), None);
        assert_eq!(
            lookup_builtin_fn_meta(ids::LIST_INIT).unwrap().name,
            "__list_init"
        );
    }

    #[test]
    fn catalog_with_fns() {
        let cat = BuiltinCatalog::with_fns();
        assert_eq!(cat.lookup_fn("println"), Some(ids::PRINTLN));
        assert!(cat.lookup_type("i64").is_none());
    }
}
