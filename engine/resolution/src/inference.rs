//! Order static result dependencies before their consumers without recursively
//! descending a chain of source function calls on the Rust stack.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TypeIndex, TypeKind};

use crate::{SymbolId, SymbolKind, resolver::Resolver, typing};

/// A body is checked once in each fact context. Specialized trait replay saves
/// and restores this state together with the types and lowering facts it owns.
#[derive(Clone, Default)]
pub(crate) struct InferenceState {
    pub(crate) headers_prepared: bool,
    value_initializers: HashMap<SymbolId, NodeIndex>,
    pub(crate) blocks: HashSet<NodeIndex>,
    pub(crate) headers: HashSet<NodeIndex>,
    pub(crate) bodies: HashMap<NodeIndex, BodyState>,
    pub(crate) values: HashSet<NodeIndex>,
    pub(crate) cyclic: HashSet<NodeIndex>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyState {
    InProgress,
    Ready,
}

fn function(ast: &Ast, node: NodeIndex) -> bool {
    matches!(
        ast.node(node).kind,
        NodeKind::FunctionDef | NodeKind::TraitDeriveFn
    )
}

fn static_scope(r: &Resolver<'_>, ast: &Ast, symbol: SymbolId) -> bool {
    let mut current = Some(r.symbols[symbol.0 as usize].scope);
    while let Some(scope) = current {
        let entry = &r.scopes[scope.0 as usize];
        if !entry.node.is_null()
            && matches!(
                ast.node(entry.node).kind,
                NodeKind::Block
                    | NodeKind::FunctionDef
                    | NodeKind::Lambda
                    | NodeKind::CaseArm
                    | NodeKind::ForLoop
            )
        {
            return false;
        }
        current = entry.parent;
    }
    let scope = r.scopes[r.symbols[symbol.0 as usize].scope.0 as usize].node;
    !scope.is_null()
        && matches!(
            ast.node(scope).kind,
            NodeKind::FileScope
                | NodeKind::ModuleDef
                | NodeKind::StructDef
                | NodeKind::EnumDef
                | NodeKind::ImplDef
                | NodeKind::ImplTraitDef
                | NodeKind::ExtendDef
                | NodeKind::ExtendTraitDef
                | NodeKind::TraitDef
        )
}

/// Receiver annotations and value aliases suffice to select source methods
/// before body checking. Following aliases here never invokes the type checker
/// or emits diagnostics; actual accessibility and call contracts are checked by
/// ordinary projection/operator typing.
fn receiver_type(
    r: &Resolver<'_>,
    ast: &Ast,
    expression: NodeIndex,
    values: &HashMap<SymbolId, NodeIndex>,
) -> Option<TypeIndex> {
    let mut pending = vec![(expression, false)];
    let mut visited = HashSet::new();
    let mut types = HashMap::new();
    while let Some((node, finish)) = pending.pop() {
        if node.is_null() {
            continue;
        }
        let alias = r
            .node_symbols
            .get(&node)
            .and_then(|symbol| values.get(symbol))
            .copied();
        if finish {
            let children = ast.fixed_children(node);
            let ty =
                if let Some(alias) = alias {
                    types.get(&alias).copied()
                } else {
                    match ast.node(node).kind {
                        NodeKind::ExtendedCall => types.get(&children[0]).copied().and_then(|ty| {
                            match r.type_pool.get(ty).kind {
                                TypeKind::Function { ret, .. } | TypeKind::Effect { ret, .. } => {
                                    r.type_pool.canonical_type(ret)
                                }
                                _ => Some(ty),
                            }
                        }),
                        NodeKind::Call => types.get(&children[0]).copied().and_then(|ty| {
                            let TypeKind::Function { ret, .. } = r.type_pool.get(ty).kind else {
                                return None;
                            };
                            r.type_pool.canonical_type(ret)
                        }),
                        NodeKind::Projection => types.get(&children[0]).copied().and_then(|ty| {
                            let TypeKind::Struct { fields, .. } = &r.type_pool.get(ty).kind else {
                                return None;
                            };
                            fields
                                .iter()
                                .find(|field| field.name == ast.node(children[1]).str_id)
                                .and_then(|field| r.type_pool.canonical_type(field.ty))
                        }),
                        _ => None,
                    }
                };
            if let Some(ty) = ty {
                types.insert(node, ty);
            }
            continue;
        }
        if !visited.insert(node) {
            continue;
        }
        if let Some(&symbol) = r.node_symbols.get(&node)
            && let Some(ty) = r
                .type_pool
                .canonical_type(r.symbols[symbol.0 as usize].type_index)
        {
            types.insert(node, ty);
            continue;
        }
        let dependency = alias.or_else(|| match ast.node(node).kind {
            NodeKind::ExtendedCall | NodeKind::Call | NodeKind::Projection => {
                ast.fixed_children(node).first().copied()
            }
            _ => None,
        });
        if let Some(dependency) = dependency {
            pending.push((node, true));
            pending.push((dependency, false));
        }
    }
    types.remove(&expression)
}

pub(crate) fn resolve_static_dependencies(r: &mut Resolver<'_>, ast: &Ast) {
    r.function_inference.value_initializers = ast
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            if !matches!(
                node.kind,
                NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl
            ) {
                return None;
            }
            let children = ast.fixed_children(NodeIndex(index as u32));
            r.node_symbols
                .get(&children[0])
                .map(|&symbol| (symbol, children[2]))
        })
        .collect();
    resolve_nodes(r, ast, static_nodes(r, ast), None);
}

