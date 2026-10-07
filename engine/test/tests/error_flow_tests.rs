//! Source Error carriers preserve domains, branches, payloads and control boundaries.
mod common;

use ast::NodeKind;
use diagnostic::{Diagnostic, DiagnosticContext, Level};
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

fn resolve(source: &str) -> resolution::ResolvedAst {
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty(), "{errors:?}");
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(
        FileName::Custom("error-contract.ns".into()),
        source.to_owned(),
    );
    let ctx = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &ctx, file.start_pos).parse();
    let resolved = resolution::resolve(ast, &ctx);
    let errors: Vec<Diagnostic> = ctx
        .diagnostics()
        .iter()
        .chain(resolved.diagnostics.iter())
        .filter(|d| d.level == Level::Error)
        .cloned()
        .collect();
    assert!(errors.is_empty(), "{source}: {errors:?}");
    resolved
}
fn function_return(resolved: &resolution::ResolvedAst, name: &str) -> TypeIndex {
    let candidates: Vec<_> = resolved
        .symbols
        .iter()
        .filter(|s| str_interner::get(s.name) == name)
        .collect();
    assert_eq!(candidates.len(), 1);
    let signature = resolved
        .type_pool
        .canonical_type(candidates[0].type_index)
        .unwrap();
    let TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind else {
        panic!("function signature")
    };
    ret
}
fn shape(resolved: &resolution::ResolvedAst, name: &str) -> (Vec<TypeIndex>, TypeIndex) {
    let t = resolved
        .type_pool
        .canonical_type(function_return(resolved, name))
        .unwrap();
    let TypeKind::ErrorQualified { errors, inner } = &resolved.type_pool.get(t).kind else {
        panic!("{name} must remain ErrorQualified")
    };
    assert_eq!(resolved.type_pool.get(t).size, 24);
    assert_eq!(resolved.type_pool.get(t).align, 8);
    (errors.clone(), *inner)
}
fn answer(source: &str) {
    assert_eq!(common::run_value(source), Ok(42), "{source}");
}
fn rejected(source: &str, category: &str) {
    let result = driver::Driver::new().compile(source);
    assert!(result.has_errors, "accepted {source}");
    assert!(result.codegen_output.functions.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.level == Level::Error && d.message.to_ascii_lowercase().contains(category)),
        "{source}: {:?}",
        result.diagnostics
    );
    assert!(result.into_artifact().is_err());
}
fn runtime_type_error(source: &str) {
    let result = common::run_value(source);
    assert!(
        matches!(&result,Err(e)if e.contains("TypeError")),
        "{source}: {result:?}"
    );
}

#[test]
fn concrete_const_concat_alias_nested_and_empty_sets_have_canonical_source_facts() {
    let source = "enum E{bad};enum F{bad};typealias Alias=E;const Left=[Alias];const Right=[F,E];fn a()->![E,F] i64{42};fn b()->!(Left++Right) i64{42};fn c()->!F !Alias i64{42};fn empty()->![] i64{42};fn main(){42}";
    let resolved = resolve(source);
    let a = function_return(&resolved, "a");
    let b = function_return(&resolved, "b");
    let c = function_return(&resolved, "c");
    assert_eq!(
        resolved.type_pool.canonical_type(a),
        resolved.type_pool.canonical_type(b)
    );
    assert_eq!(
        resolved.type_pool.canonical_type(a),
        resolved.type_pool.canonical_type(c)
    );
    let (members, inner) = shape(&resolved, "a");
    assert_eq!(members.len(), 2);
    assert_eq!(resolved.type_pool.as_intrinsic(inner), Some(Intrinsic::I64));
    assert_eq!(
        resolved
            .type_pool
            .as_intrinsic(function_return(&resolved, "empty")),
        Some(Intrinsic::I64)
    );
    answer(source);
}

#[test]
fn invalid_set_expressions_cycles_and_runtime_computation_never_emit_artifacts() {
    for source in [
        "fn main()->!Any i64{42}",
        "const A=B;const B=A;fn main()->!A i64{42}",
        "enum E{bad};fn types()->List{[E]};fn main()->!types() i64{42}",
        "enum E{bad};const Values=[E,42];fn main()->!Values i64{42}",
        "enum E{bad};global Values:List=[E];fn main()->!Values i64{42}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(result.codegen_output.functions.is_empty());
        assert!(result.into_artifact().is_err());
    }
}

