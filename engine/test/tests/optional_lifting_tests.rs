//! Optional boundaries preserve null and convert the non-null numeric payload.

mod common;

#[test]
fn optional_numeric_lifting_preserves_concrete_type_and_null() {
    for source in [
        "fn main(){ let small:i32=42; let value:?i64=small; if value==42 and type_of(value)==i64 {42}else{0} }",
        "fn main(){ let small:?i32=42; let value:?i64=small; if value==42 and type_of(value)==i64 {42}else{0} }",
        "fn main(){ let small:?i32=null; let value:?i64=small; if value==null {42}else{0} }",
        "typealias Count=i64\ntypealias Maybe=?Count\nstruct P{x:Maybe}\nfn main(){ let small:i32=42; let value=P{x:small}; if value.x==42 and type_of(value.x)==i64 {42}else{0} }",
        "struct P{x:?i8}\nfn main(){ let value=P{x:42}; if value.x==42 and type_of(value.x)==i8 {42}else{0} }",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn optional_lifting_does_not_unwrap_or_narrow_implicitly() {
    for source in [
        "fn main(){ let value:?i64=42; let required:i64=value; required }",
        "fn main(){ let wide:?i64=42; let narrow:?i32=wide; narrow }",
        "struct P{x:?i8}\nfn main(){ P{x:128} }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}: expected rejection");
    }
}
