//! Dynamic Maps keep String keys and Any values while preserving evaluation order.

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
fn empty_maps_aliases_and_missing_keys_keep_one_nominal_type() {
    executes_42(&[
        "typealias Entries=Map\nfn main(){let m:Entries=Map();let n=Map.new();if m.len()==0 and n.len()==0 and not m.contains(\"missing\") and m.get(\"missing\")==null and m.remove(\"missing\")==null and type_of(m)==Map and type_of(n)==Entries {42}else{0}}",
        "fn main(){let m=Map();m(\"answer\")=42;let dynamic:Any=m;if dynamic(\"answer\")==42 and dynamic.len()==1 {42}else{0}}",
    ]);
}

#[test]
fn equal_string_contents_replace_values_through_shared_aliases() {
    executes_42(&[
        "fn main(){let m=Map();let key=std.string.str_concat(\"ans\",\"wer\");m(key)=41;let alias:Map=m;alias.set(\"answer\",42);if m.len()==1 and m(\"answer\")==42 and m.contains(std.string.str_concat(\"an\",\"swer\")) and m.remove(\"answer\")==42 and alias.len()==0 {42}else{0}}",
        "fn main(){let m=Map();m.set(\"nothing\",null);if m.contains(\"nothing\") and m(\"nothing\")==null and m.len()==1 {m.remove(\"nothing\");if not m.contains(\"nothing\") and m.len()==0 {42}else{0}}else{0}}",
    ]);
}

#[test]
fn map_values_include_nested_collections_closures_and_scalars() {
    executes_42(&[
        "fn answer()->i64{42}\nfn main(){let m=Map();let nested=Map();nested(\"n\")=42;m(\"map\")=nested;m(\"list\")=[42,true,null];m(\"fn\")=answer;m(\"text\")=\"stored\";let recovered:Map=m(\"map\");let list:List=m(\"list\");let f:fn()->i64=m(\"fn\");if recovered(\"n\")==42 and list(0)==42 and f()==42 and m.get(\"text\")==\"stored\" {42}else{0}}",
    ]);
}

#[test]
fn indexing_evaluates_receiver_key_and_value_once_in_source_order() {
    executes_42(&[
        "global trace:i64=0\nglobal m:Map=Map()\nfn receiver()->Map{trace=trace*10+1;m}\nfn key()->String{trace=trace*10+2;\"answer\"}\nfn value()->i64{trace=trace*10+3;42}\nfn main(){receiver()(key())=value();if trace==123 and m(\"answer\")==42 {42}else{0}}",
        "fn main(){let left=Map();let right=Map();var selected:Map=left;selected(\"answer\")=if true {selected=right;42}else{0};if left.contains(\"answer\") and not right.contains(\"answer\") {42}else{0}}",
        "fn main(){let m=Map();var key:String=\"original\";m(key)=if true {key=\"changed\";42}else{0};if m.contains(\"original\") and not m.contains(\"changed\") {42}else{0}}",
        "fn main(){let left=Map();let right=Map();left(\"answer\")=42;right(\"answer\")=0;var selected:Map=left;let value=selected(if true {selected=right;\"answer\"}else{\"missing\"});if value==42 and selected(\"answer\")==0 {42}else{0}}",
    ]);
}

#[test]
fn invalid_keys_arity_and_storage_access_are_compile_errors() {
    for (tail, expected) in [
        ("let m=Map();m(42)", "Map index must have type String"),
        ("let m=Map();m(true)=42", "Map index must have type String"),
        ("Map()(null)", "Map index must have type String"),
        ("Map()()", "exactly one String"),
        ("Map()(\"a\",\"b\")", "exactly one String"),
        ("Map()(key=\"a\")", "positional index"),
        ("Map().get(42)", "type mismatch"),
        ("Map().set(42,true)", "type mismatch"),
        ("Map().contains(false)", "type mismatch"),
        ("Map().remove(42)", "type mismatch"),
        ("Map().capacity", "unknown Map member"),
        ("Map{}", "collection storage"),
        ("let m:Map={}", "anonymous Object"),
        ("let f=Map().get;f(\"key\")", "bound Map method values"),
        ("let m=Map();m(\"key\")+=1", "compound assignment target"),
    ] {
        let source = format!("fn main(){{{tail}}}");
        let result = driver::Driver::new().compile(&source);
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
fn gradual_keys_are_checked_at_runtime() {
    for source in [
        "fn main(){let m=Map();let key:Any=42;m(key)}",
        "fn main(){let m=Map();let key:Any=42;m(key)=true}",
        "fn main(){let m:Any=Map();m(42)}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
        let error = common::run_value(source).expect_err("non-String dynamic key must fail");
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}

#[test]
fn maps_grow_reinsert_and_survive_multiple_continuation_resumes() {
    executes_42(&[
        "fn main(){let m=Map();var i:i64=0;while i<200 {m(to_string(i))=i;i=i+1};i=0;while i<200 {if m.remove(to_string(i))!=i {return 0};i=i+2};i=0;while i<200 {m(to_string(i))=i+1;i=i+2};if m.len()==200 and m(\"198\")==199 and m(\"199\")==199 {42}else{0}}",
        "effect pause(catch k)->i64\nfn work()->i64{let m=Map();m(\"answer\")=42;pause()#;m(\"answer\").as(i64)}\nfn main(){let saved=work()# {pause(k)=>k};let a=saved(0);let b=saved(1);if a==42 and b==42 {42}else{0}}",
    ]);
}
