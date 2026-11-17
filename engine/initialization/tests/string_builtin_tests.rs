//! String concatenation supports multi-MiB ExternalRc strings and rejects values
//! exceeding `Vm::MAX_STRING_BYTES` (`ObjectTooLarge`).

use initialization::Engine;
use interpreter::{Vm, VmError, VmResult};
use nsbc::{Constant, FuncId, Instruction, Reg};
use runtime::{FunctionCode, ids};
use type_pool::TypeIndex;

#[test]
fn concatenating_valid_strings_beyond_the_object_limit_is_a_runtime_error() {
    // Start with a 16 MiB + 1 byte ExternalRc string (32x above legacy 512 KiB limit)
    // and double it 6 times (`16 -> 32 -> 64 -> 128 -> 256 -> 512 -> >1024 MiB`):
    // the first 5 concatenations succeed inside the default 64 MiB MMTk heap via ExternalRc,
    // and the 6th cleanly fails with `VmError::ObjectTooLarge` before allocation.
    let seed_len = (Vm::MAX_STRING_BYTES / 64) + 1;
    let input = "x".repeat(seed_len);
    let mut engine = Engine::with_defaults();
    let constant = engine
        .vm_mut()
        .push_constant(&Constant::Str(input))
        .unwrap();
    let mut instrs = vec![Instruction::load_const(Reg(0), constant as u16)];
    for _ in 0..6 {
        instrs.push(Instruction::mov(Reg(1), Reg(0)));
        instrs.push(Instruction::call_builtin(ids::STR_CONCAT, 2));
    }
    instrs.push(Instruction::ret(Reg(0)));
    engine.vm_mut().add_function(FunctionCode {
        display_owner: None,
        abi: nsbc::FunctionAbi::value_only(0),
        func_id: FuncId(0),
        instructions: instrs.into_iter().map(Instruction::encode).collect(),
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
