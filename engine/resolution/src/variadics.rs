//! Source packing layouts are declaration contracts, independent of Fn arity.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::CollectionRole;

use crate::resolver::Resolver;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Fixed,
    List(usize),
    ListMap,
}

pub(crate) fn layout(
    r: &Resolver<'_>,
    ast: &Ast,
    parameters: &[NodeIndex],
) -> Result<Layout, &'static str> {
    let parameters: Vec<_> = parameters
        .iter()
        .copied()
        .filter(|&parameter| {
            !matches!(
                ast.node(parameter).kind,
                NodeKind::ParamSelf | NodeKind::ParamCatch
            )
        })
        .collect();
    let variadics: Vec<_> = parameters
        .iter()
        .enumerate()
        .filter(|&(_, &parameter)| ast.node(parameter).kind == NodeKind::ParamVarargs)
        .map(|(index, _)| index)
        .collect();
    let role = |index: usize| {
        crate::typing::param_type_index(r, ast, parameters[index])
            .and_then(|ty| r.type_pool.collection_role(ty))
    };
    match variadics.as_slice() {
        [] => Ok(Layout::Fixed),
        &[index] => {
            if role(index) != Some(CollectionRole::List) {
                return Err("a variadic parameter requires a List type annotation");
            }
            if parameters[index + 1..]
                .iter()
                .any(|&parameter| ast.node(parameter).kind != NodeKind::ParamOptional)
            {
                return Err("a variadic parameter must follow all fixed positional parameters");
            }
            Ok(Layout::List(index))
        }
        &[0, 1]
            if parameters.len() == 2
                && role(0) == Some(CollectionRole::List)
                && role(1) == Some(CollectionRole::Map) =>
        {
            Ok(Layout::ListMap)
        }
        _ => Err(
            "dual variadic parameters require exactly List then Map and no other caller parameters",
        ),
    }
}