#[test]
fn construction_facts_are_error_only_and_dynamic_payload_never_tags_any() {
    let source =
        "enum E{bad};fn make(){error E.bad};fn dynamic(value:Any){error value};fn main(){42}";
    let resolved = resolve(source);
    let (members, inner) = shape(&resolved, "make");
    assert_eq!(members.len(), 1);
    assert_eq!(
        resolved.type_pool.as_intrinsic(inner),
        Some(Intrinsic::NoReturn)
    );
    let (open, inner) = shape(&resolved, "dynamic");
    assert_eq!(open, vec![Intrinsic::Any.type_index()]);
    assert_eq!(
        resolved.type_pool.as_intrinsic(inner),
        Some(Intrinsic::NoReturn)
    );
    assert_eq!(resolved.error_constructions.len(), 2);
    let mut statics = 0;
    let mut dynamics = 0;
    for plan in resolved.error_constructions.values() {
        match plan.tag {
            resolution::ErrorTagSource::Static(t) => {
                assert_eq!(
                    resolved.type_pool.canonical_type(t),
                    resolved.type_pool.canonical_type(members[0])
                );
                statics += 1;
            }
            resolution::ErrorTagSource::DynamicPayload => dynamics += 1,
        };
        assert!(matches!(
            resolved.ast.node(plan.operand).kind,
            NodeKind::Projection | NodeKind::Symbol | NodeKind::Id
        ));
    }
    assert_eq!((statics, dynamics), (1, 1));
}

#[test]
fn implicit_success_and_explicit_error_remain_distinct_even_with_identical_payload_types() {
    for source in [
        "enum E{bad};fn main(){let good:!E E=E.bad;let bad:!E E=error E.bad;let a=good match{E.bad! =>20,error E.bad=>0};let b=bad match{E.bad! =>0,error E.bad=>22};a+b}",
        "fn main(){let good:!i64 i64=20;let bad:!i64 i64=error 22;let a=good match{n! =>n,error _=>0};let b=bad match{_! =>0,error n=>n.as(i64)};a+b}",
        "enum E{bad};fn main(){let good:!E i8=42;good match{n! =>if type_of(n)==i8{n}else{0},error _=>0}}",
    ] {
        answer(source);
    }
}

#[test]
fn operand_is_evaluated_once_and_fields_globals_arguments_returns_and_closures_lift_success() {
    answer(
        "enum E{bad};global calls:i64=0;global total:!E i64=40;struct Box{value:!E i64};fn next()->i64{calls+=1;2};fn consume(x:!E i64)->!E i64{x};fn make()->fn()->!E i64{let b=Box{value:next()};||consume(b.value)};fn main(){let f=make();let a=total!{E.bad=>0};let b=f()!{E.bad=>0};if calls==1{a+b}else{0}}",
    );
    answer(
        "enum E{bad};global calls:i64=0;fn next()->E{calls+=1;E.bad};fn main(){let v:!E i64=error next();v match{error E.bad=>if calls==1{42}else{0},_! =>0}}",
    );
}

#[test]
fn qualified_subset_and_payload_numeric_tuple_optional_conversions_keep_branch() {
    for source in [
        "enum E{bad};enum F{bad};fn widen(x:!E i8)->![E,F] i64{x};fn main(){widen(42) match{n! =>if type_of(n)==i64{n}else{0},error _=>0}}",
        "enum E{bad};enum F{bad};fn widen(x:!E i8)->![E,F] i64{x};fn main(){widen(error E.bad) match{error E.bad=>42,_=>0}}",
        "enum E{bad};fn widen(x:!E (i8,i8))->!E (i64,i64){x};fn main(){widen((40,2)) match{(a,b)! =>if type_of(a)==i64 and type_of(b)==i64{a+b}else{0},error _=>0}}",
        "enum E{bad};fn widen(x:!E ?i8)->!E ?i64{x};fn main(){widen(42) match{n! =>n.unwrap(),error _=>0}}",
    ] {
        answer(source);
    }
    rejected(
        "enum E{bad};enum F{bad};fn narrow(x:!F Any)->!E Any{x};fn main(){42}",
        "type",
    );
}

