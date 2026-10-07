mod common;

#[test]
fn normalization_preserves_source_identity_and_creates_shared_call_facts() {
    use ast::{NodeIndex, NodeKind};
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    let source = "fn invoke(x:i64,f:fn()->i64)->i64{x+f()};fn main(){invoke(40) do {2}}";
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty());
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("post-do.ns".into()), source.into());
    let diagnostics = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    assert!(!diagnostics.has_errors());
    let post = NodeIndex(
        ast.nodes
            .iter()
            .position(|node| node.kind == NodeKind::PostDo)
            .unwrap() as u32,
    );
    let span = ast.node(post).span;
    let left = ast.fixed_children(post)[0];
    let body = ast.fixed_children(post)[1];
    let callee = ast.fixed_children(left)[0];
    let original_argument = ast.multi_children(left)[0];
    let resolved = resolution::resolve(ast, &diagnostics);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    assert_eq!(resolved.ast.node(post).kind, NodeKind::Call);
    assert_eq!(resolved.ast.node(post).span, span);
    assert_eq!(resolved.ast.fixed_children(post)[0], callee);
    let arguments = resolved.ast.multi_children(post);
    assert_eq!(arguments.len(), 2);
    assert_eq!(arguments[0], original_argument);
    let callback = arguments[1];
    assert_eq!(resolved.ast.node(callback).kind, NodeKind::Lambda);
    assert_eq!(resolved.ast.fixed_children(callback)[0], body);
    assert!(resolved.ast.multi_children(callback).is_empty());
    assert_eq!(
        resolved.node_types[&post],
        type_pool::Intrinsic::I64.type_index()
    );
    let plan = &resolved.call_arguments[&post];
    for (index, binding) in plan.parameters.iter().enumerate() {
        assert!(
            matches!(binding.value, resolution::CallArgumentValue::Explicit { source_index } if source_index == index)
        );
    }
    assert!(!resolved.node_types.contains_key(&left));
}

#[test]
fn trailing_callback_initializers_follow_invoked_bodies_without_executing_construction() {
    for source in [
        "fn apply(f:fn(Any)->Any)->List{[0].map do |x|f(x)};mod api{pub global answer:List=apply() do |x|storage.n};mod storage{pub global n:i64=42;pub fn unused()->List{api.answer}};fn main(){api.answer(0).as(i64)}",
        "fn ignore(f:fn()->Any)->i64{42};mod api{pub global answer:i64=ignore do {storage.n}};mod storage{pub global n:i64=api.answer};fn main(){storage.n}",
        "fn invoke(f:fn()->i64)->i64{f()};mod api{pub global answer:i64=invoke do {return storage.n}};mod storage{pub global n:i64=42;pub fn unused()->i64{api.answer}};fn main(){api.answer}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn trailing_lambdas_keep_associated_default_types_and_frozen_receiver_proofs() {
    for source in [
        "struct P{};trait Read{assoc Item:Type=Any;fn get(self)->Item;derive fn answer(self)->Item{let xs=[0].map do |x|self.get();xs(0).as(Item)}};extend Read for P{assoc Item:Type=String;pub fn get(self)->String{\"ab\"++\"cdefgh\"}};fn main(){P{}.answer().len()+34}",
        "struct P{};trait Read{assoc Item:Type=Any;fn get(self)->Item;derive fn answer(self)->Item{let xs=[0].map do |x|self.get();xs(0).as(Item)}};extend Read for P{assoc Item:Type=i64;pub fn get(self)->i64{42}};fn main(){P{}.answer()}",
        "fn invoke(f:fn()->i64)->i64{f()};struct P{};trait Read{fn get(self)->i64;derive fn answer(self)->i64{let saved=self;invoke do {saved.get()}}};mod a{extend Read for P{pub fn get(self)->i64{40}};pub fn answer()->i64{P{}.answer()}};mod b{extend Read for P{pub fn get(self)->i64{2}};pub fn answer()->i64{P{}.answer()}};fn main(){a.answer()+b.answer()}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn invalid_control_and_non_callable_trailing_targets_do_not_produce_artifacts() {
    for source in [
        "fn invoke(f:fn()->Unit){f()};fn main(){for x in [1]{invoke do {break}}}",
        "fn invoke(f:fn()->Unit){f()};fn main(){while true{invoke do {continue}}}",
        "fn main(){42 do |x|x}",
        "fn main(){[42].filter do {true}}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(compiled.codegen_output.functions.is_empty(), "{source}");
        assert!(compiled.into_artifact().is_err(), "{source}");
    }
}
