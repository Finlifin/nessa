mod common;

#[test]
fn rest_ast_and_successful_bindings_have_exact_list_and_any_types() {
    use ast::{NodeIndex, NodeKind};
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    let source = "fn main(){[40,2] match{[head,...tail]=>head.as(i64)+tail(0).as(i64),_=>0}}";
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty());
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("list-pattern.ns".into()), source.into());
    let diagnostics = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    let pattern = NodeIndex(
        ast.nodes
            .iter()
            .position(|node| node.kind == NodeKind::PatternList)
            .unwrap() as u32,
    );
    let children = ast.multi_children(pattern);
    assert_eq!(children.len(), 2);
    let (head, rest) = (children[0], children[1]);
    assert_eq!(ast.node(rest).kind, NodeKind::PatternRestBind);
    let tail = ast.fixed_children(rest)[0];
    assert_eq!(ast.node(tail).kind, NodeKind::Id);
    assert_eq!(ast.node(tail).str_id, str_interner::intern("tail"));
    let resolved = resolution::resolve(ast, &diagnostics);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    let list = resolved.type_pool.list_type().unwrap();
    assert_eq!(resolved.node_types[&pattern], list);
    assert_eq!(
        resolved.node_types[&head],
        type_pool::Intrinsic::Any.type_index()
    );
    assert_eq!(resolved.node_types[&rest], list);
    assert_eq!(resolved.node_types[&tail], list);
    assert_eq!(
        resolved.symbols[resolved.node_symbols[&tail].0 as usize].type_index,
        list
    );
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn rest_alternatives_share_one_symbol_and_preserve_canonical_list_aliases() {
    for source in [
        "typealias Items=List;fn main(){let xs:Items=[40,2];xs match{([0,...tail] or [...tail]) as whole=>if tail'type==List and whole'type==Items{tail(0).as(i64)+tail(1).as(i64)}else{0},_=>0}}",
        "fn main(){let tail=100;[40,2] match{[...tail]=>tail(0).as(i64)+tail(1).as(i64),_=>0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn list_patterns_keep_associated_default_item_specialization_and_captures() {
    let source = r#"
        struct P{};struct Q{};
        trait Values{
            assoc Item:Type=Any;
            fn get(self)->Item;
            derive fn callback(self)->fn()->Item{
                [self.get()] match{[value,...rest] if rest.len()==0=>||value.as(Item)}
            }
        };
        impl Values for P{assoc Item:Type=i64;pub fn get(self)->i64{42}};
        impl Values for Q{assoc Item:Type=String;pub fn get(self)->String{"abcd"++"efgh"}};
        fn main(){let p:fn()->i64=P{}.callback();let q:fn()->String=Q{}.callback();
            if p'type==fn()->i64 and q'type==fn()->String and q()=="abcdefgh"{p()}else{0}}
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn list_guards_keep_frozen_receiver_proofs_and_initializer_callback_dependencies() {
    for source in [
        "struct P{};trait Read{fn value(self)->i64;derive fn callback(self)->fn()->i64{[0,1] match{[x,...rest] if self.value()>0=>||self.value()+rest.len()-1,_=>||0}}};mod a{extend Read for P{pub fn value(self)->i64{40};};pub fn callback()->fn()->i64{P{}.callback()}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{let f=a.callback();f()+P{}.value()}};fn main(){b.answer()}",
        "fn invoke(f:fn()->bool)->bool{f()};mod api{pub global answer:i64=[40,2] match{[head,...tail] if invoke do {storage.allow}=>head.as(i64)+tail(0).as(i64),_=>0}};mod storage{pub global allow:bool=true;pub fn unused()->i64{api.answer}};fn main(){api.answer}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}
