//! Startup dependencies follow proofs preserved by immutable receiver aliases.
mod common;

#[test]
fn receiver_aliases_do_not_load_unselected_scoped_providers() {
    for body in [
        "let copy=self;copy.next()",
        "let first=self;let second=first;second.next()",
        "let copy:Self=self;copy.next()",
    ] {
        let source = format!(
            "struct P{{}};trait Source{{assoc Item:Type=i64;fn next(self)->Item;derive fn copy(self)->Item{{{body}}}}};mod api{{extend Source for P{{pub fn next(self)->i64{{storage.value}}}};pub global answer:i64=P{{}}.copy()}};mod unused{{pub global value:i64=api.answer;extend Source for P{{pub fn next(self)->i64{{value}}}}}};mod storage{{pub global value:i64=42}};fn main(){{api.answer}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42);
    }
}

#[test]
fn written_aliases_and_independent_self_parameters_keep_conservative_dependencies() {
    for (association, body) in [
        ("", "let copy=self;copy=other;copy.next()"),
        ("", "self=other;let copy=self;copy.next()"),
        ("", "self=other;self.next()"),
        ("assoc Item:Type=i64;", "let copy=other;copy.next()"),
    ] {
        let source = format!(
            "struct P{{}};trait Source{{{association}fn next(self)->i64;derive fn copy(self,other:Self)->i64{{{body}}}}};mod api{{extend Source for P{{pub fn next(self)->i64{{storage.value}}}};pub global answer:i64=P{{}}.copy(P{{}})}};mod unused{{pub global value:i64=api.answer;extend Source for P{{pub fn next(self)->i64{{value}}}}}};mod storage{{pub global value:i64=42}};fn main(){{api.answer}}"
        );
        let compiled = driver::Driver::new().compile(&source);
        assert!(compiled.has_errors, "{source}");
        assert!(
            compiled.diagnostics.iter().any(|diagnostic| diagnostic
                .message
                .contains("cyclic module value initialization dependency")),
            "{source}: {:?}",
            compiled.diagnostics
        );
        assert!(compiled.into_artifact().is_err());
    }
}
