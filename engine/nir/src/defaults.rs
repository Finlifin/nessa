//! Declaration defaults that execute at a checked call or construction site.

use ast::NodeIndex;
use resolution::{CallArgumentValue, ResolvedAst, StructFieldValue};

pub(crate) fn selected_defaults(
    resolved: &ResolvedAst,
    node: NodeIndex,
) -> impl Iterator<Item = (NodeIndex, NodeIndex)> + '_ {
    let parameters = resolved
        .call_arguments
        .get(&node)
        .into_iter()
        .flat_map(|plan| {
            plan.parameters
                .iter()
                .filter_map(move |binding| match binding.value {
                    CallArgumentValue::Default { expression, .. } => {
                        Some((plan.declaration, expression))
                    }
                    CallArgumentValue::Explicit { .. }
                    | CallArgumentValue::Variadic { .. }
                    | CallArgumentValue::MapVariadic { .. } => None,
                })
        });
    let fields = resolved
        .struct_constructions
        .get(&node)
        .into_iter()
        .flat_map(|plan| {
            plan.fields
                .iter()
                .filter_map(move |binding| match binding.value {
                    StructFieldValue::Default { expression } => {
                        Some((plan.declaration, expression))
                    }
                    StructFieldValue::Explicit { .. } => None,
                })
        });
    parameters.chain(fields)
}
