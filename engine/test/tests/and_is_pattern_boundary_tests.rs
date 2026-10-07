mod common;

#[test]
fn constraint_ast_separates_expression_and_binders_with_exact_types() {
    use ast::{NodeIndex, NodeKind};
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    let source = "fn main(){(40,2) match{(a,b) and a+b is answer if answer==42=>answer,_=>0}}";
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty());
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("and-is.ns".into()), source.into());
    let diagnostics = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    let node = NodeIndex(
        ast.nodes
            .iter()
            .position(|node| node.kind == NodeKind::PatternAndIs)
            .unwrap() as u32,
    );
    let children = ast.fixed_children(node);
    assert_eq!(children.len(), 3);
    assert_eq!(ast.node(children[0]).kind, NodeKind::PatternTuple);
    assert_eq!(ast.node(children[1]).kind, NodeKind::Add);
    assert_eq!(ast.node(children[2]).kind, NodeKind::Id);
    let (expression, binding) = (children[1], children[2]);
    let resolved = resolution::resolve(ast, &diagnostics);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    assert_eq!(
        resolved.node_types[&expression],
        type_pool::Intrinsic::I64.type_index()
    );
    assert_eq!(
        resolved.node_types[&binding],
        type_pool::Intrinsic::I64.type_index()
    );
    let name = str_interner::intern("answer");
    let symbols: Vec<_> = resolved
        .node_symbols
        .iter()
        .filter(|(node, _)| resolved.ast.node(**node).str_id == name)
        .map(|(_, symbol)| *symbol)
        .collect();
    assert_eq!(symbols.len(), 3);
    assert!(
        symbols
            .iter()
            .all(|symbol| *symbol == resolved.node_symbols[&binding])
    );
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn constraints_specialize_associated_defaults_and_rhs_payload_captures() {
    let source = r#"
        struct P{};struct Q{};
        trait Source{
            assoc Item:Type=Any;
            fn next(self)->IterationStep(Item);
            fn fallback(self)->Item;
            derive fn callback(self)->fn()->Item{
                self match {
                    _ and self.next() is IterationStep(Item).yielded(value) as whole if whole'type==IterationStep(Item) => ||value,
                    _=>||self.fallback()
                }
            }
        };
        impl Source for P{
            assoc Item:Type=i64;
            pub fn next(self)->IterationStep(i64){IterationStep(i64).yielded(42)};
            pub fn fallback(self)->i64{0}
        };
        impl Source for Q{
            assoc Item:Type=String;
            pub fn next(self)->IterationStep(String){IterationStep(String).yielded("abcd"++"efgh")};
            pub fn fallback(self)->String{""}
        };
        fn main(){let p:fn()->i64=P{}.callback();let q:fn()->String=Q{}.callback();
            if p'type==fn()->i64 and q'type==fn()->String and q()=="abcdefgh"{p()}else{0}}
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn computed_receiver_aliases_keep_selected_proofs_after_escape() {
    let source = r#"
        struct P{};
        trait Read{
            fn value(self)->i64;
            derive fn callback(self)->fn()->i64{
                self match{_ and self is saved=>||saved.value()}
            }
        };
        mod a{extend Read for P{pub fn value(self)->i64{40}};
            pub fn callback()->fn()->i64{P{}.callback()}}
        mod b{extend Read for P{pub fn value(self)->i64{2}};
            pub fn answer()->i64{let f=a.callback();f()+P{}.value()}}
        fn main(){b.answer()}
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn initializer_constraints_follow_invoked_callbacks_and_computed_expressions() {
    for source in [
        "mod api{pub global answer:i64=0 match{_ and storage.n is value=>value}};mod storage{pub global n:i64=42;pub fn unused()->i64{api.answer}};fn main(){api.answer}",
        "fn invoke(f:fn()->i64)->i64{f()};mod api{pub global answer:i64=0 match{_ and invoke do {storage.n} is value=>value}};mod storage{pub global n:i64=42;pub fn unused()->i64{api.answer}};fn main(){api.answer}",
        "mod api{pub global answer:i64=0 match{_ and storage.n is value if storage.allow=>value,_=>0}};mod storage{pub global n:i64=42;pub global allow:bool=true;pub fn unused()->i64{api.answer}};fn main(){api.answer}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}
