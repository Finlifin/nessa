//! String concatenation must reject values beyond the managed payload limit.

use initialization::Engine;
use interpreter::{VmError, VmResult};
use nsbc::{Constant, FuncId, Instruction, Reg};
use runtime::{FunctionCode, ids};
use type_pool::TypeIndex;

#[test]
fn concatenating_valid_strings_beyond_the_object_limit_is_a_runtime_error() {
    // Strings have an eight-byte length prefix and a u16 payload-word count.
    let maximum_bytes = usize::from(u16::MAX) * 8 - 8;
    let input = "x".repeat(maximum_bytes / 2 + 1);
    let mut engine = Engine::with_defaults();
    let constant = engine
        .vm_mut()
        .push_constant(&Constant::Str(input))
        .unwrap();
    engine.vm_mut().add_function(FunctionCode {
        display_owner: None,
        abi: None,
        func_id: FuncId(0),
        instructions: [
            Instruction::load_const(Reg(0), constant as u16),
            Instruction::load_const(Reg(1), constant as u16),
            Instruction::call_builtin(ids::STR_CONCAT, 2),
            Instruction::ret(Reg(0)),
        ]
        .into_iter()
        .map(Instruction::encode)
        .collect(),
        register_count: 32,
        param_count: 0,
        function_type: TypeIndex::INVALID,
        is_closure: false,
    });
    engine.vm_mut().spawn_root(FuncId(0));
    assert!(matches!(
        engine.vm_mut().run(),
        VmResult::Error(VmError::ObjectTooLarge)
    ));
}
