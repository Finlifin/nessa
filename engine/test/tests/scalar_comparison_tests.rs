//! Same-type scalar comparisons use the native kernel shared with the standard
//! Eq/Ord implementations instead of calling through them.

mod common;

#[test]
fn scalar_comparisons_keep_their_values() {
    let source = r#"
        fn main() {
            let big: u64 = 18446744073709551615
            let mid: u64 = 9223372036854775808
            let results = [big > mid, "abc" < "abd", "b" >= "abc", 'a' < 'b', false < true, () == (), -3 < 2, -3 <= -3, 1.5 > 1.25, 2.0 != 2.0, "x" == "x", 7 >= 8]
            var bits = 0
            for result in results { bits = bits * 2 + if result.as(bool) { 1 } else { 0 } }
            bits
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 0b1111_1111_1010);
}

#[test]
fn same_type_integer_comparison_emits_no_call() {
    let compiled =
        driver::Driver::new().compile("fn main() { var i = 0; while i < 16000 { i = i + 1 }; i }");
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let main = str_interner::intern("main");
    let opcodes: Vec<_> = artifact
        .codegen_output
        .functions
        .iter()
        .find(|function| function.name == main)
        .unwrap()
        .instructions
        .iter()
        .map(|&word| nsbc::Instruction::decode(word).unwrap().opcode)
        .collect();
    assert!(opcodes.contains(&nsbc::Opcode::CmpLt));
    assert!(!opcodes.contains(&nsbc::Opcode::Call));
}
