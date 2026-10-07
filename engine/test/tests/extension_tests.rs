//! Source extensions bind Self while keeping methods inside their lexical range.

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
fn self_and_self_type_use_the_canonical_owner_in_plain_and_trait_extensions() {
    executes_42(&[
        "struct P{x:i64}\nextend P{pub fn identity(self)->Self{self};pub fn value(self)->i64{self.x}}\nfn main(){let p=P{x:42};p.identity().value()}",
        "struct P{x:i64}\ntrait Read{fn value(self)->i64}\nextend Read for P{pub fn value(self)->i64{self.x}}\nfn main(){let p=P{x:42};p.value()}",
        "struct P{private x:i64}\nextend P{pub fn new(value:i64)->Self{P{x:value}};pub fn value(self)->i64{self.x}}\nfn main(){P.new(42).value()}",
    ]);
}

#[test]
fn equally_named_methods_in_disjoint_module_and_block_scopes_are_independent() {
    executes_42(&[
        "struct P{}\nmod a{extend P{pub fn value(self)->i64{40}};pub fn answer()->i64{let p=P{};p.value()}}\nmod b{extend P{pub fn value(self)->i64{2}};pub fn answer()->i64{let p=P{};p.value()}}\nfn main(){a.answer()+b.answer()}",
        "struct P{}\nfn main(){let p=P{};let a=if true{extend P{pub fn value(self)->i64{40}};p.value()}else{0};let b=if true{extend P{pub fn value(self)->i64{2}};p.value()}else{0};a+b}",
    ]);
}

#[test]
fn imported_aliases_and_private_helpers_keep_the_declaration_scope() {
    executes_42(&[
        "mod model{pub struct P{x:i64}}\nuse model.P as Item\ntypealias Alias=Item\nextend Alias{private fn base(self)->i64{self.x};pub fn answer(self)->i64{self.base()+2}}\nfn main(){let p=Item{x:40};p.answer()}",
        "struct P{}\nmod a{extend P{private fn value(self)->i64{0}}}\nextend P{pub fn value(self)->i64{42}}\nfn main(){let p=P{};p.value()}",
    ]);
}

#[test]
fn extension_apply_update_concat_and_optional_defaults_use_checked_source_bindings() {
    executes_42(&[
        "struct P{x:i64}\nextend P{pub fn apply(self,.delta:i64=2)->i64{self.x+delta}}\nfn main(){let p=P{x:40};p()}",
        "struct P{x:i64}\nextend P{pub fn apply(self,index:i64)->i64{self.x+index};pub fn update(self,index:i64,value:i64){self.x=value-index}}\nfn main(){let p=P{x:0};p(2)=42;p(2)}",
        "struct P{x:i64}\nextend P{pub fn concat(self,other:i64)->i64{self.x+other}}\nfn main(){let p=P{x:40};p++2}",
        "struct P{}\nglobal result:i64=P{}++2\nmod storage{pub global base:i64=40}\nextend P{pub fn concat(self,other:i64)->i64{storage.base+other}}\nfn main(){result}",
    ]);
}

#[test]
fn extension_frames_support_closure_capture_and_multishot_effects() {
    executes_42(&[
        "effect pause(catch k)->i64\nstruct P{x:i64}\nextend P{pub fn apply(self,delta:i64)->i64{let resumed=pause()#;self.x+delta+resumed}}\nfn work()->i64{let p=P{x:40};p(2)}\nfn main(){let saved=work()# {pause(k)=>k};let a=saved(0);let b=saved(1);if a==42 and b==43 {42}else{0}}",
        "struct P{x:i64}\nextend P{pub fn make(self)->fn()->i64{||self.x}}\nfn main(){let p=P{x:42};let f=p.make();f()}",
        "struct P{}\nfn main(){let p=P{};let f=if true{extend P{pub fn value(self)->i64{42}};||p.value()}else{||0};f()}",
    ]);
}

#[test]
fn invisible_ambiguous_and_duplicate_extensions_are_rejected() {
    for (source, expected) in [
        (
            "struct P{}\nfn main(){let x:i64=42;extend P{pub fn value(self)->i64{x}};let p=P{};p.value()}",
            "cannot capture",
        ),
        (
            "struct P{}\nmod a{extend P{pub fn value(self)->i64{42}}}\nfn main(){let p=P{};p.value()}",
            "visible",
        ),
        (
            "struct P{}\nfn main(){let p=P{};let a=if true{extend P{pub fn value(self)->i64{42}};p.value()}else{0};p.value()}",
            "visible",
        ),
        (
            "struct P{}\nextend P{private fn value(self)->i64{42}}\nfn main(){let p=P{};p.value()}",
            "visible",
        ),
        (
            "struct P{}\nimpl P{pub fn value(self)->i64{1}}\nextend P{pub fn value(self)->i64{42}}\nfn main(){let p=P{};p.value()}",
            "ambiguous",
        ),
        (
            "struct P{}\nextend P{pub fn value(self)->i64{1}}\nextend P{pub fn value(self)->i64{42}}",
            "duplicate",
        ),
        (
            "struct P{}\nfn main(){extend P{pub fn value(self)->i64{1}};extend P{pub fn value(self)->i64{42}};42}",
            "duplicate",
        ),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}");
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
fn same_scope_forward_reads_keep_source_initialization_order() {
    let source = "struct P{}\nglobal result:i64=P{}++2\nglobal base:i64=40\nextend P{pub fn concat(self,other:i64)->i64{base+other}}\nfn main(){result}";
    assert!(
        matches!(common::run_value(source), Err(error) if error.contains("UninitializedGlobal"))
    );
}
