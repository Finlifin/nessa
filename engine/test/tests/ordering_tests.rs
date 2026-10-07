//! Ordering contracts execute ordinary trait implementations with frozen evidence.
mod common;

#[test]
fn scalar_ordering_preserves_exact_integer_and_scalar_values() {
    for ty in [
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    ] {
        let source = format!(
            "fn order(a:Ord,b:Ord)->Ordering{{a.cmp(b)}};fn main(){{let a:{ty}=41;let b:{ty}=42;if order(a,b)==Ordering.less and order(b,a)==Ordering.greater and order(a,a)==Ordering.equal{{42}}else{{0}}}}"
        );
        assert_eq!(
            common::run_value(&source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
    for source in [
        "fn order(a:Ord,b:Ord)->Ordering{a.cmp(b)};fn main(){let a:u128=18446744073709551616;let b:u128=18446744073709551617;if order(a,b)==Ordering.less{42}else{0}}",
        "fn order(a:Ord,b:Ord)->Ordering{a.cmp(b)};fn main(){if order(false,true)==Ordering.less and order('é','x')==Ordering.greater and order((),())==Ordering.equal and order(\"éx\",\"éy\")==Ordering.less{42}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn scalar_ord_defaults_work_through_trait_and_concrete_receivers() {
    let check = "fn check(a:Ord,b:Ord)->bool{a.lt(b) and not a.gt(b) and a.lte(b) and not a.gte(b) and not a.lt(a) and not a.gt(a) and a.lte(a) and a.gte(a)};";
    for ty in [
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    ] {
        let source = format!(
            "{check}fn main(){{let a:{ty}=41;let b:{ty}=42;if check(a,b) and a.lt(b) and b.gt(a) and a.lte(b) and b.gte(a){{42}}else{{0}}}}"
        );
        assert_eq!(
            common::run_value(&source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
    for (left, right) in [("false", "true"), ("'x'", "'é'"), ("\"ab\"", "\"ac\"")] {
        let source = format!(
            "{check}fn main(){{let a={left};let b={right};if check(a,b) and a.lt(b) and b.gt(a) and a.lte(b) and b.gte(a){{42}}else{{0}}}}"
        );
        assert_eq!(
            common::run_value(&source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
    let source = "fn check(a:Ord,b:Ord)->bool{not a.lt(b) and not a.gt(b) and a.lte(b) and a.gte(b)};fn main(){let a=();if check(a,a) and not a.lt(a) and not a.gt(a) and a.lte(a) and a.gte(a){42}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn ord_defaults_dispatch_overrides_derived_methods_and_frozen_parent_proofs() {
    for source in [
        "struct P{};derive Eq for P;impl Ord for P{pub fn cmp(self,other:Self)->Ordering{Ordering.less};pub fn lt(self,other:Self)->bool{false}};fn check(a:Ord,b:Ord)->bool{not a.lt(b) and a.gte(b) and not a.gt(b) and a.lte(b)};fn main(){let a=P{};let b=P{};if check(a,b) and not a.lt(b) and a.gte(b) and a.cmp(b)==Ordering.less{42}else{0}}",
        "struct P{x:i64};derive Eq,Ord for P;fn check(a:Ord,b:Ord)->bool{a.lt(b) and not a.gt(b) and a.lte(b) and not a.gte(b)};fn main(){let a=P{x:1};let b=P{x:2};if check(a,b) and a.lt(b) and b.gt(a) and a.lte(b) and b.gte(a){42}else{0}}",
        "struct P{x:i64};derive Eq for P;mod a{extend Ord for P{pub fn cmp(self,other:Self)->Ordering{Ordering.less}};pub fn answer()->i64{outside.consume(P{x:42},P{x:42})}};mod outside{extend Ord for P{pub fn cmp(self,other:Self)->Ordering{Ordering.greater}};pub fn consume(x:Ord,y:Ord)->i64{if x.lt(y) and not x.gt(y) and x.lte(y) and not x.gte(y) and x.eq(y){42}else{0}}};fn main(){a.answer()}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn float_partial_ordering_is_unordered_for_nan_and_equal_for_signed_zero() {
    for ty in ["f32", "f64"] {
        let source = format!(
            "fn order(a:PartialOrd,b:PartialOrd)->?Ordering{{a.partial_cmp(b)}};fn main(){{let a:{ty}=1.5;let b:{ty}=2.5;let zero:{ty}=0.0;let negative:{ty}=-0.0;if order(a,b)==Ordering.less and order(zero,negative)==Ordering.equal and a<b and a<=b and b>a and b>=a{{42}}else{{0}}}}"
        );
        assert_eq!(
            common::run_value(&source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
    let source = "fn order(a:PartialOrd,b:PartialOrd)->?Ordering{a.partial_cmp(b)};fn main(){let nan:f64=sqrt(-1.0);let n:f64=1.0;if order(nan,n)==null and not(nan<n) and not(nan<=n) and not(nan>n) and not(nan>=n) and not(n<nan) and not(n<=nan) and not(n>nan) and not(n>=nan){42}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn ordering_self_arguments_reject_different_concrete_types() {
    for source in [
        "fn order(a:Ord,b:Ord)->Ordering{a.cmp(b)};fn main(){let a:i64=42;let b:u64=42;order(a,b);42}",
        "fn order(a:PartialOrd,b:PartialOrd)->?Ordering{a.partial_cmp(b)};fn main(){let a:f64=42.0;let b:f32=42.0;order(a,b);42}",
        "struct P{};struct Q{};derive Eq,Ord for P;derive Eq,Ord for Q;fn compare(a:Ord,b:Ord)->bool{a.lt(b)};fn main(){compare(P{},Q{});42}",
        "fn compare(a:Ord,b:Ord)->bool{a.gte(b)};fn main(){let a:i64=42;let b:u64=42;compare(a,b);42}",
        "fn order(a:Ord,b:Ord)->Ordering{a.cmp(b)};fn main(){let x:Any=i64;order(x,x);42}",
        "fn order(a:Ord,b:Ord)->Ordering{a.cmp(b)};fn main(){let x:Any=||42;order(x,x);42}",
    ] {
        let result = common::run_value(source);
        assert!(
            matches!(result, Err(error) if error.contains("InvalidTraitProof") || error.contains("TypeError")),
            "{source}"
        );
    }
}

#[test]
fn custom_ordering_methods_return_enum_results_and_keep_source_effects() {
    for source in [
        "struct P{n:i64};derive Eq,PartialEq for P;global calls:i64=0;impl Ord for P{pub fn cmp(self,other:Self)->Ordering{calls=calls+1;if self.n<other.n{Ordering.less}else{if self.n==other.n{Ordering.equal}else{Ordering.greater}}}};fn main(){let a=P{n:1};let b=P{n:2};if a<b and not(b<a) and (true or a>b) and calls==2{42}else{0}}",
        "struct P{};derive PartialEq for P;impl PartialOrd for P{pub fn partial_cmp(self,other:Self)->?Ordering{null}};fn main(){let a=P{};if not(a<a) and not(a<=a) and not(a>a) and not(a>=a){42}else{0}}",
        "struct P{};derive Eq for P;effect pause(catch k)->i64;impl Ord for P{pub fn cmp(self,other:Self)->Ordering{pause()#;Ordering.less}};fn compare(a:Ord,b:Ord)->bool{a<b};fn main(){let saved=compare(P{},P{})#{pause(k)=>k};if saved(0) and saved(1){42}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn derived_ordering_compares_struct_enum_tuple_and_optional_fields() {
    for source in [
        "struct P{a:i64,b:i64};derive Eq,Ord for P;fn less(a:Ord,b:Ord)->bool{a<b};fn main(){let a=P{a:1,b:99};let b=P{a:2,b:0};if less(a,b) and a.cmp(a)==Ordering.equal{42}else{0}}",
        "enum E{first(x:i64),second(x:i64)};derive Eq,Ord for E;fn less(a:Ord,b:Ord)->bool{a<b};fn main(){if less(E.first(99),E.second(0)) and less(E.first(1),E.first(2)){42}else{0}}",
        "typealias Pair=(i64,i64);derive Eq,Ord for Pair;fn less(a:Ord,b:Ord)->bool{a<b};fn main(){let a:Pair=(1,99);let b:Pair=(2,0);if less(a,b) and a.cmp(a)==Ordering.equal{42}else{0}}",
        "struct P{x:?i64};derive Eq,Ord for P;fn main(){let a=P{x:null};let b=P{x:1};if a<b and not(b<a) and a.cmp(a)==Ordering.equal{42}else{0}}",
        "struct P{x:f64};derive PartialEq,PartialOrd for P;fn less(a:PartialOrd,b:PartialOrd)->bool{a<b};fn main(){let a=P{x:sqrt(-1.0)};let b=P{x:1.0};if a.partial_cmp(b)==null and not less(a,b){42}else{0}}",
        "struct Inner{n:i64};derive Eq for Inner;global calls:i64=0;impl Ord for Inner{pub fn cmp(self,other:Self)->Ordering{calls=calls+1;Ordering.less}};struct Outer{a:Inner,b:Inner};derive Eq,Ord for Outer;fn main(){let x=Outer{a:Inner{n:99},b:Inner{n:99}};let y=Outer{a:Inner{n:0},b:Inner{n:0}};if x<y and calls==1{42}else{0}}",
        "struct Inner{n:i64};derive PartialEq for Inner;global calls:i64=0;impl PartialOrd for Inner{pub fn partial_cmp(self,other:Self)->?Ordering{calls=calls+1;null}};struct Outer{a:Inner,b:Inner};derive PartialEq,PartialOrd for Outer;fn main(){let x=Outer{a:Inner{n:99},b:Inner{n:99}};let y=Outer{a:Inner{n:0},b:Inner{n:0}};if x.partial_cmp(y)==null and calls==1{42}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

fn collect_at_print(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

fn collect_scalar_cmp(
    context: &mut interpreter::BuiltinCtx<'_>,
) -> Result<(), interpreter::VmError> {
    context.require_arity(2)?;
    let left = context.arg(0)?;
    let right = context.arg(1)?;
    assert!(context.collect_garbage()?);
    match context.scalar_partial_cmp(left, right)? {
        Some(std::cmp::Ordering::Less) => context.return_i64(-1),
        Some(std::cmp::Ordering::Equal) => context.return_i64(0),
        Some(std::cmp::Ordering::Greater) => context.return_i64(1),
        None => context.set_return(runtime::TaggedValue::NULL),
    }
}

#[test]
fn real_gc_preserves_ordering_closure_proofs_and_multishot_continuations() {
    for source in [
        "fn capture(a:Ord,b:Ord)->fn()->Ordering{||a.cmp(b)};fn main(){let f=capture(\"abcd\"++\"efgh\",\"abcd\"++\"efgi\");print(\"\");if f()==Ordering.less{42}else{0}}",
        "effect pause(catch k)->i64;fn compare(a:Ord,b:Ord)->Ordering{pause()#;a.cmp(b)};fn main(){let saved=compare(\"abcd\"++\"efgh\",\"abcd\"++\"efgi\")#{pause(k)=>k};print(\"\");if saved(0)==Ordering.less and saved(1)==Ordering.less{42}else{0}}",
        "struct P{text:String};derive Eq for P;mod a{extend Ord for P{pub fn cmp(self,other:Self)->Ordering{print(\"\");if self.text<other.text{Ordering.less}else{Ordering.greater}}};pub fn capture()->fn()->Ordering{let x:Ord=P{text:\"abcd\"++\"efgh\"};let y:Ord=P{text:\"abcd\"++\"efgi\"};||x.cmp(y)}};mod b{extend Ord for P{pub fn cmp(self,other:Self)->Ordering{Ordering.greater}};pub fn run()->i64{let f=a.capture();print(\"\");if f()==Ordering.less{42}else{0}}};fn main(){b.run()}",
        "struct P{text:String};derive Eq for P;effect pause(catch k)->i64;impl Ord for P{pub fn cmp(self,other:Self)->Ordering{pause()#;print(\"\");if self.text<other.text{Ordering.less}else{Ordering.greater}}};fn compare(a:Ord,b:Ord)->bool{a.lt(b)};fn main(){let saved=compare(P{text:\"abcd\"++\"efgh\"},P{text:\"abcd\"++\"efgi\"})#{pause(k)=>k};print(\"\");if saved(0) and saved(1){42}else{0}}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(!compiled.has_errors, "{source}: {:?}", compiled.diagnostics);
        let mut engine = initialization::Engine::with_defaults();
        let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
            .unwrap()
            .unwrap();
        engine
            .vm_mut()
            .register_builtin(runtime::ids::PRINT, collect_at_print);
        engine
            .vm_mut()
            .register_builtin(runtime::ids::SCALAR_CMP, collect_scalar_cmp);
        let before = engine.vm_mut().completed_collections();
        let task = engine.vm_mut().spawn_root(entry);
        assert!(
            matches!(engine.vm_mut().run(), interpreter::VmResult::Finished),
            "{source}"
        );
        assert!(engine.vm_mut().completed_collections() > before);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
