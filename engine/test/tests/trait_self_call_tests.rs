//! Static receiver specialization retains source Self provenance without
//! changing runtime trait evidence or concrete method implementation signatures.

mod common;

use ast::{NodeIndex, NodeKind};
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use type_pool::{TypeIndex, TypeKind};

fn resolve(source: &str) -> (resolution::ResolvedAst, Vec<String>) {
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty(), "{errors:?}");
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("trait-self-call.ns".into()), source.into());
    let diagnostics = diagnostic::DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    let resolved = resolution::resolve(ast, &diagnostics);
    let errors = diagnostics
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    (resolved, errors)
}

fn named_type(resolved: &resolution::ResolvedAst, name: &str) -> TypeIndex {
    resolved
        .symbols
        .iter()
        .find(|symbol| symbol.name == str_interner::intern(name))
        .unwrap()
        .type_index
}

fn call_type(resolved: &resolution::ResolvedAst) -> TypeIndex {
    let call = resolved
        .ast
        .nodes
        .iter()
        .position(|node| node.kind == NodeKind::Call)
        .unwrap();
    resolved.node_types[&NodeIndex(call as u32)]
}

#[test]
fn inherited_self_result_uses_child_receiver_and_aliases() {
    for (parents, receiver) in [
        ("trait Child(Base){}", "Child"),
        ("trait Child(Base){};typealias View=Child", "View"),
        (
            "typealias Parent=Base;trait Child(Parent){};typealias View=Child",
            "View",
        ),
        (
            "trait Left(Base){};trait Right(Base){};trait Child(Left,Right){}",
            "Child",
        ),
    ] {
        let source = format!(
            "trait Base{{fn clone(self)->Self}};{parents};fn keep(x:{receiver})->Child{{x.clone()}}"
        );
        let (resolved, errors) = resolve(&source);
        assert!(errors.is_empty(), "{source}: {errors:?}");
        assert_eq!(
            call_type(&resolved),
            named_type(&resolved, "Child"),
            "{source}"
        );
        let result = driver::Driver::new().compile(&format!("{source};fn main(){{42}}"));
        assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
    }
    let source = "trait Base{fn clone(self)->Self};trait Child(Base){fn value(self)->i64};struct P{};impl Base for P{pub fn clone(self)->Self{self}};impl Child for P{pub fn value(self)->i64{42}};fn copied(x:Child)->Child{x.clone()};fn main(){copied(P{}).value()}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn nested_self_specializes_only_recorded_source_paths() {
    let (resolved, errors) = resolve(
        "trait Base{fn pack(self,other:Self)->(?Self,fn(Self)->Self,Base)};trait Child(Base){};fn use_pack(x:Child,y:Child){x.pack(y)}",
    );
    assert!(errors.is_empty(), "{errors:?}");
    let child = named_type(&resolved, "Child");
    let base = named_type(&resolved, "Base");
    let TypeKind::Tuple { elements } = &resolved.type_pool.get(call_type(&resolved)).kind else {
        panic!("tuple result");
    };
    assert!(
        matches!(resolved.type_pool.get(elements[0]).kind, TypeKind::Optional { inner } if inner == child)
    );
    assert!(
        matches!(&resolved.type_pool.get(elements[1]).kind, TypeKind::Function { params, ret } if params == &[child] && *ret == child)
    );
    assert_eq!(elements[2], base);
}

#[test]
fn explicit_trait_return_and_parent_name_order_are_preserved() {
    for (declarations, expected) in [
        (
            "trait Base{fn clone(self)->Base};trait Child(Base){}",
            "Base",
        ),
        (
            "trait Base{fn clone(self)->Self};trait Child(Base){fn clone(self)->Self}",
            "Child",
        ),
        (
            "trait Base{fn clone(self)->i64};trait Other{fn clone(self)->bool};trait Child(Base,Other){}",
            "i64",
        ),
    ] {
        let (resolved, errors) = resolve(&format!("{declarations};fn keep(x:Child){{x.clone()}}"));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(call_type(&resolved), named_type(&resolved, expected));
    }
}

#[test]
fn inherited_self_arguments_and_explicit_returns_reject_wrong_types() {
    for source in [
        "trait Base{fn clone(self)->Self};trait Child(Base){fn clone(self)->i64};fn keep(x:Child){x.clone()}",
        "trait Base{fn clone(self)->Base};trait Child(Base){};fn keep(x:Child)->Child{x.clone()}",
        "trait Base{fn merge(self,other:Self)->Self};trait Child(Base){};fn keep(x:Child,y:Base)->Child{x.merge(y)}",
        "trait Base{fn clone(self)->Self};trait Child(Base){};fn keep(x:Child){x.clone(42)}",
        "trait Base{fn clone(self)->Self};trait Child(Base){};fn keep(x:Child){x.unknown()}",
    ] {
        let compiled = driver::Driver::new().compile(&format!("{source};fn main(){{42}}"));
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled.codegen_output.functions.is_empty(),
            "emitted {source}"
        );
        assert!(
            compiled.diagnostics.iter().any(|diagnostic| diagnostic
                .message
                .contains("type mismatch")
                || diagnostic.message.contains("expected type")
                || diagnostic.message.contains("arguments")
                || diagnostic.message.contains("unknown member")
                || diagnostic.message.contains("signature")),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
}

#[test]
fn explicit_child_types_cannot_replace_inherited_self_binders() {
    for source in [
        "trait Base{fn clone(self)->Self};trait Child(Base){fn clone(self)->Child}",
        "trait Base{fn merge(self,other:Self)->Self};trait Child(Base){fn merge(self,other:Child)->Self}",
        "trait Base{fn clone(self)->?Self};trait Child(Base){fn clone(self)->?Child}",
        "trait Base{fn clone(self)->Child};trait Child(Base){fn clone(self)->Self}",
    ] {
        let compiled = driver::Driver::new().compile(&format!("{source};fn main(){{42}}"));
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled.codegen_output.functions.is_empty(),
            "emitted {source}"
        );
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("signature Self bindings")),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
    let (resolved, errors) = resolve(
        "trait Base{fn merge(self,other:Self)->Self};trait Child(Base){fn merge(self,other:Self)->Self};trait Grandchild(Child){};fn keep(x:Grandchild,y:Grandchild)->Grandchild{x.merge(y)}",
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(call_type(&resolved), named_type(&resolved, "Grandchild"));
}
