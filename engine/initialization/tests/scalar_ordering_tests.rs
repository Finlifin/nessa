//! The internal comparison primitive checks native arity and operand categories.

use initialization::Engine;
use interpreter::{VmError, VmResult};
use nsbc::{FuncId, Instruction, Reg};
use runtime::{FunctionCode, ids};
use type_pool::TypeIndex;

#[test]
fn scalar_ordering_native_rejects_invalid_arity_and_operand_categories() {
    for (instructions, expected_arity) in [
        (vec![Instruction::call_builtin(ids::SCALAR_CMP, 0)], Some(0)),
        (vec![Instruction::call_builtin(ids::SCALAR_CMP, 1)], Some(1)),
        (vec![Instruction::call_builtin(ids::SCALAR_CMP, 3)], Some(3)),
        (
            vec![
                Instruction::load_null(Reg(0)),
                Instruction::load_null(Reg(1)),
                Instruction::call_builtin(ids::SCALAR_CMP, 2),
            ],
            None,
        ),
    ] {
        let mut engine = Engine::with_defaults();
        engine.vm_mut().add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            instructions: instructions.into_iter().map(Instruction::encode).collect(),
            register_count: 32,
            param_count: 0,
            function_type: TypeIndex::INVALID,
            is_closure: false,
        });
        engine.vm_mut().spawn_root(FuncId(0));
        match expected_arity {
            Some(got) => assert!(
                matches!(engine.vm_mut().run(), VmResult::Error(VmError::ArityError { expected: 2, got: actual }) if actual == got)
            ),
            None => assert!(matches!(
                engine.vm_mut().run(),
                VmResult::Error(VmError::TypeError)
            )),
        }
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