#[test]
fn dynamic_construction_rethrow_and_erased_carrier_assertions_enforce_actual_closed_domains() {
    answer(
        "enum E{bad};fn again(value:Any)->!E i64{error value};fn main(){again(E.bad) match{error E.bad=>42,_! =>0}}",
    );
    answer(
        "enum E{bad};fn again(value:Any)->!E i64{error value};fn main(){let original:!E i64=error E.bad;let result=original!{catch e=>again(e)};result match{error E.bad=>42,_! =>0}}",
    );
    for source in [
        "enum E{bad};enum F{bad};fn again(value:Any)->!E i64{error value};fn main(){again(F.bad);42}",
        "enum E{bad};enum F{bad};fn main(){let erased:Any=error F.bad;let checked=erased.as(!E i64);42}",
        "enum E{bad};fn main(){let erased:Any=E.bad;let checked=erased.as(!E i64);42}",
    ] {
        runtime_type_error(source);
    }
}

#[test]
fn propagation_exits_only_its_callable_once_and_skips_following_side_effects() {
    answer(
        "enum E{bad};global trace:i64=0;fn get()->!E i64{error E.bad};fn read()->!E i64{let n=get()!;trace=99;n};fn main(){let result=read();result match{error E.bad=>if trace==0{42}else{0},_! =>0}}",
    );
    answer(
        "enum E{bad};global trace:i64=0;fn get()->!E i64{40};fn read()->!E i64{let n=get()!;trace+=1;n+2};fn main(){read() match{n! =>if trace==1{n}else{0},error _=>0}}",
    );
    answer(
        "enum E{bad};fn main(){let f=||->!E i64{let v:!E i64=error E.bad;v!;99};let result=f();result match{error E.bad=>42,_! =>0}}",
    );
}

#[test]
fn propagation_return_inference_joins_domains_and_retains_precise_success_types() {
    let source = "enum E{bad};enum F{bad};fn e()->!E i64{40};fn f()->!F i64{2};fn read(){let a=e()!;let b=f()!;a+b};fn only(){error E.bad};fn mixed(flag:bool){if flag{42}else{error E.bad}};fn main(){read()!{E.* =>0,F.* =>0}}";
    let resolved = resolve(source);
    let (members, inner) = shape(&resolved, "read");
    assert_eq!(members.len(), 2);
    assert_eq!(resolved.type_pool.as_intrinsic(inner), Some(Intrinsic::I64));
    let (_, only) = shape(&resolved, "only");
    assert_eq!(
        resolved.type_pool.as_intrinsic(only),
        Some(Intrinsic::NoReturn)
    );
    let (_, mixed) = shape(&resolved, "mixed");
    assert_eq!(resolved.type_pool.as_intrinsic(mixed), Some(Intrinsic::I64));
    assert_eq!(resolved.error_propagations.len(), 2);
    for plan in resolved.error_propagations.values() {
        assert_eq!(
            resolved.type_pool.as_intrinsic(plan.success),
            Some(Intrinsic::I64)
        );
        assert_eq!(
            resolved.type_pool.canonical_type(plan.return_target),
            resolved
                .type_pool
                .canonical_type(function_return(&resolved, "read"))
        );
    }
    answer(source);
}

#[test]
fn propagation_rejects_plain_values_nonqualified_returns_and_inline_default_escape() {
    for source in [
        "fn main(){42!}",
        "enum E{bad};fn read()->i64{let x:!E i64=error E.bad;x!};fn main(){42}",
        "enum E{bad};fn get()->!E i64{error E.bad};fn call(.n:i64=get()!)->i64{n};fn main(){42}",
        "enum E{bad};fn get()->!E i64{error E.bad};struct P{n:i64=get()!};fn main(){42}",
        "enum E{bad};fn get()->!E i64{error E.bad};global n:i64=get()!;fn main(){42}",
    ] {
        rejected(source, "error");
    }
    answer(
        "enum E{bad};fn call(.f:fn()->!E i64=||{let v:!E i64=error E.bad;v!})->i64{f() match{error E.bad=>42,_! =>0}};fn main(){call()}",
    );
}

#[test]
fn elimination_preserves_implicit_success_and_residual_error_without_hidden_return() {
    answer(
        "enum E{a,b};global trace:i64=0;fn main(){let value:!E i64=error E.b;let residual=value!{E.a=>0};trace+=1;residual match{error E.b=>41+trace,error _=>0,_! =>0}}",
    );
    answer("enum E{bad};fn main(){let value:!E i64=40;value!{E.bad=>0}+2}");
    answer("enum E{bad};fn main(){let value:!E i64=40;value!{ok! =>ok+2,E.bad=>0}}");
    answer("enum E{bad};fn main(){let value:!E i64=42;value!{_! =>42,E.bad=>0}}");
}

