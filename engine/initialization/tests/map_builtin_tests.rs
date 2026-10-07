//! Raw CallBuiltin operands must honor Map contracts even without std adapters.

use initialization::Engine;
use interpreter::{VmError, VmResult};
use nsbc::{FuncId, Instruction, Reg};
use runtime::{FunctionCode, ids};
use type_pool::TypeIndex;

fn invoke(id: u32, count: u8) -> VmResult {
    let mut engine = Engine::with_defaults();
    engine.vm_mut().add_function(FunctionCode {
        display_owner: None,
        abi: None,
        func_id: FuncId(0),
        instructions: [
            Instruction::load_imm(Reg(0), 42),
            Instruction::call_builtin(id, count),
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
    engine.vm_mut().run()
}

#[test]
fn raw_native_calls_reject_wrong_map_argument_counts_before_reading_values() {
    for (id, expected, got) in [
        (ids::MAP_INIT, 0, 1),
        (ids::MAP_LEN, 1, 0),
        (ids::MAP_GET, 2, 1),
        (ids::MAP_SET, 3, 2),
        (ids::MAP_REMOVE, 2, 1),
        (ids::MAP_CONTAINS, 2, 1),
        (ids::MAP_KEYS, 1, 0),
        (ids::MAP_KEYS, 1, 2),
    ] {
        assert!(
            matches!(
                invoke(id, got),
                VmResult::Error(VmError::ArityError { expected: actual_expected, got: actual_got })
                    if actual_expected == expected && actual_got == got
            ),
            "native {id}"
        );
    }
}

#[test]
fn raw_native_map_calls_reject_scalar_receivers() {
    for (id, count) in [
        (ids::MAP_LEN, 1),
        (ids::MAP_GET, 2),
        (ids::MAP_SET, 3),
        (ids::MAP_REMOVE, 2),
        (ids::MAP_CONTAINS, 2),
        (ids::MAP_KEYS, 1),
    ] {
        assert!(
            matches!(invoke(id, count), VmResult::Error(VmError::TypeError)),
            "native {id}"
        );
    }
}
