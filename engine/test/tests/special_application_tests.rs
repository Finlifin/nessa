//! Instance apply/update sugar retains declaration binding and source evaluation order.

mod common;

fn executes_42(sources: &[&str]) {
    for source in sources {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn known_apply_checks_types_and_binds_named_defaults_and_variadic_values() {
    executes_42(&[
        "struct P{base:i64}\nimpl P{pub fn apply(self,x:i64,.scale:i64=self.base)->i64{x*scale}}\nfn main(){let p=P{base:2};if p(21)==42 and p(x=14,scale=3)==42 {42}else{0}}",
        "struct P{}\nimpl P{pub fn apply(self,...xs:List)->i64{xs(0).as(i64)+xs(1).as(i64)}}\nfn main(){let p=P{};p(40,2)}",
        "struct P{}\nimpl P{pub fn apply(self)->String{\"answer\"}}\nfn main(){let p=P{};if p()==\"answer\" {42}else{0}}",
        "enum E{zero,pub fn apply(self,x:i64)->i64{x}}\nfn main(){E.zero(42)}",
        "struct P{}\nfn main(){let p=P{};p(42)}\nimpl P{pub fn apply(self,x:i64){x}}",
        "struct P{}\nimpl P{pub fn apply(self,(a,b):(i64,i64),.extra:i64=a+b)->i64{extra}}\nfn main(){let p=P{};p((40,2))}",
    ]);
}

#[test]
fn update_does_not_require_apply_and_discards_its_return_value() {
    executes_42(&[
        "struct P{value:i64}\nimpl P{pub fn update(self,index:i64,value:i64,.scale:i64=2)->i64{self.value=index+value*scale;self.value}}\nfn main(){let p=P{value:0};p(index=0,scale=1)=42;p.value}",
        "struct P{values:List}\nimpl P{pub fn update(self,...xs:List){self.values=xs}}\nfn main(){let p=P{values:[]};p(40,1)=2;if p.values.len()==3 and p.values(2)==2 {p.values(0).as(i64)+p.values(2).as(i64)}else{0}}",
    ]);
}

#[test]
fn receivers_arguments_rhs_and_defaults_have_source_ordered_snapshots() {
    executes_42(&[
        "global trace:i64=0\nstruct P{value:i64}\nglobal p:P=P{value:0}\nfn receiver()->P{trace=trace*10+1;p}\nfn index()->i64{trace=trace*10+2;0}\nfn value()->i64{trace=trace*10+3;42}\nfn factor()->i64{trace=trace*10+4;1}\nimpl P{pub fn update(self,index:i64,value:i64,.scale:i64=factor()){trace=trace*10+5;self.value=index+value*scale}}\nfn main(){receiver()(index())=value();if trace==12345 and p.value==42 {42}else{0}}",
        "struct P{value:i64}\nimpl P{pub fn update(self,index:i64,value:i64){self.value=index+value}}\nfn main(){let left=P{value:0};let right=P{value:0};var selected:P=left;var index:i64=40;selected(index)=if true {selected=right;index=0;2}else{0};if left.value==42 and right.value==0 {42}else{0}}",
        "global trace:i64=0\nfn first()->i64{trace=trace*10+1;40}\nfn second()->i64{trace=trace*10+2;2}\nstruct P{}\nimpl P{pub fn apply(self,a:i64,b:i64)->i64{a+b}}\nfn main(){let p=P{};let result=p(b=second(),a=first());if trace==21 {result}else{0}}",
    ]);
}

#[test]
fn application_effects_and_method_initialization_dependencies_are_preserved() {
    executes_42(&[
        "effect pause(catch k)->i64\nstruct P{base:i64}\nimpl P{pub fn apply(self,x:i64)->i64{let resumed=pause()#;self.base+x+resumed}}\nfn work()->i64{let p=P{base:40};p(2)}\nfn main(){let saved=work()# {pause(k)=>k};let a=saved(0);let b=saved(1);if a==42 and b==43 {42}else{0}}",
        "struct P{}\nglobal result:i64=P{}(2)\nimpl P{const base:i64=40;pub fn apply(self,x:i64)->i64{base+x}}\nfn main(){result}",
        "global hook:i64=0\nstruct P{value:i64}\nimpl P{fn __init__(){hook=40};pub fn update(self,x:i64,value:i64){self.value=hook+x+value}}\nfn main(){let p=P{value:0};p(0)=2;p.value}",
    ]);
}

#[test]
fn missing_methods_bad_signatures_and_bad_bindings_are_compile_errors() {
    for (source, expected) in [
        (
            "struct P{}\nfn main(){let p=P{};p(42)}",
            "unknown member `apply`",
        ),
        (
            "struct P{}\nfn main(){let p=P{};p(0)=42}",
            "unknown member `update`",
        ),
        (
            "struct P{}\nimpl P{fn apply(x:i64)->i64{x}}\nfn main(){let p=P{};p(42)}",
            "self parameter",
        ),
        (
            "struct P{}\nimpl P{private fn apply(self,x:i64)->i64{x}}\nfn main(){let p=P{};p(42)}",
            "not visible",
        ),
        (
            "struct P{}\nimpl P{pub fn apply(self,x:i64)->i64{x}}\nfn main(){let p=P{};p(true)}",
            "application argument",
        ),
        (
            "struct P{}\nimpl P{pub fn apply(self,x:i64)->i64{x}}\nfn main(){let p=P{};p()}",
            "missing required parameter",
        ),
        (
            "struct P{}\nimpl P{pub fn apply(self,x:i64)->i64{x}}\nfn main(){let p=P{};p(x=42,x=0)}",
            "more than once",
        ),
        (
            "struct P{}\nimpl P{pub fn update(self,index:i64,value:String){}}\nfn main(){let p=P{};p(0)=42}",
            "application argument",
        ),
        (
            "fn f(x:i64)->i64{x}\nfn main(){f(0)=42}",
            "call assignment requires an instance",
        ),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}: expected rejection");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn function_closure_collection_and_dynamic_application_paths_remain_valid() {
    executes_42(&[
        "struct P{value:i64}\nimpl P{pub fn apply(self,x:i64)->i64{self.value+x};pub fn update(self,x:i64,value:i64){self.value=x+value}}\nfn main(){let p:Any=P{value:40};let first=p(2);p(0)=42;if first==42 and p(0)==42 {42}else{0}}",
        "fn answer(x:i64)->i64{x}\nfn main(){let f:fn(i64)->i64=answer;let closure=|x:i64|->i64{x};let xs=[40];xs(0)=42;let m=Map();m(\"x\")=42;if f(42)==42 and closure(42)==42 and xs(0)==42 {m(\"x\").as(i64)}else{0}}",
    ]);
}