#[test]
fn family_all_variants_and_generic_catches_remove_closed_domains_but_guards_keep_residual() {
    let source = "enum E{a,b};fn family(v:!E i64){v!{E.* as e=>e match{E.a=>40,E.b=>42}}};fn variants(v:!E i64){v!{E.a=>40,E.b=>42}};fn partial(v:!E i64){v!{E.a=>0}};fn guarded(v:!E i64){v!{E.* if false=>0}};fn main(){family(error E.b)}";
    let resolved = resolve(source);
    for name in ["family", "variants"] {
        assert_eq!(
            resolved
                .type_pool
                .as_intrinsic(function_return(&resolved, name)),
            Some(Intrinsic::I64)
        );
    }
    for name in ["partial", "guarded"] {
        let (members, inner) = shape(&resolved, name);
        assert_eq!(members.len(), 1);
        assert_eq!(resolved.type_pool.as_intrinsic(inner), Some(Intrinsic::I64));
    }
    assert_eq!(resolved.error_eliminations.len(), 4);
    answer(source);
    answer(
        "enum E{bad};fn main(){let value:!E i64=error E.bad;value!{catch e=>if type_of(e)==E{42}else{0}}}",
    );
}

#[test]
fn open_domains_cannot_be_exhausted_by_a_finite_family_but_generic_error_binder_can() {
    let source = "enum E{bad};fn dynamic(v:Any){error v};fn finite(v:Any){dynamic(v)!{E.* =>42}};fn all(v:Any){dynamic(v)!{catch e=>42}};fn main(){all(E.bad)}";
    let resolved = resolve(source);
    let (errors, _) = shape(&resolved, "finite");
    assert_eq!(errors, vec![Intrinsic::Any.type_index()]);
    assert_eq!(
        resolved
            .type_pool
            .as_intrinsic(function_return(&resolved, "all")),
        Some(Intrinsic::I64)
    );
    answer(source);
}

#[test]
fn ordered_refutable_composite_patterns_export_real_payload_bindings() {
    answer(
        "enum E{packet(text:String,values:List),other};fn main(){let value:!E i64=error E.packet(\"abcd\"++\"efgh\",[20,22]);value!{E.packet(text,[a,b]) if false=>0,E.packet(text,[a,b])=>if text.len()==8{a.as(i64)+b.as(i64)}else{0},E.* =>0,_! =>0}}",
    );
    answer(
        "enum E{bad};fn main(){let value:!E (i64,i64)=(20,22);value match{((a,b) as whole)! if whole.0==a=>a+b,_! =>0,error _=>0}}",
    );
    answer(
        "enum E{bad};fn main(){let value:!E i64=42;if (value matches n! if n==42 and type_of(n)==i64){42}else{0}}",
    );
    rejected(
        "enum E{bad};fn main(){let value:!E i64=42;if value matches n!{n}else{0}}",
        "undefined",
    );
    for source in [
        "enum E{bad};fn main(){let value:!E i64=42;let n! = value;42}",
        "enum E{bad};fn take(n! : !E i64)->i64{n};fn main(){42}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors);
        assert!(result.into_artifact().is_err());
    }
}

#[test]
fn branch_introduced_errors_join_with_original_residual_and_do_not_escape_implicitly() {
    answer(
        "enum E{a,b};enum F{bad};global trace:i64=0;fn main(){let value:!E i64=error E.a;let joined=value!{E.a=>error F.bad};trace+=1;joined match{error F.bad=>if trace==1{42}else{0},error E.b=>0,error _=>0,_! =>0}}",
    );
}

#[test]
fn typed_default_self_and_associated_item_replay_keep_two_concrete_domains() {
    answer(
        "enum E{bad};enum F{bad};struct P{};struct Q{};trait Read{assoc Err:Type=E;assoc Item:Type=i64;fn get(self)->!Err Item;derive fn answer(self)->!Err Item{self.get()!}};impl Read for P{pub fn get(self)->!E i64{40}};impl Read for Q{assoc Err:Type=F;pub fn get(self)->!F i64{2}};fn main(){let a=P{}.answer()!{E.* =>0};let b=Q{}.answer()!{F.* =>0};a+b}",
    );
    answer(
        "struct P{n:i64};struct Q{n:i64};trait Fail{derive fn fail(self)->!Self i64{error self}};impl Fail for P{};impl Fail for Q{};fn main(){let a=P{n:20}.fail() match{error p=>p.as(P).n,_! =>0};let b=Q{n:22}.fail() match{error q=>q.as(Q).n,_! =>0};a+b}",
    );
}

