//! Schema installation and checked publication of module globals.

use nsbc::GlobalInfo;
use runtime::{GlobalError, TaggedValue};

use crate::{Vm, VmError};

impl Vm {
    /// Install the artifact's global schema once, before executing initializers.
    ///
    /// # Errors
    /// Rejects invalid types, unsupported index capacities, repeated schema
    /// installation, allocation failure, or a cross-VM operation.
    pub fn initialize_globals(&mut self, schema: &[GlobalInfo]) -> Result<(), VmError> {
        let _operation = self.execution_operation()?;
        if schema.len() > (1 << 17) {
            return Err(VmError::ObjectTooLarge);
        }
        for global in schema {
            if self
                .state
                .type_pool
                .canonical_type(global.type_index)
                .is_none()
            {
                return Err(VmError::InvalidType(global.type_index));
            }
        }
        self.roots.globals.initialize(schema).map_err(VmError::from)
    }

    pub(super) fn store_global(&mut self, index: u32, value: TaggedValue) -> Result<(), VmError> {
        let info = self.roots.globals.check_store(index)?;
        let value = self.assert_value_type(value, info.type_index)?;
        self.roots.globals.set(index, value).map_err(VmError::from)
    }
}

impl From<GlobalError> for VmError {
    fn from(error: GlobalError) -> Self {
        match error {
            GlobalError::InvalidIndex(index) => Self::InvalidGlobal(index),
            GlobalError::Uninitialized(index) => Self::UninitializedGlobal(index),
            GlobalError::Immutable(index) => Self::ImmutableGlobal(index),
            GlobalError::SchemaAlreadyInitialized => Self::GlobalsAlreadyInitialized,
            GlobalError::OutOfMemory => Self::OutOfMemory,
        }
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{AddrMode, FuncId, Instruction, Opcode, Reg};
    use runtime::FunctionCode;
    use type_pool::{Intrinsic, TypeIndex};

    use super::*;
    use crate::VmResult;
    use crate::tests::make_vm;

    fn run_code(vm: &mut Vm, instructions: &[Instruction]) -> VmResult {
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            instructions: instructions
                .iter()
                .map(|instruction| instruction.encode())
                .collect(),
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        vm.run()
    }

    #[test]
    fn bytecode_reports_invalid_and_uninitialized_global_reads() {
        for (index, initialized_schema) in [(0u16, true), (4095, false)] {
            let mut vm = make_vm();
            if initialized_schema {
                vm.initialize_globals(&[GlobalInfo {
                    type_index: Intrinsic::Any.type_index(),
                    is_mutable: false,
                }])
                .unwrap();
            }
            let result = run_code(
                &mut vm,
                &[
                    Instruction::a_type(Opcode::LoadGlobal, AddrMode::Imm, Reg(0), Reg(0), index),
                    Instruction::ret(Reg(0)),
                ],
            );
            if initialized_schema {
                assert!(matches!(
                    result,
                    VmResult::Error(VmError::UninitializedGlobal(0))
                ));
            } else {
                assert!(matches!(
                    result,
                    VmResult::Error(VmError::InvalidGlobal(4095))
                ));
            }
        }
    }

    #[test]
    fn const_initialization_rejects_a_second_store_and_keeps_the_first_value() {
        let mut vm = make_vm();
        vm.initialize_globals(&[GlobalInfo {
            type_index: Intrinsic::I64.type_index(),
            is_mutable: false,
        }])
        .unwrap();
        let result = run_code(
            &mut vm,
            &[
                Instruction::load_imm(Reg(1), 42),
                Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(1), 0),
                Instruction::load_imm(Reg(1), 99),
                Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(1), 0),
                Instruction::ret(Reg(1)),
            ],
        );
        assert!(matches!(
            result,
            VmResult::Error(VmError::ImmutableGlobal(0))
        ));
        assert_eq!(vm.roots.globals.get(0).unwrap().as_i64(), Some(42));
    }

    #[test]
    fn malformed_wide_global_reads_and_stores_return_index_errors() {
        for opcode in [Opcode::LoadGlobalWide, Opcode::StoreGlobalWide] {
            let mut vm = make_vm();
            vm.initialize_globals(&[]).unwrap();
            let result = run_code(
                &mut vm,
                &[
                    Instruction::load_imm(Reg(1), 42),
                    Instruction::a_type(opcode, AddrMode::Imm, Reg(1), Reg(16), 0),
                    Instruction::ret(Reg(1)),
                ],
            );
            assert!(matches!(
                result,
                VmResult::Error(VmError::InvalidGlobal(65536))
            ));
        }
    }

    #[test]
    fn stores_validate_types_without_publishing_failed_initializers() {
        let mut vm = make_vm();
        vm.initialize_globals(&[GlobalInfo {
            type_index: Intrinsic::I64.type_index(),
            is_mutable: false,
        }])
        .unwrap();
        assert!(matches!(
            vm.store_global(0, TaggedValue::TRUE),
            Err(VmError::TypeError)
        ));
        assert_eq!(vm.roots.globals.get(0), Err(GlobalError::Uninitialized(0)));
        assert!(matches!(
            vm.store_global(u32::MAX, TaggedValue::TRUE),
            Err(VmError::InvalidGlobal(u32::MAX))
        ));
        vm.store_global(0, TaggedValue::from_i64(42)).unwrap();
        assert_eq!(vm.roots.globals.get(0).unwrap().as_i64(), Some(42));
        assert!(matches!(
            vm.initialize_globals(&[]),
            Err(VmError::GlobalsAlreadyInitialized)
        ));
    }

    #[test]
    fn wide_globals_preserve_high_index_bits_and_the_source_register() {
        let index = (1 << 17) - 1;
        let mut vm = make_vm();
        vm.initialize_globals(&vec![
            GlobalInfo {
                type_index: Intrinsic::I64.type_index(),
                is_mutable: true
            };
            index + 1
        ])
        .unwrap();
        let result = run_code(
            &mut vm,
            &[
                Instruction::load_imm(Reg(17), 42),
                Instruction::a_type(
                    Opcode::StoreGlobalWide,
                    AddrMode::Imm,
                    Reg(17),
                    Reg(31),
                    4095,
                ),
                Instruction::a_type(
                    Opcode::LoadGlobalWide,
                    AddrMode::Imm,
                    Reg(18),
                    Reg(31),
                    4095,
                ),
                Instruction::ret(Reg(18)),
            ],
        );
        assert!(matches!(result, VmResult::Finished));
        assert_eq!(
            vm.roots.globals.get(index as u32).unwrap().as_i64(),
            Some(42)
        );
        assert_eq!(
            vm.roots.globals.get(4095),
            Err(GlobalError::Uninitialized(4095))
        );
    }

    #[test]
    fn global_schema_rejects_invalid_types_before_installation() {
        let mut vm = make_vm();
        assert!(matches!(
            vm.initialize_globals(&[GlobalInfo {
                type_index: TypeIndex::INVALID,
                is_mutable: true,
            }]),
            Err(VmError::InvalidType(TypeIndex::INVALID))
        ));
        vm.initialize_globals(&[]).unwrap();
    }

    #[test]
    fn a_capturing_closure_and_its_caller_share_the_same_global_binding() {
        let mut vm = make_vm();
        vm.initialize_globals(&[GlobalInfo {
            type_index: Intrinsic::I64.type_index(),
            is_mutable: true,
        }])
        .unwrap();
        let signature = vm
            .state
            .type_pool
            .intern_structural(type_pool::TypeKind::Function {
                params: vec![],
                ret: Intrinsic::I64.type_index(),
            });
        let main = [
            Instruction::load_imm(Reg(0), 41),
            Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(0), 0),
            Instruction::load_imm(Reg(0), 1),
            Instruction::new_closure(Reg(1), 1, 1),
            Instruction::call_indirect(Reg(1), 0),
            Instruction::a_type(Opcode::LoadGlobal, AddrMode::Imm, Reg(2), Reg(0), 0),
            Instruction::ret(Reg(2)),
        ];
        let closure = [
            Instruction::a_type(Opcode::LoadGlobal, AddrMode::Imm, Reg(1), Reg(0), 0),
            Instruction::r_type(Opcode::Add, Reg(2), Reg(0), Reg(1)),
            Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(2), 0),
            Instruction::ret(Reg(2)),
        ];
        for (id, code, is_closure, param_count) in [
            (0, main.as_slice(), false, 0),
            (1, closure.as_slice(), true, 1),
        ] {
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(id),
                instructions: code
                    .iter()
                    .map(|instruction| instruction.encode())
                    .collect(),
                register_count: 3,
                param_count,
                is_closure,
                function_type: if is_closure {
                    signature
                } else {
                    TypeIndex::INVALID
                },
            });
        }
        let task = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Finished));
        assert_eq!(vm.task_result_i64(task).unwrap(), 42);
        assert_eq!(vm.roots.globals.get(0).unwrap().as_i64(), Some(42));
    }
}
