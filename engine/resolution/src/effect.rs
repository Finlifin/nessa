//! Phase 3c: Effect Resolution
//!
//! After name resolution has registered every `EffectDef` / `AsyncEffectDef`
//! symbol, this phase collects effect metadata (parameters, return type) and
//! stores it in [`EffectInfo`] entries on the resolver.

use ast::{Ast, NodeKind};
use type_pool::TypeIndex;

use crate::resolver::Resolver;
use crate::{EffectInfo, EffectOperation, SymbolKind};

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Collect [`EffectInfo`] entries from all effect symbols found by Phase 3a.
pub(crate) fn resolve_effects(r: &mut Resolver, ast: &Ast) {
    // Gather (name, type_index, def_node, is_async) from all Effect symbols.
    let effect_defs: Vec<_> = r
        .scopes
        .iter()
        .flat_map(|scope| scope.bindings.values())
        .filter_map(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Effect {
                // Determine async-ness from the definition node kind.
                let is_async = ast.node(sym.def_node).kind == NodeKind::AsyncEffectDef;
                Some((sym.name, sym.type_index, sym.def_node, is_async))
            } else {
                None
            }
        })
        .collect();

    for (name, type_index, def_node, is_async) in effect_defs {
        // EffectDef / AsyncEffectDef layout:
        //   fixed: [0] name  [1] ret_type
        //   multi: params (ParamTyped / ParamOptional / etc.)
        //
        // Effects do NOT have body members — their multi_children are the
        // *parameters* of the effect, not operations.  The effect itself is a
        // single "callable" with those params and a return type.

        let children = ast.fixed_children(def_node);
        let _ret_type_node = children[1]; // TODO: resolve to TypeIndex

        let mut params = Vec::new();
        for &param_node in ast.multi_children(def_node) {
            // Each param produces a placeholder TypeIndex for now.
            let _param_kind = ast.node(param_node).kind;
            params.push(TypeIndex::INVALID);
        }

        // The effect is modelled as a single operation bearing the effect's
        // own name, with the collected parameter types and return type.
        let op = EffectOperation {
            name,
            param_types: params,
            return_type: TypeIndex::INVALID, // filled in during full type resolution
        };

        r.effects.push(EffectInfo {
            name,
            type_index,
            operations: vec![op],
            is_async,
        });
    }
}
