//! Validate source initialization functions after their signatures are resolved.

use ast::Ast;
use type_pool::{Intrinsic, TypeKind};

use crate::SymbolKind;
use crate::resolver::Resolver;

pub(crate) fn validate(r: &Resolver<'_>, ast: &Ast) {
    for symbol in &r.symbols {
        if symbol.kind != SymbolKind::Function || str_interner::get(symbol.name) != "__init__" {
            continue;
        }
        let Some(signature) = r.type_pool.canonical_type(symbol.type_index) else {
            continue;
        };
        if let TypeKind::Function { ret, .. } = r.type_pool.get(signature).kind
            && r.type_pool.as_intrinsic(ret) != Some(Intrinsic::Unit)
        {
            r.diag_ctx
                .error("__init__ must return Unit".into())
                .with_primary_span(ast.node(symbol.def_node).span)
                .emit(r.diag_ctx);
        }
    }
}