fn static_nodes(r: &Resolver<'_>, ast: &Ast) -> Vec<NodeIndex> {
    let mut nodes = typing::module_value_declarations(r, ast);
    nodes.extend(r.symbols.iter().filter_map(|symbol| {
        (!symbol.def_node.is_null()
            && symbol.kind == SymbolKind::Function
            && function(ast, symbol.def_node)
            && static_scope(r, ast, symbol.id))
        .then_some(symbol.def_node)
    }));
    nodes
}

/// Methods discovered after a lexical/factory result becomes known still use
/// the same iterative dependency checker, rather than recursively typing their
/// source bodies from within another call's type checker.
pub(crate) fn resolve_function_dependencies(r: &mut Resolver<'_>, ast: &Ast, function: NodeIndex) {
    let mut nodes = static_nodes(r, ast);
    nodes.push(function);
    resolve_nodes(r, ast, nodes, Some(function));
}

/// The enclosing expression establishes lambda parameters and match/loop
/// binders before its block is reached. Local functions and value initializers
/// can then be ordered using their actual bound identities, including captures.
pub(crate) fn resolve_block_dependencies(r: &mut Resolver<'_>, ast: &Ast, block: NodeIndex) {
    if !r.function_inference.blocks.insert(block) {
        return;
    }
    typing::prepare_block_signatures(r, ast, block);
    let mut nodes: Vec<_> = ast
        .multi_children(block)
        .iter()
        .copied()
        .filter(|&node| {
            function(ast, node)
                || matches!(
                    ast.node(node).kind,
                    NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl
                )
        })
        .collect();
    for &child in ast.multi_children(block) {
        if matches!(
            ast.node(child).kind,
            NodeKind::ExtendDef | NodeKind::ExtendTraitDef
        ) {
            nodes.extend(
                ast.multi_children(child)
                    .iter()
                    .map(|&node| crate::structs::unwrap_member(ast, node))
                    .filter(|&node| function(ast, node)),
            );
        }
    }
    for &node in &nodes {
        if !function(ast, node) {
            typing::prepare_value_annotation(r, ast, node);
        }
    }
    resolve_nodes(r, ast, nodes, None);
}

