//! Final inferred signatures and consumer facts are independent of declaration order.

mod common;

use ast::{NodeIndex, NodeKind};
use diagnostic::{Diagnostic, DiagnosticContext, Level};
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

fn resolve(source: &str) -> (resolution::ResolvedAst, Vec<Diagnostic>) {
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty(), "{errors:?}");
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("inference.ns".into()), source.to_owned());
    let diagnostics = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    let resolved = resolution::resolve(ast, &diagnostics);
    let mut errors = diagnostics.diagnostics().to_vec();
    errors.extend(resolved.diagnostics.iter().cloned());
    (resolved, errors)
}

fn checked(source: &str) -> resolution::ResolvedAst {
    let (resolved, diagnostics) = resolve(source);
    assert!(
        !diagnostics.iter().any(|d| d.level == Level::Error),
        "{source}: {diagnostics:?}"
    );
    resolved
}

fn result_type(resolved: &resolution::ResolvedAst, name: &str) -> TypeIndex {
    let symbol = resolved
        .symbols
        .iter()
        .find(|s| str_interner::get(s.name) == name)
        .unwrap();
    let signature = resolved
        .type_pool
        .canonical_type(symbol.type_index)
        .unwrap();
    let TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind else {
        panic!("{name} must be a function");
    };
    ret
}

fn assert_result(resolved: &resolution::ResolvedAst, name: &str, expected: Intrinsic) {
    assert_eq!(
        resolved.type_pool.as_intrinsic(result_type(resolved, name)),
        Some(expected),
        "{name}"
    );
}

fn rejected(source: &str) {
    rejected_with_message(source, "type mismatch");
}

fn rejected_with_message(source: &str, message: &str) {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "accepted {source}");
    assert!(compiled.codegen_output.functions.is_empty());
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.level == Level::Error && d.message.contains(message)),
        "{source}: {:?}",
        compiled.diagnostics
    );
    assert!(compiled.into_artifact().is_err());
}

fn chain(order: &[usize], leaf: &str, main: &str) -> String {
    let depth = order.len() - 1;
    let mut source = String::new();
    for &index in order {
        let body = if index == depth {
            leaf.to_owned()
        } else {
            format!("chain_{}()", index + 1)
        };
        source.push_str(&format!("fn chain_{index}(){{{body}}};"));
    }
    source.push_str(main);
    source
}

#[test]
fn chains_of_512_dependencies_finalize_every_signature_and_call_in_all_orders() {
    const COUNT: usize = 513;
    let orders = [
        (0..COUNT).collect::<Vec<_>>(),
        (0..COUNT).rev().collect(),
        (0..COUNT).map(|i| i * 73 % COUNT).collect(),
    ];
    for order in orders {
        for (leaf, expected, main) in [
            (
                "true",
                Intrinsic::Bool,
                "fn main(){if chain_0(){42}else{0}}",
            ),
            ("42", Intrinsic::I64, "fn main(){chain_0()}"),
        ] {
            let source = chain(&order, leaf, main);
            let resolved = checked(&source);
            for index in 0..COUNT {
                assert_result(&resolved, &format!("chain_{index}"), expected);
            }
            let mut calls = 0;
            for (index, node) in resolved.ast.nodes.iter().enumerate() {
                if node.kind != NodeKind::Call {
                    continue;
                }
                let index = NodeIndex(index as u32);
                let callee = resolved.ast.fixed_children(index)[0];
                if resolved.ast.node(callee).kind == NodeKind::Id
                    && str_interner::get(resolved.ast.node(callee).str_id).starts_with("chain_")
                {
                    assert_eq!(
                        resolved.type_pool.as_intrinsic(resolved.node_types[&index]),
                        Some(expected)
                    );
                    calls += 1;
                }
            }
            assert_eq!(calls, COUNT);
            assert_eq!(common::run_value(&source), Ok(42));
        }
    }
}