#[test]
fn handler_propagation_has_its_own_return_boundary_and_generic_catch_has_none() {
    answer(
        "enum E{bad};effect choose(catch k)->!E i64;fn work()->!E i64{choose()#!};fn main(){let value:!E i64=work()#{choose(k)=>{let result:!E i64=error E.bad;result!;k(42)}};value match{error E.bad=>42,_! =>0}}",
    );
    answer(
        "enum E{bad};fn read()->!E i64{let value:!E i64=error E.bad;value!{catch e=>{return error e}}};fn main(){read() match{error E.bad=>42,_! =>0}}",
    );
}

#[test]
fn raw_any_success_is_never_reinterpreted_as_matching_error_payload() {
    answer(
        "enum E{bad};fn lift(value:Any)->!E E{value};fn main(){lift(E.bad) match{E.bad! =>42,error E.bad=>0}}",
    );
    answer(
        "enum E{bad};fn main(){let value:Any=E.bad;let lifted=value.as(!E E);lifted match{E.bad! =>42,error E.bad=>0}}",
    );
}

#[test]
fn generic_catch_and_case_scopes_keep_original_local_nominal_provenance_distinct() {
    let source = "enum E{bad};fn main(){let value:!E i64=error E.bad;let answer=value!{catch e=>{struct Local{n:i64};Local{n:40}.n},n! =>{struct Local{n:i64};Local{n:n}.n}};answer+2}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let pool = &compiled.type_pool;
    let declarations:Vec<_>=pool.identity_input().unwrap().declarations.iter().filter(|d|matches!(d.path.last(),Some(type_pool::IdentityPathSegment::Named(name))if name=="Local")).collect();
    assert_eq!(declarations.len(), 2);
    assert_ne!(declarations[0].path, declarations[1].path);
    assert_ne!(
        pool.stable_type_id(declarations[0].type_index).unwrap(),
        pool.stable_type_id(declarations[1].type_index).unwrap()
    );
    assert!(declarations.iter().any(|d| {
        d.path
            .iter()
            .any(|p| matches!(p, type_pool::IdentityPathSegment::Lexical { kind: 14, .. }))
    }));
    assert!(declarations.iter().any(|d| {
        d.path
            .iter()
            .any(|p| matches!(p, type_pool::IdentityPathSegment::Lexical { kind: 3, .. }))
    }));
    answer(source);
}

#[test]
fn qualified_parameter_field_defaults_and_initializer_provider_dependencies_are_real_values() {
    answer(
        "enum E{bad};struct P{value:!E i64=40};fn read(.value:!E i64=2)->i64{value!{E.* =>0}};fn main(){(P{}.value!{E.* =>0})+read()}",
    );
    answer(
        "enum E{bad};fn call(.callback:fn()->!E i64=||42)->!E i64{callback()};fn main(){call()!{E.* =>0}}",
    );
    answer(
        "enum E{bad};mod consumer{pub global value:!E i64=provider.read()};mod provider{pub global n:i64=42;pub fn read()->!E i64{n}};fn main(){consumer.value!{E.* =>0}}",
    );
}

#[test]
fn ordinary_error_matches_require_success_and_complete_error_coverage() {
    for source in [
        "enum E{bad};fn main(){let value:!E i64=error E.bad;value match{error E.bad=>42}}",
        "enum E{a,b};fn main(){let value:!E i64=42;value match{n! =>n,error E.a=>0}}",
        "enum E{bad};fn main(){let value:!E (i64,i64)=(20,22);value match{((a,b) as whole)! if whole.0==a=>a+b,error _=>0}}",
        "enum E{bad};fn dynamic(value:Any){error value};fn main(){dynamic(E.bad) match{_! =>0,error E.bad=>42}}",
    ] {
        rejected(source, "exhaust");
    }
    answer(
        "enum E{bad};fn dynamic(value:Any){error value};fn main(){dynamic(E.bad) match{_! =>0,error payload=>if type_of(payload)==E{42}else{0}}}",
    );
}
