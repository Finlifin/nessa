//! Checked apply/update signatures for the engine's dynamic collections.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{CollectionRole, Intrinsic, TypeIndex, TypeKind, TypePool};

use crate::resolver::Resolver;
use crate::{SymbolKind, typing};

#[derive(Clone, Copy)]
pub(crate) enum CollectionKind {
    List,
    Map,
}

impl CollectionKind {
    pub(crate) fn from_type(pool: &TypePool, ty: TypeIndex) -> Option<Self> {
        match pool.collection_role(ty)? {
            CollectionRole::List => Some(Self::List),
            CollectionRole::Map => Some(Self::Map),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::List => "List",
            Self::Map => "Map",
        }
    }

    fn index_type(self) -> TypeIndex {
        match self {
            Self::List => Intrinsic::Usize.type_index(),
            Self::Map => Intrinsic::Str.type_index(),
        }
    }

    fn index_name(self) -> &'static str {
        match self {
            Self::List => "usize",
            Self::Map => "String",
        }
    }
}

pub(crate) fn index_call(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    kind: CollectionKind,
) -> TypeIndex {
    let name = kind.name();
    let index_type = kind.index_type();
    let arguments = ast.multi_children(node);
    if arguments.len() != 1 {
        r.diag_ctx
            .error(format!(
                "{name} indexing requires exactly one {} argument",
                kind.index_name()
            ))
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
    for &argument in arguments {
        if ast.node(argument).kind == NodeKind::NamedArg {
            r.diag_ctx
                .error(format!("{name} indexing requires a positional index"))
                .with_primary_span(ast.node(argument).span)
                .emit(r.diag_ctx);
        }
        typing::resolve_types_expected(r, ast, argument, Some(index_type));
        if let Some(&actual) = r.node_types.get(&argument)
            && !r.type_pool.is_subtype(actual, index_type)
            && !r.type_pool.is_gradually_consistent(actual, index_type)
        {
            r.diag_ctx
                .error(format!("{name} index must have type {}", kind.index_name()))
                .with_primary_span(ast.node(argument).span)
                .emit(r.diag_ctx);
        }
    }
    Intrinsic::Any.type_index()
}

pub(crate) fn method_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    projection: NodeIndex,
    ty: TypeIndex,
    kind: CollectionKind,
) -> Option<TypeIndex> {
    let collection_name = kind.name();
    let name = ast.node(ast.fixed_children(projection)[1]).str_id;
    let symbol = r
        .scopes
        .iter()
        .filter(|scope| scope.assoc_type == r.type_pool.canonical_type(ty))
        .find_map(|scope| scope.bindings.get(&name).copied());
    let Some(symbol) = symbol else {
        r.diag_ctx
            .error(format!(
                "unknown {collection_name} member `{}`",
                str_interner::get(name)
            ))
            .with_primary_span(ast.node(projection).span)
            .emit(r.diag_ctx);
        return None;
    };
    let method = symbol;
    let symbol = &r.symbols[symbol.0 as usize];
    if r.current_call_callee != Some(projection) {
        r.diag_ctx
            .error(format!("bound {collection_name} method values are not implemented; call the method directly"))
            .with_primary_span(ast.node(projection).span)
            .emit(r.diag_ctx);
        return None;
    }
    if symbol.kind != SymbolKind::Function || symbol.def_node.is_null() {
        r.diag_ctx
            .error(format!(
                "{collection_name} instance members must be methods"
            ))
            .with_primary_span(ast.node(projection).span)
            .emit(r.diag_ctx);
        return None;
    }
    let signature = r.type_pool.canonical_type(symbol.type_index)?;
    let TypeKind::Function { params, ret } = r.type_pool.get(signature).kind.clone() else {
        return None;
    };
    let parameters = ast.multi_children(symbol.def_node);
    if parameters
        .first()
        .is_none_or(|&parameter| ast.node(parameter).kind != NodeKind::ParamSelf)
    {
        r.diag_ctx
            .error(format!(
                "{collection_name} instance methods require a self parameter"
            ))
            .with_primary_span(ast.node(projection).span)
            .emit(r.diag_ctx);
        return None;
    }
    r.instance_methods.insert(projection, method);
    Some(r.register_type(TypeKind::Function {
        params: params.into_iter().skip(1).collect(),
        ret,
    }))
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    use super::*;

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("map.ns".into()), source.to_owned());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve_with_options(
            ast,
            &diagnostics,
            crate::ResolveOptions::for_builtin_package(),
        );
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        (resolved, errors)
    }

    #[test]
    fn map_alias_indexing_has_string_keys_any_values_and_checked_dynamic_keys() {
        let (resolved, errors) = resolve(
            "typealias Map = .Map'builtin\ntypealias Entries=Map\nfn main(m:Entries, key:Any){m(\"key\")=true; m(key)}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let mut calls = 0;
        for (index, node) in resolved.ast.nodes.iter().enumerate() {
            if node.kind != NodeKind::Call {
                continue;
            }
            let call = NodeIndex(index as u32);
            assert_eq!(resolved.node_types[&call], Intrinsic::Any.type_index());
            let key = resolved.ast.multi_children(call)[0];
            if resolved.ast.node(key).kind == NodeKind::Id {
                assert_eq!(
                    resolved.node_coercions[&key],
                    crate::Coercion {
                        target: Intrinsic::Str.type_index(),
                        kind: crate::CoercionKind::Assert
                    }
                );
            }
            calls += 1;
        }
        assert_eq!(calls, 2);
    }

    #[test]
    fn map_methods_omit_receiver_and_check_key_signature() {
        let prefix = "typealias Map=.Map'builtin\nimpl Map{pub fn get(self,key:String)->Any{null};pub fn set(self,key:String,value:Any){}}\n";
        let (_, errors) = resolve(&format!(
            "{prefix}fn main(m:Map){{m.set(\"key\",true);m.get(\"key\")}}"
        ));
        assert!(errors.is_empty(), "{errors:?}");
        for (tail, message) in [
            ("fn main(m:Map){m(0)}", "Map index must have type String"),
            (
                "fn main(m:Map){m(0)=true}",
                "Map index must have type String",
            ),
            ("fn main(m:Map){m()}", "exactly one String"),
            ("fn main(m:Map){m(\"a\",\"b\")}", "exactly one String"),
            ("fn main(m:Map){m(key=\"a\")}", "positional index"),
            ("fn main(m:Map){m.get(42)}", "type mismatch"),
            (
                "fn main(m:Map){let f=m.get;f(\"key\")}",
                "bound Map method values",
            ),
            ("fn main(m:Map){m.capacity}", "unknown Map member"),
            ("fn main(){Map{}}", "collection storage"),
            ("fn main(){let m:Map={}}", "anonymous Object"),
            ("derive Eq for Map", "collection traits cannot be derived"),
        ] {
            let (_, errors) = resolve(&format!("{prefix}{tail}"));
            assert!(
                errors.iter().any(|error| error.contains(message)),
                "{tail}: {errors:?}"
            );
        }
    }

    #[test]
    fn builtin_map_view_uses_current_pool_identity_after_relocation() {
        let map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = DiagnosticContext::new(&map);
        let mut resolver =
            Resolver::new(&diagnostics, crate::ResolveOptions::for_builtin_package());
        // This relocation fixture tests legacy collection descriptors, without source-only binders.
        resolver.type_pool = type_pool::TypePool::with_intrinsics();
        let original = resolver.type_pool.map_type().unwrap();
        let mut snapshot = resolver.type_pool.snapshot();
        let offset = original.as_u32() as usize;
        snapshot.types.insert(
            offset,
            type_pool::TypeInfo {
                kind: TypeKind::Struct {
                    name: str_interner::intern("BeforeMapRoles"),
                    fields: vec![],
                },
                type_id: type_pool::TypeId::ZERO,
                size: 0,
                align: 8,
            },
        );
        snapshot.methods.insert(offset, vec![]);
        resolver.type_pool = TypePool::restore(snapshot).unwrap();
        let expected = resolver.type_pool.map_type().unwrap();
        assert_eq!(expected.as_u32(), original.as_u32() + 1);
        let symbol = resolver
            .resolve_builtin_view(NodeIndex::NULL, "Map")
            .unwrap();
        assert_eq!(resolver.symbols[symbol.0 as usize].type_index, expected);
    }
}
