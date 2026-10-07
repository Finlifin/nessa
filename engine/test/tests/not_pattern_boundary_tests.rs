mod common;

#[test]
fn keyword_negation_has_distinct_ast_and_private_symbol_identity() {
    use ast::{NodeIndex, NodeKind};
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    let source = "fn main(){let x=40;1 match {not (x if x==0) as whole=>x+whole+1,_=>0}}";
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty());
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("not.ns".into()), source.into());
    let diagnostics = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    assert!(!diagnostics.has_errors());
    let negated = NodeIndex(
        ast.nodes
            .iter()
            .position(|node| node.kind == NodeKind::PatternNot)
            .unwrap() as u32,
    );
    let alias = NodeIndex(
        ast.nodes
            .iter()
            .position(|node| node.kind == NodeKind::PatternAsBind)
            .unwrap() as u32,
    );
    assert_eq!(ast.fixed_children(alias)[0], negated);
    let inner_guard = ast.fixed_children(negated)[0];
    assert_eq!(ast.node(inner_guard).kind, NodeKind::PatternIfGuard);
    let private_binding = ast.fixed_children(inner_guard)[0];
    let resolved = resolution::resolve(ast, &diagnostics);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    let private = resolved.node_symbols[&private_binding];
    let private_scope = resolved.symbols[private.0 as usize].scope;
    let outer_scope = resolved.node_scopes[&negated];
    assert_ne!(private_scope, outer_scope);
    assert_eq!(
        resolved.scopes[private_scope.0 as usize].parent,
        Some(outer_scope)
    );
    let name = str_interner::intern("x");
    let mut references: Vec<_> = resolved
        .node_symbols
        .iter()
        .filter(|(node, _)| resolved.ast.node(**node).str_id == name)
        .map(|(_, symbol)| *symbol)
        .collect();
    references.sort_by_key(|symbol| symbol.0);
    references.dedup();
    assert_eq!(references.len(), 2);
    assert!(references.contains(&private));
    assert_eq!(
        resolved.node_types[&negated],
        type_pool::Intrinsic::I64.type_index()
    );
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn negated_associated_default_patterns_specialize_private_payload_and_outer_alias() {
    let source = r#"
        struct P{};struct Q{};
        trait Source{
            assoc Item:Type=Any;
            fn next(self)->IterationStep(Item);
            fn fallback(self)->Item;
            derive fn callback(self)->fn()->Item{
                self.next() match {
                    not (IterationStep(Item).yielded(value) if false) as whole => ||{
                        whole match {IterationStep(Item).yielded(value)=>value,_=>self.fallback()}
                    },
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
fn negated_guards_keep_initializer_reads_and_callback_dependencies() {
    for source in [
        "mod api{pub global answer:i64=1 match{not(x if storage.allow) as whole=>whole+41,_=>0}};mod storage{pub global allow:bool=false;pub fn unused()->i64{api.answer}};fn main(){api.answer}",
        "fn invoke(f:fn()->bool)->bool{f()};mod api{pub global answer:i64=1 match{not(x if invoke do {storage.allow})=>42,_=>0}};mod storage{pub global allow:bool=false;pub fn unused()->i64{api.answer}};fn main(){api.answer}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn bare_enum_for_patterns_share_name_resolution_with_negation() {
    for source in [
        "enum E{none,some(n:i64)};fn main(){let n:i64=0;for E.none in [E.some(1),E.none,E.some(2),E.none]{n+=21};n}",
        "enum E{none,some(n:i64)};fn main(){let n:i64=0;for E.none or E.some(_) in [E.some(1),E.none]{n+=21};n}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}