#[test]
fn local_global_aliases_and_returned_lambdas_have_final_function_value_types() {
    for source in [
        "fn wrapper(){let alias=leaf;alias()};fn leaf(){42};fn main(){wrapper()}",
        "const alias=leaf;fn wrapper(){alias()};fn leaf(){42};fn main(){wrapper()}",
        "fn factory(){||leaf()};fn wrapper(){factory()()};fn leaf(){42};fn main(){wrapper()}",
    ] {
        let resolved = checked(source);
        assert_result(&resolved, "leaf", Intrinsic::I64);
        assert_result(&resolved, "wrapper", Intrinsic::I64);
        for symbol in resolved
            .symbols
            .iter()
            .filter(|s| str_interner::get(s.name) == "alias")
        {
            let signature = resolved
                .type_pool
                .canonical_type(symbol.type_index)
                .unwrap();
            let TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind else {
                panic!("alias")
            };
            assert_eq!(resolved.type_pool.as_intrinsic(ret), Some(Intrinsic::I64));
        }
        if source.contains("factory") {
            let result = resolved
                .type_pool
                .canonical_type(result_type(&resolved, "factory"))
                .unwrap();
            let TypeKind::Function { ret, .. } = resolved.type_pool.get(result).kind else {
                panic!("factory result")
            };
            assert_eq!(resolved.type_pool.as_intrinsic(ret), Some(Intrinsic::I64));
        }
        for (index, node) in resolved.ast.nodes.iter().enumerate() {
            if node.kind != NodeKind::Call {
                continue;
            }
            let result = resolved.node_types[&NodeIndex(index as u32)];
            if let Some(signature) = resolved.type_pool.canonical_type(result)
                && let TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind
            {
                assert_eq!(resolved.type_pool.as_intrinsic(ret), Some(Intrinsic::I64));
            } else {
                assert_eq!(
                    resolved.type_pool.as_intrinsic(result),
                    Some(Intrinsic::I64)
                );
            }
        }
        assert_eq!(common::run_value(source), Ok(42));
    }
    for source in [
        "fn wrapper(){let alias=leaf;alias()};fn leaf(){true};fn main(){let n:i64=wrapper();42}",
        "const alias=leaf;fn wrapper(){alias()};fn leaf(){true};fn main(){let n:i64=wrapper();42}",
        "fn factory(){||leaf()};fn wrapper(){factory()()};fn leaf(){true};fn main(){let n:i64=wrapper();42}",
    ] {
        rejected(source);
    }
}

#[test]
fn static_projections_and_methods_follow_inferred_dependencies() {
    for source in [
        "mod api{pub fn wrapper(){leaf()};fn leaf(){42}};fn main(){api.wrapper()}",
        "struct P{};impl P{pub fn wrapper(){leaf()};fn leaf(){42}};fn main(){P.wrapper()}",
        "struct P{};impl P{pub fn wrapper(self){self.leaf()};fn leaf(self){42}};fn main(){P{}.wrapper()}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
    for source in [
        "mod api{pub fn wrapper(){leaf()};fn leaf(){true}};fn main(){let n:i64=api.wrapper();42}",
        "struct P{};impl P{pub fn wrapper(){leaf()};fn leaf(){true}};fn main(){let n:i64=P.wrapper();42}",
        "struct P{};impl P{pub fn wrapper(self){self.leaf()};fn leaf(self){true}};fn main(){let n:i64=P{}.wrapper();42}",
    ] {
        rejected(source);
    }
}

#[test]
fn unused_and_selected_defaults_reject_fully_inferred_bad_chains() {
    for declarations in [
        "effect ask(.n:i64=wrapper())->i64;fn wrapper(){middle()};fn middle(){leaf()};fn leaf(){true};",
        "fn leaf(){true};fn middle(){leaf()};fn wrapper(){middle()};effect ask(.n:i64=wrapper())->i64;",
    ] {
        for main in ["fn main(){42}", "fn main(){ask()#{ask(n)=>n}}"] {
            rejected(&format!("{declarations}{main}"));
        }
    }
}

#[test]
fn explicit_and_genuinely_inferred_any_keep_runtime_boundary_assertions() {
    for declarations in [
        "fn wrapper(){leaf()};fn leaf()->Any{true};",
        "fn wrapper(){leaf(true)};fn leaf(b:bool){if b{true}else{42}};",
    ] {
        let source = format!("{declarations}fn main(){{let n:i64=wrapper();42}}");
        let resolved = checked(&source);
        assert_result(&resolved, "wrapper", Intrinsic::Any);
        let wrapper_call = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .find_map(|(i, n)| {
                let i = NodeIndex(i as u32);
                (n.kind == NodeKind::Call
                    && str_interner::get(
                        resolved.ast.node(resolved.ast.fixed_children(i)[0]).str_id,
                    ) == "wrapper")
                    .then_some(i)
            })
            .unwrap();
        assert_eq!(
            resolved.node_coercions[&wrapper_call].kind,
            resolution::CoercionKind::Assert
        );
        assert_eq!(
            resolved
                .type_pool
                .as_intrinsic(resolved.node_coercions[&wrapper_call].target),
            Some(Intrinsic::I64)
        );
        assert!(
            common::run_value(&source)
                .unwrap_err()
                .contains("TypeError")
        );
    }
}

#[test]
fn finalized_concrete_results_do_not_retain_provisional_any_assertions() {
    let source = "fn caller()->i64{leaf()};fn leaf(){42};fn main(){caller()}";
    let resolved = checked(source);
    for (index, node) in resolved
        .ast
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == NodeKind::Call)
    {
        let index = NodeIndex(index as u32);
        assert_eq!(
            resolved.type_pool.as_intrinsic(resolved.node_types[&index]),
            Some(Intrinsic::I64)
        );
        assert!(
            !resolved
                .node_coercions
                .get(&index)
                .is_some_and(|c| c.kind == resolution::CoercionKind::Assert),
            "stale assertion on {node:?}"
        );
    }
    assert_eq!(common::run_value(source), Ok(42));
    let (_, diagnostics) = resolve("fn caller()->i64{leaf()};fn leaf(){true};fn main(){42}");
    let errors = diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("type mismatch"));
}