fn resolve_nodes(
    r: &mut Resolver<'_>,
    ast: &Ast,
    mut nodes: Vec<NodeIndex>,
    root: Option<NodeIndex>,
) {
    nodes.sort_unstable_by_key(|node| node.0);
    nodes.dedup();
    if nodes.is_empty() {
        return;
    }
    let mut definitions = HashMap::new();
    for (index, &node) in nodes.iter().enumerate() {
        let mut pending = vec![ast.fixed_children(node)[0]];
        while let Some(binding) = pending.pop() {
            if binding.is_null() {
                continue;
            }
            if let Some(&symbol) = r.node_symbols.get(&binding) {
                definitions.insert(symbol, index);
            }
            pending.extend(ast.fixed_children(binding));
            pending.extend(ast.multi_children(binding));
        }
    }
    let mut edges = vec![Vec::new(); nodes.len()];
    let mut methods = vec![Vec::new(); nodes.len()];
    for (index, &node) in nodes.iter().enumerate() {
        let children = ast.fixed_children(node);
        let mut pending = vec![children[2]];
        if function(ast, node) {
            pending.extend(ast.multi_children(node));
            pending.push(children[3]);
        }
        while let Some(expression) = pending.pop() {
            if expression.is_null() {
                continue;
            }
            let mut references = Vec::new();
            if let Some(&symbol) = r.node_symbols.get(&expression) {
                references.push(symbol);
            }
            let method = match ast.node(expression).kind {
                NodeKind::Projection if !r.node_symbols.contains_key(&expression) => {
                    let children = ast.fixed_children(expression);
                    (!children[1].is_null()).then_some((children[0], ast.node(children[1]).str_id))
                }
                NodeKind::Concat => Some((
                    ast.fixed_children(expression)[0],
                    str_interner::intern("concat"),
                )),
                _ => None,
            };
            if let Some(method) = method {
                methods[index].push(method);
            }
            if let Some((receiver, name)) = method
                && let Some(ty) =
                    receiver_type(r, ast, receiver, &r.function_inference.value_initializers)
                && let Ok(symbol) = crate::associated::member(r, ty, name)
            {
                references.push(symbol);
            }
            for symbol in references {
                if let Some(&dependency) = definitions.get(&symbol) {
                    let definition = nodes[dependency];
                    // Explicit function results and value annotations are
                    // complete contracts, including within a dependency cycle.
                    if ast.fixed_children(definition)[1].is_null() {
                        edges[index].push(dependency);
                    }
                }
            }
            pending.extend(ast.fixed_children(expression));
            pending.extend(ast.multi_children(expression));
        }
        edges[index].sort_unstable();
        edges[index].dedup();
    }
    let (order, cyclic) = dependency_order(&edges);
    r.function_inference.cyclic.extend(
        cyclic
            .into_iter()
            .filter_map(|index| function(ast, nodes[index]).then_some(nodes[index])),
    );
    // Static dependencies establish factory result contracts first. Revisit
    // only the selected receiver edges, then schedule newly resolved methods
    // before checking the consumer body. No checked AST body is replayed here.
    let roots = root
        .map(|root| {
            nodes
                .iter()
                .position(|&node| node == root)
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or(order);
    let mut state = vec![None; nodes.len()];
    for root in roots {
        if state[root].is_some() {
            continue;
        }
        state[root] = Some(BodyState::InProgress);
        let mut pending = vec![(root, 0usize)];
        while let Some(&(index, next)) = pending.last() {
            if next < edges[index].len() {
                let child = edges[index][next];
                pending
                    .last_mut()
                    .expect("a pending frame was just inspected")
                    .1 += 1;
                if state[child].is_none() {
                    state[child] = Some(BodyState::InProgress);
                    pending.push((child, 0));
                } else if state[child] == Some(BodyState::InProgress) {
                    let cycle_start = pending
                        .iter()
                        .position(|&(node, _)| node == child)
                        .expect("in-progress dependencies belong to this work stack");
                    r.function_inference
                        .cyclic
                        .extend(pending[cycle_start..].iter().filter_map(|&(index, _)| {
                            function(ast, nodes[index]).then_some(nodes[index])
                        }));
                }
                continue;
            }
            let mut added = false;
            for &(receiver, name) in &methods[index] {
                if let Some(ty) =
                    receiver_type(r, ast, receiver, &r.function_inference.value_initializers)
                    && let Ok(symbol) = crate::associated::member(r, ty, name)
                    && let Some(&dependency) = definitions.get(&symbol)
                    && ast.fixed_children(nodes[dependency])[1].is_null()
                    && !edges[index].contains(&dependency)
                {
                    edges[index].push(dependency);
                    added = true;
                }
            }
            if added {
                continue;
            }
            let node = nodes[index];
            if function(ast, node) {
                typing::resolve_function_def_types(r, ast, node);
            } else {
                typing::resolve_decl(r, ast, node);
                r.function_inference.values.insert(node);
            }
            state[index] = Some(BodyState::Ready);
            pending.pop();
        }
    }
}

/// Iterative DFS yields dependencies first. A second walk on reversed edges
/// identifies genuinely cyclic components, rather than treating downstream
/// consumers as cyclic too. Unannotated cycles retain the existing gradual Any
/// contract; solving anchored result equations within an SCC is a separate task.
fn dependency_order(edges: &[Vec<usize>]) -> (Vec<usize>, HashSet<usize>) {
    let mut visited = vec![false; edges.len()];
    let mut order = Vec::with_capacity(edges.len());
    for root in 0..edges.len() {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        let mut pending = vec![(root, 0)];
        while let Some((node, next)) = pending.last_mut() {
            if *next == edges[*node].len() {
                order.push(*node);
                pending.pop();
            } else {
                let child = edges[*node][*next];
                *next += 1;
                if !visited[child] {
                    visited[child] = true;
                    pending.push((child, 0));
                }
            }
        }
    }
    let mut reverse = vec![Vec::new(); edges.len()];
    for (node, dependencies) in edges.iter().enumerate() {
        for &dependency in dependencies {
            reverse[dependency].push(node);
        }
    }
    visited.fill(false);
    let mut cyclic = HashSet::new();
    for &root in order.iter().rev() {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        let mut pending = vec![root];
        let mut component = Vec::new();
        while let Some(node) = pending.pop() {
            component.push(node);
            for &child in &reverse[node] {
                if !visited[child] {
                    visited[child] = true;
                    pending.push(child);
                }
            }
        }
        if component.len() > 1 || edges[root].contains(&root) {
            cyclic.extend(component);
        }
    }
    (order, cyclic)
}

#[cfg(test)]
mod tests {
    use super::dependency_order;

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        use diagnostic::DiagnosticContext;
        use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("inference.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        (resolved, errors)
    }

    fn result(resolved: &crate::ResolvedAst, name: &str) -> type_pool::TypeIndex {
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| {
                symbol.kind == crate::SymbolKind::Function && str_interner::get(symbol.name) == name
            })
            .unwrap();
        let type_pool::TypeKind::Function { ret, .. } =
            resolved.type_pool.get(symbol.type_index).kind
        else {
            panic!("function signature missing");
        };
        ret
    }

    #[test]
    fn long_function_chain_finalizes_symbols_calls_and_rejects_once() {
        use type_pool::Intrinsic;
        let mut source = String::new();
        for index in 0..512 {
            source.push_str(&format!("fn f{index}(){{f{}()}};", index + 1));
        }
        source.push_str("fn f512(){true};effect unused(.x:i64=f0())->i64;fn main(){42}");
        let (resolved, errors) = resolve(&source);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            errors[0],
            "type mismatch in parameter default: expected `i64`, found `bool`"
        );
        for index in 0..=512 {
            assert_eq!(
                result(&resolved, &format!("f{index}")),
                Intrinsic::Bool.type_index()
            );
        }
        for (index, node) in resolved.ast.nodes.iter().enumerate() {
            if node.kind == ast::NodeKind::Call {
                assert_eq!(
                    resolved.node_types[&ast::NodeIndex(index as u32)],
                    Intrinsic::Bool.type_index()
                );
            }
        }
        assert!(resolved.node_coercions.is_empty());
    }

    #[test]
    fn lexical_nested_functions_wait_for_captured_bindings_and_later_leaves() {
        use type_pool::Intrinsic;
        for source in [
            "effect unused(.x:i64=outer())->i64;fn outer(){fn wrapper(){leaf()};fn leaf(){true};wrapper()};fn main(){42}",
            "effect unused(.x:i64=outer())->i64;fn outer(){fn leaf(){true};fn wrapper(){leaf()};wrapper()};fn main(){42}",
        ] {
            let (resolved, errors) = resolve(source);
            assert_eq!(
                errors,
                ["type mismatch in parameter default: expected `i64`, found `bool`"]
            );
            assert_eq!(result(&resolved, "outer"), Intrinsic::Bool.type_index());
        }
        for source in [
            "fn outer(){let x=40;fn inner(){x+2};inner()}",
            "fn outer(){fn wrapper(){leaf()};let x=40;fn leaf(){x+2};wrapper()}",
            "fn outer(){let (x,y)=(40,2);fn inner(){x+y};inner()}",
            "fn outer(){let f:fn(i64)->i64=|x|{fn inner(){x+2};inner()};f(40)}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert_eq!(result(&resolved, "outer"), Intrinsic::I64.type_index());
        }
    }

    #[test]
    fn module_aliases_and_returned_function_values_use_final_signatures() {
        use type_pool::Intrinsic;
        for source in [
            "const callback=factory();fn wrapper(){callback()};fn factory(){leaf};fn leaf(){true}",
            "const first=second;const second=leaf;fn wrapper(){let f=first;f()};fn leaf(){true}",
            "mod other{pub fn leaf(){true}};fn wrapper(){other.leaf()}",
            "struct P{};impl P{pub fn leaf(){true}};fn wrapper(){P.leaf()}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert_eq!(result(&resolved, "wrapper"), Intrinsic::Bool.type_index());
        }
    }

    #[test]
    fn annotated_cycles_anchor_results_and_dynamic_results_remain_dynamic() {
        use type_pool::Intrinsic;
        for source in ["fn a()->i64{b()};fn b(){a()}", "global n:i64=a();fn a(){n}"] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert_eq!(result(&resolved, "a"), Intrinsic::I64.type_index());
        }
        for source in [
            "fn a(){b()};fn b(){a()}",
            "fn a()->Any{true}",
            "fn a(x:bool){if x{42}else{true}}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert_eq!(result(&resolved, "a"), Intrinsic::Any.type_index());
        }
    }

    #[test]
    fn self_method_chains_finalize_before_receivers_and_consumers() {
        use type_pool::Intrinsic;
        let (resolved, errors) = resolve(
            "struct Leaf{};struct P{child:Leaf};fn wrapper(p:P){p.outer()};impl P{fn outer(self){self.child.leaf()}};impl Leaf{fn leaf(self){true}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(result(&resolved, "wrapper"), Intrinsic::Bool.type_index());
        let mut source = String::from("struct P{};fn wrapper(p:P){p.m0()};impl P{");
        for index in 0..512 {
            source.push_str(&format!("fn m{index}(self){{self.m{}()}};", index + 1));
        }
        source.push_str("fn m512(self){true}};");
        let (resolved, errors) = resolve(&source);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(result(&resolved, "wrapper"), Intrinsic::Bool.type_index());
        for index in 0..=512 {
            assert_eq!(
                result(&resolved, &format!("m{index}")),
                Intrinsic::Bool.type_index()
            );
        }
    }

    #[test]
    fn local_function_declarations_keep_identity_through_value_shadowing() {
        use type_pool::Intrinsic;
        for source in [
            "fn outer(){let f=40;fn f(){true};f()}",
            "fn outer(){fn f(){true};let f=||42;f()}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert_eq!(result(&resolved, "f"), Intrinsic::Bool.type_index());
        }
        let (_, errors) = resolve("fn outer(){fn f(){true};fn f(){42};f()}");
        assert_eq!(errors, ["duplicate definition `f`"]);
        let (_, errors) = resolve("fn outer(){fn f(){x};let x=42;f()}");
        assert!(errors.iter().any(|error| error == "undefined name `x`"));
    }

    #[test]
    fn nested_associated_function_headers_are_specialized_per_replay() {
        use type_pool::{Intrinsic, TypeKind};
        let (resolved, errors) = resolve(
            "struct P{};struct Q{};trait Read{assoc Item:Type=Self;derive fn named(self)->fn(Item)->Item{fn identity(other:Item)->Item{other};identity};derive fn explicit(self)->fn(Any)->Any{fn raw(other:Any)->Any{other};raw}};impl Read for P{};impl Read for Q{}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let identity = resolved
            .symbols
            .iter()
            .find(|symbol| {
                symbol.kind == crate::SymbolKind::Function
                    && str_interner::get(symbol.name) == "identity"
            })
            .unwrap();
        let raw = resolved
            .symbols
            .iter()
            .find(|symbol| {
                symbol.kind == crate::SymbolKind::Function
                    && str_interner::get(symbol.name) == "raw"
            })
            .unwrap();
        let mut implementors = std::collections::HashSet::new();
        for plan in &resolved.default_methods {
            let nested = if plan.specialized_symbol_types.contains_key(&identity.id) {
                identity.id
            } else {
                raw.id
            };
            let signature = plan.specialized_symbol_types[&nested];
            let TypeKind::Function { params, ret } = &resolved.type_pool.get(signature).kind else {
                panic!("missing specialized nested function signature");
            };
            if nested == identity.id {
                let TypeKind::Function { ret: returned, .. } =
                    resolved.type_pool.get(plan.signature).kind
                else {
                    panic!("missing adapter signature");
                };
                assert_eq!(signature, returned);
                assert_eq!(params, &[*ret]);
                assert!(!resolved.type_pool.contains_associated_type(signature));
                implementors.insert(str_interner::get(match resolved.type_pool.get(*ret).kind {
                    TypeKind::Struct { name, .. } => name,
                    _ => panic!("nested signature must use its concrete implementor"),
                }));
            } else {
                assert_eq!(params, &[Intrinsic::Any.type_index()]);
                assert_eq!(*ret, Intrinsic::Any.type_index());
            }
        }
        assert_eq!(implementors, ["P".into(), "Q".into()].into_iter().collect());
        // Context-only declarations never publish an abstract executable header.
        assert_eq!(identity.type_index, type_pool::TypeIndex::INVALID);
    }

    #[test]
    fn inferred_factory_receivers_schedule_long_method_chains_in_a_child() {
        const CHILD: &str = "NESSA_INFERENCE_FACTORY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "inference::tests::inferred_factory_receivers_schedule_long_method_chains_in_a_child", "--nocapture"])
                .env(CHILD, "1")
                .output().unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use type_pool::Intrinsic;
        for wrapper in ["factory().m0()", "fn make(){P{}};make().m0()"] {
            let mut source = format!(
                "effect unused(.x:i64=wrapper())->i64;fn wrapper(){{{wrapper}}};fn factory(){{P{{}}}};struct P{{}};impl P{{"
            );
            for index in 0..512 {
                source.push_str(&format!("fn m{index}(self){{self.m{}()}};", index + 1));
            }
            source.push_str("fn m512(self){true}};fn main(){42}");
            let (resolved, errors) = resolve(&source);
            assert_eq!(
                errors,
                ["type mismatch in parameter default: expected `i64`, found `bool`"]
            );
            assert_eq!(result(&resolved, "wrapper"), Intrinsic::Bool.type_index());
            for index in 0..=512 {
                assert_eq!(
                    result(&resolved, &format!("m{index}")),
                    Intrinsic::Bool.type_index()
                );
            }
        }
    }

    #[test]
    fn lexical_extensions_prepare_headers_with_their_enclosing_block() {
        use type_pool::Intrinsic;
        for source in [
            "struct P{};fn outer(){extend P{pub fn value(self)->i64{42}};P{}.value()}",
            "struct P{};trait Read{fn value(self)->i64};fn outer(){extend Read for P{pub fn value(self)->i64{42}};P{}.value()}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert_eq!(result(&resolved, "outer"), Intrinsic::I64.type_index());
            assert_eq!(result(&resolved, "value"), Intrinsic::I64.type_index());
        }
    }

    #[test]
    fn long_dependencies_use_a_work_stack() {
        let mut edges: Vec<_> = (1..20_000).map(|next| vec![next]).collect();
        edges.push(Vec::new());
        let (order, cyclic) = dependency_order(&edges);
        assert_eq!(order, (0..20_000).rev().collect::<Vec<_>>());
        assert!(cyclic.is_empty());
    }

    #[test]
    fn cycle_does_not_mark_its_consumers() {
        let (order, cyclic) = dependency_order(&[vec![1], vec![2], vec![1], vec![0]]);
        assert_eq!(order, vec![2, 1, 0, 3]);
        assert_eq!(cyclic, [1, 2].into_iter().collect());
    }
}
