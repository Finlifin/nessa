//! Trait interface signatures retain source Self and reject incompatible implementations.

mod common;

#[test]
fn compatible_methods_keep_static_receivers_and_aliases() {
    for source in [
        "struct P{x:i64}\ntrait Read{fn value(self)->i64}\nimpl Read for P{pub fn value(self)->i64{self.x}}\nfn read(x:Read)->i64{x.value()}\nfn main(){read(P{x:42})}",
        "struct P{}\ntrait Factory{fn zero()->i64}\nimpl Factory for P{pub fn zero()->i64{42}}\nfn main(){P.zero()}",
        "struct P{x:i64}\ntrait Clone{fn clone(self)->Self}\nimpl Clone for P{pub fn clone(self)->Self{self}}\nfn main(){let p=P{x:42};p.clone().x}",
        "struct Holder{typealias This=Self}\nstruct P{}\ntrait Keep{fn value(self,other:Holder.This)->i64}\nimpl Keep for P{pub fn value(self,other:Holder)->i64{42}}\nfn main(){P{}.value(Holder{})}",
        "struct P{}\ntrait Mix{fn value(self,other:Self,read:Mix)->i64}\nimpl Mix for P{pub fn value(self,other:Self,read:Mix)->i64{42}}\nfn main(){let p=P{};p.value(p,p)}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn interface_mismatches_are_rejected_before_bytecode_generation() {
    for source in [
        "struct P{};trait Read{fn value(self)->i64};impl Read for P{fn value(self)->String{\"bad\"}};fn main(){42}",
        "struct P{};trait Read{fn value(self,x:i64)->i64};impl Read for P{fn value(self,x:Any)->i64{42}};fn main(){42}",
        "struct P{};trait Read{fn value(self,x:i64)->i64};impl Read for P{fn value(self)->i64{42}};fn main(){42}",
        "struct P{};trait Read{fn value(self,x:i64)->i64};impl Read for P{fn value(x:P,y:i64)->i64{42}};fn main(){42}",
        "struct P{};trait Read{fn value(self,other:Read)->i64};impl Read for P{fn value(self,other:Self)->i64{42}};fn main(){42}",
        "struct P{};trait Read{fn value(self,other:Self)->i64};impl Read for P{fn value(self,other:Read)->i64{42}};fn main(){42}",
        "struct P{};trait Read{fn value(self,other:?Self)->i64};impl Read for P{fn value(self,other:?Read)->i64{42}};fn main(){42}",
        "struct P{};trait Read{fn value(self,.x:i64=1)->i64};impl Read for P{fn value(self,x:i64)->i64{42}};fn main(){42}",
        "trait Base{fn value(self)->i64};trait Child(Base){};struct P{};impl Base for P{fn value(self)->i64{42}};impl Child for P{fn value(self)->String{\"bad\"}};fn main(){42}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled.codegen_output.functions.is_empty(),
            "emitted invalid {source}"
        );
        assert!(
            compiled.diagnostics.iter().any(|diagnostic| diagnostic
                .message
                .contains("signature mismatch")
                || diagnostic.message.contains("parameter kinds")),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
}