#[test]
fn annotated_recursion_anchors_results_and_unannotated_cycles_stay_gradual() {
    let source = "fn first(n:i64)->i64{if n==0{42}else{second(n-1)}};fn second(n:i64){first(n)};fn main(){second(3)}";
    let resolved = checked(source);
    assert_result(&resolved, "first", Intrinsic::I64);
    assert_result(&resolved, "second", Intrinsic::I64);
    assert_eq!(common::run_value(source), Ok(42));
    for source in [
        "fn first(){second()};fn second(){first()};fn main(){42}",
        "fn first(n:i64){if n==0{42}else{second(n-1)}};fn second(n:i64){first(n)};fn main(){42}",
    ] {
        let resolved = checked(source);
        assert_result(&resolved, "first", Intrinsic::Any);
        assert_result(&resolved, "second", Intrinsic::Any);
        assert_eq!(common::run_value(source), Ok(42));
    }
}

#[test]
fn narrow_and_optional_return_boundaries_preserve_concrete_representations() {
    for source in [
        "fn wrapper(){leaf()};fn leaf()->i8{42};fn main(){let n:?i64=wrapper();if type_of(n)==i64 and n==42{42}else{0}}",
        "fn wrapper(){leaf()};fn leaf()->?i64{null};fn main(){if wrapper()==null{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn associated_self_item_defaults_keep_separate_replay_results() {
    for source in [
        "struct P{};trait Read{assoc Item:Type=Any;fn get(self)->Item;derive fn answer(self)->Item{self.get()}};impl Read for P{assoc Item:Type=i64;pub fn get(self)->i64{42}};fn main(){P{}.answer()}",
        "struct P{text:String};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn callback(self)->fn()->Item{let item:Item=self.next();||->Item{item}}};impl Source for P{pub fn next(self)->P{self}};fn main(){let p=P{text:\"ab\"++\"cdefgh\"}.callback()();p.text.len()+34}",
        "struct P{};trait Read{fn get(self)->i64;derive fn answer(self)->i64{self.get()}};mod a{extend Read for P{pub fn get(self)->i64{40}};pub fn answer()->i64{P{}.answer()}};mod b{extend Read for P{pub fn get(self)->i64{2}};pub fn answer()->i64{P{}.answer()}};fn main(){a.answer()+b.answer()}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn nested_function_results_wait_for_their_lexical_binding_types() {
    for source in [
        "fn outer(){let x=40;fn inner(){x+2};inner()};fn main(){outer()}",
        "fn outer(x:i64){fn inner(){x+2};inner()};fn main(){outer(40)}",
        "fn outer(){let (x,y)=(40,2);fn inner(){x+y};inner()};fn main(){outer()}",
        "fn apply(f:fn(i64)->i64)->i64{f(40)};fn main(){apply(|x|{fn inner(){x+2};inner()})}",
    ] {
        let resolved = checked(source);
        assert_result(&resolved, "inner", Intrinsic::I64);
        if source.contains("outer") {
            assert_result(&resolved, "outer", Intrinsic::I64);
        }
        rejected_with_message(source, "named functions cannot capture");
    }
}

#[test]
fn inferred_closure_factory_keeps_heap_captures_live_through_completed_gc() {
    fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
        context.require_arity(1)?;
        assert!(context.collect_garbage()?);
        context.return_unit();
        Ok(())
    }
    let source = "fn factory(){let text=leaf();||{print(\"\");text.len()+34}};fn leaf(){\"ab\"++\"cdefgh\"};fn main(){let f=factory();print(\"\");let first=f();print(\"\");let second=f();if first==42 and second==42{second}else{0}}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    engine
        .vm_mut()
        .register_builtin(runtime::ids::PRINT, collect);
    let before = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert!(engine.vm_mut().completed_collections() >= before + 4);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn nested_unannotated_chains_finalize_in_both_declaration_orders() {
    for body in [
        "fn leaf(){true};fn wrapper(){leaf()};wrapper()",
        "fn wrapper(){leaf()};fn leaf(){true};wrapper()",
    ] {
        rejected(&format!(
            "effect ask(.n:i64=outer())->i64;fn outer(){{{body}}};fn main(){{42}}"
        ));
    }
    for body in [
        "fn leaf(){42};fn wrapper(){leaf()};wrapper()",
        "fn wrapper(){leaf()};fn leaf(){42};wrapper()",
    ] {
        let source = format!("fn outer(){{{body}}};fn main(){{outer()}}");
        let resolved = checked(&source);
        for name in ["outer", "wrapper", "leaf"] {
            assert_result(&resolved, name, Intrinsic::I64);
        }
        assert_eq!(common::run_value(&source), Ok(42));
    }
}

#[test]
fn lambda_captures_keep_lexical_result_types_in_executable_programs() {
    for source in [
        "fn outer(){let x=40;let inner=||{x+2};inner()};fn main(){outer()}",
        "fn outer(x:i64){let inner=||{x+2};inner()};fn main(){outer(40)}",
        "fn outer(){let (x,y)=(40,2);let inner=||{x+y};inner()};fn main(){outer()}",
        "fn apply(f:fn(i64)->i64)->i64{f(40)};fn main(){apply(|x|{let inner=||{x+2};inner()})}",
    ] {
        let resolved = checked(source);
        assert_result(&resolved, "inner", Intrinsic::I64);
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn forward_block_functions_shadow_outer_functions_before_their_declaration() {
    let source = "fn leaf(){true};fn main(){let result=leaf();fn leaf(){42};result}";
    let resolved = checked(source);
    assert_result(&resolved, "main", Intrinsic::I64);
    let call = resolved
        .ast
        .nodes
        .iter()
        .enumerate()
        .find_map(|(index, node)| (node.kind == NodeKind::Call).then_some(NodeIndex(index as u32)))
        .unwrap();
    assert_eq!(
        resolved.type_pool.as_intrinsic(resolved.node_types[&call]),
        Some(Intrinsic::I64)
    );
    assert_eq!(common::run_value(source), Ok(42));
    rejected("fn leaf(){42};fn main(){let result:i64=leaf();fn leaf(){true};result}");
}

fn factory_method_chain(leaf: &str, prefix: &str, main: &str) -> String {
    let mut source = format!(
        "{prefix}fn wrapper(){{factory().m0()}};fn factory(){{P{{}}}};struct P{{}};impl P{{"
    );
    for index in 0..513 {
        let body = if index == 512 {
            leaf.to_owned()
        } else {
            format!("self.m{}()", index + 1)
        };
        source.push_str(&format!("pub fn m{index}(self){{{body}}};"));
    }
    source.push_str("};");
    source.push_str(main);
    source
}

#[test]
fn factory_receivers_finalize_long_method_signatures_and_caller_facts() {
    for (leaf, expected) in [("true", Intrinsic::Bool), ("42", Intrinsic::I64)] {
        let source = factory_method_chain(leaf, "", "fn main(){42}");
        let resolved = checked(&source);
        assert_result(&resolved, "wrapper", expected);
        for index in 0..513 {
            assert_result(&resolved, &format!("m{index}"), expected);
        }
        let mut method_calls = 0;
        for (index, node) in resolved.ast.nodes.iter().enumerate() {
            if node.kind != NodeKind::Call {
                continue;
            }
            let index = NodeIndex(index as u32);
            let callee = resolved.ast.fixed_children(index)[0];
            if resolved.ast.node(callee).kind == NodeKind::Projection {
                assert_eq!(
                    resolved.type_pool.as_intrinsic(resolved.node_types[&index]),
                    Some(expected)
                );
                method_calls += 1;
            }
        }
        assert_eq!(method_calls, 513);
    }
}

#[test]
fn nested_named_callbacks_keep_each_associated_replay_and_explicit_any_separate() {
    let source = "struct P{value:i64};struct Q{value:i64};trait Read{assoc Item:Type=Self;derive fn named(self)->fn(Item)->Item{fn identity(other:Item)->Item{other};identity};derive fn explicit(self,value:Any)->Any{value}};impl Read for P{};impl Read for Q{};fn main(){let p=P{value:40};let q=Q{value:2};let pn=p.named();let qn=q.named();if type_of(pn)==(fn(P)->P) and type_of(qn)==(fn(Q)->Q) and type_of(p.explicit(q))==Q{pn(p).value+qn(q).value}else{0}}";
    let resolved = checked(source);
    assert_result(&resolved, "explicit", Intrinsic::Any);
    for (callback, receiver) in [("pn", "p"), ("qn", "q")] {
        let receiver_type = resolved
            .symbols
            .iter()
            .find(|s| str_interner::get(s.name) == receiver)
            .unwrap()
            .type_index;
        assert_eq!(
            resolved
                .type_pool
                .canonical_type(result_type(&resolved, callback)),
            resolved.type_pool.canonical_type(receiver_type)
        );
    }
    assert_eq!(common::run_value(source), Ok(42));
}
