//! Derived comparisons execute as ordinary functions, preserving nested calls and effects.

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
fn derived_struct_and_tuple_alias_comparisons_check_all_components() {
    executes_42(&[
        "struct P { x:i64, label:String }\nderive Eq for P\nfn main(){ let a=P{x:42,label:\"same\"}; let b=P{x:42,label:\"same\"}; let c=P{x:41,label:\"same\"}; let d=P{x:42,label:\"other\"}; if a.eq(b) and not a.eq(c) and not a.eq(d) and a==b and a!=c {42}else{0} }",
        "typealias Pair = (i64, String)\nderive Eq for Pair\nfn main(){ let a:Pair=(42,\"same\"); let b:Pair=(42,\"same\"); let c:Pair=(42,\"other\"); if a.eq(b) and not a.eq(c) and a==b and a!=c {42}else{0} }",
        "struct P { pair:(i64,String), maybe:?i64 }\nderive Eq for P\nfn main(){ let a=P{pair:(42,\"same\"),maybe:null}; let b=P{pair:(42,\"same\"),maybe:null}; let c=P{pair:(42,\"same\"),maybe:42}; if a.eq(b) and not a.eq(c) and c.eq(P{pair:(42,\"same\"),maybe:42}) {42}else{0} }",
        "struct P { value:i64 }\nderive PartialEq for P\nfn main(){ let a=P{value:42}; let b=P{value:42}; let c=P{value:0}; if a.eq(b) and not a.eq(c) and a==b and a!=c {42}else{0} }",
    ]);
}

#[test]
fn derived_enum_comparisons_check_tags_payloads_and_recursive_values() {
    executes_42(&[
        "enum E { none, some(value:i64), pair(a:i64,b:String) }\nderive Eq for E\nfn main(){ if E.none.eq(E.none) and not E.none.eq(E.some(42)) and E.some(42).eq(E.some(42)) and not E.some(42).eq(E.some(0)) and E.pair(42,\"x\").eq(E.pair(42,\"x\")) and not E.pair(42,\"x\").eq(E.pair(42,\"y\")) and E.none==E.none and E.none!=E.some(42) and E.some(42)==E.some(42) and E.some(42)!=E.some(0) {42}else{0} }",
        "enum Tree { leaf(value:i64), node(left:Tree,right:Tree) }\nderive Eq for Tree\nfn main(){ let a=Tree.node(Tree.leaf(40),Tree.leaf(2)); let b=Tree.node(Tree.leaf(40),Tree.leaf(2)); let c=Tree.node(Tree.leaf(40),Tree.leaf(3)); if a.eq(b) and not a.eq(c) and not a.eq(Tree.leaf(42)) and a==b and a!=c {42}else{0} }",
        "enum E { none, some(value:(i64,String)) }\nderive PartialEq for E\nfn main(){ if E.some((42,\"same\")).eq(E.some((42,\"same\"))) and not E.some((42,\"same\")).eq(E.some((41,\"same\"))) {42}else{0} }",
    ]);
}

#[test]
fn nested_user_eq_side_effects_execute_once_and_short_circuit() {
    executes_42(&[
        "global calls:i64=0\nstruct User { value:i64 }\nimpl Eq for User { fn eq(self,other:User)->bool { calls=calls+1; self.value==other.value } }\nstruct Pair { first:User, last:User }\nderive Eq for Pair\nfn main(){ let a=Pair{first:User{value:1},last:User{value:9}}; let b=Pair{first:User{value:2},last:User{value:9}}; if not a.eq(b) and calls==1 {42}else{0} }",
        "global calls:i64=0\nstruct User { value:i64 }\nimpl Eq for User { fn eq(self,other:User)->bool { calls=calls+1; self.value==other.value } }\nstruct Pair { first:User, last:User }\nderive Eq for Pair\nfn main(){ let a=Pair{first:User{value:1},last:User{value:9}}; let b=Pair{first:User{value:1},last:User{value:9}}; if a.eq(b) and calls==2 {42}else{0} }",
    ]);
}

#[test]
fn optional_enum_and_struct_fields_compare_null_and_payload_values() {
    executes_42(&[
        "enum E { none, some(value:i64) }\nderive Eq for E\nstruct P { value:?E }\nderive Eq for P\nfn main(){ let a=P{value:null}; let b=P{value:E.some(42)}; let c=P{value:E.some(42)}; let d=P{value:E.some(0)}; let e=P{value:null}; if a==e and a!=b and b!=a and b==c and b!=d {42}else{0} }",
        "struct Value { value:i64 }\nderive Eq for Value\nstruct P { value:?Value }\nderive Eq for P\nfn main(){ let a=P{value:null}; let b=P{value:Value{value:42}}; let c=P{value:Value{value:42}}; let d=P{value:Value{value:0}}; if a.eq(P{value:null}) and not a.eq(b) and not b.eq(a) and b==c and b!=d {42}else{0} }",
    ]);
}

#[test]
fn nested_comparisons_select_the_requested_trait_and_tuple_override() {
    executes_42(&[
        "struct User { value:i64 }\nimpl Eq for User { fn eq(self,other:User)->bool { true } }\nimpl PartialEq for User { fn eq(self,other:User)->bool { false } }\nstruct E { value:User }\nderive Eq for E\nstruct P { value:User }\nderive PartialEq for P\nfn main(){ let a=E{value:User{value:1}}; let b=E{value:User{value:2}}; let c=P{value:User{value:1}}; let d=P{value:User{value:1}}; if a==b and c!=d {42}else{0} }",
        "typealias Pair=(i64,i64)\nimpl Eq for Pair { fn eq(self,other:Pair)->bool { true } }\nstruct P { value:Pair }\nderive Eq for P\nfn main(){ let a=P{value:(1,2)}; let b=P{value:(3,4)}; if a==b {42}else{0} }",
    ]);
}

#[test]
fn derived_parent_eq_preserves_effect_pause_and_user_method_frames() {
    executes_42(&[
        "effect pause(catch k)->i64\nglobal calls:i64=0\nstruct User { value:i64 }\nimpl Eq for User { fn eq(self,other:User)->bool { calls=calls+1; let gate=pause()#; self.value==other.value and gate==1 } }\nstruct P { value:User }\nderive Eq for P\nfn main(){ let a=P{value:User{value:42}}; let b=P{value:User{value:42}}; let saved=a.eq(b)# { pause(k)=>k }; let equal=saved(1); if equal and calls==1 {42}else{0} }",
    ]);
}

#[test]
fn missing_component_traits_and_unimplemented_hash_are_rejected() {
    for (source, expected) in [
        (
            "struct NoEq { value:i64 }\nstruct Box { value:NoEq }\nderive Eq for Box",
            "comparison",
        ),
        (
            "enum NoEq { item(value:i64) }\nstruct Box { value:NoEq }\nderive Eq for Box",
            "comparison",
        ),
        ("struct P { value:Any }\nderive Eq for P", "comparison"),
        ("struct P { value:i64 }\nderive Hash for P", "Hash"),
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
