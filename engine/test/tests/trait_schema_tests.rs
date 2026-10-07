//! New source artifacts derive dispatch layout from trait declarations.

mod common;

#[test]
fn inherited_slots_are_stable_across_reordered_scoped_implementations() {
    let source = "struct P{}\ntrait Base{fn base(self)->i64}\ntrait Child(Base){fn first(self)->i64;fn second(self)->i64}\nmod a{extend Base for P{pub fn base(self)->i64{1}};extend Child for P{pub fn second(self)->i64{20};pub fn first(self)->i64{19};pub fn base(self)->i64{3}};pub fn answer()->i64{let p=P{};p.first()+p.second()}}\nmod b{extend Base for P{pub fn base(self)->i64{1}};extend Child for P{pub fn first(self)->i64{2};pub fn second(self)->i64{1}};pub fn answer()->i64{let p=P{};p.first()+p.second()}}\nfn main(){a.answer()+b.answer()}";
    // Parent/child same-name methods remain ambiguous for ordinary direct name
    // lookup, so this fixture calls only distinct children. Vtable overriding is
    // checked separately in resolution's exact-entry tests.
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn missing_bootstrap_slots_and_ordering_parent_implementations_are_rejected() {
    for (source, expected) in [
        (
            "struct P{};impl Iterator for P{assoc Item:Type=i64};fn main(){42}",
            "next",
        ),
        (
            "struct P{};extend IntoIterator for P{assoc Iter:Type=P};fn main(){42}",
            "into_iter",
        ),
        (
            "struct P{};impl PartialOrd for P{pub fn cmp(self,other:Self)->i64{0}};fn main(){42}",
            "partial_cmp",
        ),
        (
            "struct P{};derive PartialOrd for P;fn main(){42}",
            "global equality parent implementation",
        ),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
}

#[test]
fn inherited_generated_comparison_slots_publish_the_same_real_function() {
    let source = "struct P{x:i64}\nderive Eq for P\ntrait Child(Eq){fn value(self)->i64}\nimpl Child for P{pub fn value(self)->i64{42}}\nfn main(){let p=P{x:1};let q=P{x:1};if p==q{p.value()}else{0}}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let tables = compiled.type_pool.vtables_snapshot();
    let child = tables
        .iter()
        .find(|table| {
            compiled.type_pool.get(table.trait_type).kind.trait_name()
                == Some(str_interner::intern("Child"))
        })
        .unwrap();
    let parent = compiled
        .type_pool
        .find_vtable(child.implementor, compiled.type_pool.well_known.eq)
        .unwrap();
    assert_eq!(child.entries[0], parent.entries[0]);
    assert_ne!(parent.entries[0], type_pool::DERIVE_FUNC_ID);
    assert!((parent.entries[0] as usize) < compiled.codegen_output.functions.len());
    compiled.type_pool.validate().unwrap();
    assert_eq!(common::run_value(source).unwrap(), 42);
}
