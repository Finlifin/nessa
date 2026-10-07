//! Consume nominal Ordering metadata from the signed interface result.

use type_pool::{TypeIndex, TypeKind, TypePool};

#[derive(Clone, Copy)]
pub(crate) struct OrderingLayout {
    pub ty: TypeIndex,
    pub less: u32,
    pub equal: u32,
    pub greater: u32,
}

impl OrderingLayout {
    pub(crate) fn from_result(pool: &TypePool, result: TypeIndex) -> Option<Self> {
        let mut ty = pool.canonical_type(result)?;
        if let TypeKind::Optional { inner } = pool.get(ty).kind {
            ty = pool.canonical_type(inner)?;
        }
        let TypeKind::Enum { variants, .. } = &pool.get(ty).kind else {
            return None;
        };
        if variants.len() != 3 || variants.iter().any(|variant| !variant.fields.is_empty()) {
            return None;
        }
        let tag = |name| {
            variants
                .iter()
                .find(|variant| str_interner::get(variant.name) == name)
                .map(|variant| variant.tag)
        };
        Some(Self {
            ty,
            less: tag("less")?,
            equal: tag("equal")?,
            greater: tag("greater")?,
        })
    }
}
