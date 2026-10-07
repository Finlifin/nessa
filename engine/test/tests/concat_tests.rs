//! Concat uses checked source methods and preserves ordinary calls and effects.

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
fn strings_concat_infer_results_and_expose_checked_methods() {
    executes_42(&[
        "fn main(){let result=\"a\"++\"b\"++\"c\";if result==\"abc\" and type_of(result)==String and result.concat(\"d\")==\"abcd\" {42}else{0}}",
        "typealias Text=String\nfn main(){let text:Text=\"你好\";if text.len()==6 and (text++\"!\").len()==7 {42}else{0}}",
        "fn join(left:String,right:String){left++right}\nfn main(){let joined=join(\"a\",\"b\");if joined==\"ab\" and type_of(joined)==String {42}else{0}}",
    ]);
}

#[test]
fn custom_concat_checks_operand_and_preserves_arbitrary_result_type() {
    executes_42(&[
        "struct Prefix{x:i32}\nimpl Prefix{pub fn concat(self,other:i64)->i64{self.x+other}}\nfn main(){let p=Prefix{x:40};let narrow:i32=2;let result=p++narrow;if result==42 and type_of(result)==i64 and p.concat(2)==42 {42}else{0}}",
        "struct Prefix{x:i64}\nimpl Prefix{pub fn concat(self,other:i8)->i64{self.x+other}}\nfn main(){let p=Prefix{x:40};p++2}",
        "struct P{x:i64}\nfn main(){let p=P{x:40};let result=p++2;if type_of(result)==i64 {result}else{0}}\nimpl P{pub fn concat(self,other:i64){self.x+other}}",
        "struct P{x:i64}\nimpl P{pub fn concat(self,other:i64){self.x+other}}\nfn main(){let p=P{x:40};let result=p++2;if type_of(result)==i64 {result}else{0}}",
    ]);
}

#[test]
fn list_concat_returns_independent_wrapper_and_shares_nested_elements() {
    executes_42(&[
        "fn main(){let left=[];let right=[42];let result=left++right;if result.len()==1 and result(0)==42 and left.len()==0 and right.len()==1 {42}else{0}}",
        "fn main(){let nested=[40];let original=[nested,2];let combined=original++original;combined(1)=0;let shared:List=combined(0);shared(0)=42;let from_original:List=original(0);if combined.len()==4 and original.len()==2 and original(1)==2 and combined(3)==2 and from_original(0)==42 {42}else{0}}",
    ]);
}

#[test]
fn concat_snapshots_left_before_rhs_and_executes_each_side_once() {
    executes_42(&[
        "global trace:i64=0\nfn left()->String{trace=trace*10+1;\"a\"}\nfn right()->String{trace=trace*10+2;\"b\"}\nfn main(){let value=left()++right();if trace==12 and value==\"ab\" {42}else{0}}",
        "fn main(){var left:String=\"original\";let value=left++if true {left=\"changed\";\" suffix\"}else{\"\"};if value==\"original suffix\" and left==\"changed\" {42}else{0}}",
    ]);
}

#[test]
fn custom_concat_supports_private_internal_calls_and_initialization_dependencies() {
    executes_42(&[
        "struct P{}\nimpl P{private fn concat(self,other:String)->String{other};pub fn reveal(self)->String{self++\"answer\"}}\nfn main(){let p=P{};if p.reveal()==\"answer\" {42}else{0}}",
        "struct P{}\nglobal result:i64=P{}++2\nimpl P{const base:i64=40;pub fn concat(self,other:i64)->i64{base+other}}\nfn main(){result}",
        "global hook:i64=0\nstruct P{}\nimpl P{fn __init__(){hook=40};pub fn concat(self,other:i64)->i64{hook+other}}\nfn main(){let p=P{};p++2}",
    ]);
}

#[test]
fn concat_effects_preserve_receiver_and_multiple_continuation_branches() {
    executes_42(&[
        "effect pause(catch k)->i64\nstruct P{x:i64}\nimpl P{pub fn concat(self,other:i64)->i64{let resumed=pause()#;self.x+other+resumed}}\nfn work()->i64{let p=P{x:40};p++2}\nfn main(){let saved=work()# {pause(k)=>k};let a=saved(0);let b=saved(1);if a==42 and b==43 {42}else{0}}",
    ]);
}

#[test]
fn concat_static_signature_and_visibility_errors_are_rejected() {
    for (source, expected) in [
        ("fn main(){42++1}", "unknown member `concat`"),
        ("fn main(){Map()++Map()}", "unknown member `concat`"),
        ("fn main(){\"a\"++42}", "concat operand"),
        ("fn main(){\"a\".concat(42)}", "type mismatch"),
        ("fn main(){\"a\".concat()}", "expects"),
        ("fn main(){\"a\".len(1)}", "expects"),
        (
            "fn main(){let value:i64=\"a\"++\"b\";value}",
            "type mismatch",
        ),
        (
            "struct P{}\nimpl P{private fn concat(self,x:i64)->i64{x}}\nfn main(){let p=P{};p++42}",
            "not visible",
        ),
        (
            "struct P{}\nimpl P{pub fn concat(other:i64)->i64{other}}\nfn main(){let p=P{};p++42}",
            "self parameter",
        ),
        (
            "struct P{}\nimpl P{pub fn concat(self,a:i64,b:i64)->i64{a+b}}\nfn main(){let p=P{};p++42}",
            "one fixed operand",
        ),
        (
            "struct P{}\nimpl P{pub fn concat(self,.a:i64=42)->i64{a}}\nfn main(){let p=P{};p++42}",
            "one fixed operand",
        ),
        (
            "struct P{}\nimpl P{pub fn concat(self,...xs:List)->i64{42}}\nfn main(){let p=P{};p++42}",
            "one fixed operand",
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
fn any_concat_is_dynamic_and_bad_operands_fail_at_runtime() {
    executes_42(&[
        "fn main(){let left:Any=\"a\";let right:Any=\"b\";let result:String=left++right;if result==\"ab\" {42}else{0}}",
        "struct P{}\nimpl P{pub fn concat(self,other:i64)->i64{other}}\nfn main(){let left:Any=P{};let result:i64=left++42;result}",
    ]);
    for source in [
        "fn main(){let left:Any=\"a\";let right:Any=42;left++right}",
        "fn main(){let left:Any=42;left++\"a\"}",
        "fn main(){let right:Any=42;\"a\"++right}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
        let error = common::run_value(source).expect_err("invalid dynamic concat must fail");
        assert!(
            error.contains("TypeError") || error.contains("MethodNotFound"),
            "{source}: {error}"
        );
    }
}
