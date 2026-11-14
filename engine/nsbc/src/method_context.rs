//! Validate the complete lexical context table before dynamic code is loaded.

use std::collections::{BTreeMap, BTreeSet};

use crate::{ArtifactError, CompiledArtifact, Instruction, instruction_requires_scope};

pub(crate) fn validate_method_contexts(artifact: &CompiledArtifact) -> Result<(), ArtifactError> {
    let output = &artifact.codegen_output;
    let mut covered = BTreeSet::new();
    for function in &output.functions {
        for (pc, &word) in function.instructions.iter().enumerate() {
            // The main validator has already checked each instruction.
            if Instruction::decode(word)
                .is_some_and(|instruction| instruction_requires_scope(instruction.opcode))
            {
                covered.insert((function.func_id.0, pc as u32));
            }
        }
    }
    let functions: BTreeMap<_, _> = output
        .functions
        .iter()
        .map(|function| (function.func_id.0, function))
        .collect();
    let mut seen = BTreeSet::new();
    for context in &output.method_call_scopes {
        let location = format!(
            "function {} PC {} method context",
            context.func_id.0, context.pc
        );
        if artifact.type_pool.scope_context(context.scope).is_none() {
            return Err(ArtifactError::new(
                &location,
                "method scope is out of range",
            ));
        }
        let function = functions.get(&context.func_id.0).ok_or_else(|| {
            ArtifactError::new(&location, "method context function does not exist")
        })?;
        if function.instructions.get(context.pc as usize).is_none() {
            return Err(ArtifactError::new(
                &location,
                "method context PC is out of range",
            ));
        }
        let key = (context.func_id.0, context.pc);
        if !covered.contains(&key) {
            return Err(ArtifactError::new(
                &location,
                "context PC is not covered by scope metadata",
            ));
        }
        if !seen.insert(key) {
            return Err(ArtifactError::new(&location, "duplicate method context"));
        }
    }
    if let Some((function, pc)) = covered.difference(&seen).next() {
        return Err(ArtifactError::new(
            format!("function {function} PC {pc} method context"),
            "missing dynamic call or type query scope",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{CodegenOutput, CompiledFunction, FuncId, FunctionAbi, MethodCallScope, Opcode, Reg};
    use type_pool::{ScopeContext, TypeIndex, TypePool};

    use super::*;

    #[test]
    fn call_and_type_instructions_require_complete_checked_scope_coverage() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        }])
        .unwrap();
        let mut artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                method_call_scopes: vec![
                    MethodCallScope {
                        func_id: FuncId(0),
                        pc: 0,
                        scope: 0,
                    },
                    MethodCallScope {
                        func_id: FuncId(0),
                        pc: 1,
                        scope: 0,
                    },
                ],
                functions: vec![CompiledFunction {
                    display_owner: None,
                    func_id: FuncId(0),
                    name: str_interner::intern("context_entry"),
                    instructions: vec![
                        Instruction::call_indirect(Reg(0), 0).encode(),
                        Instruction::a_type(
                            Opcode::TypeCheck,
                            crate::AddrMode::Imm,
                            Reg(0),
                            Reg(0),
                            type_pool::Intrinsic::I64.type_index().as_u32() as u16,
                        )
                        .encode(),
                        Instruction::return_unit().encode(),
                    ],
                    register_count: 1,
                    param_count: 0,
                    is_closure: false,
                    function_type: TypeIndex::INVALID,
                    abi: FunctionAbi::default(),
                    safepoint_pcs: vec![],
                }],
                constants: vec![],
                globals: vec![],
            },
            type_pool: pool,
            entry: Some(FuncId(0)),
            builtin_abi_version: 9,
            builtins: vec![],
        };
        crate::validate_artifact(&artifact).unwrap();

        artifact.codegen_output.method_call_scopes.pop();
        assert!(
            crate::validate_artifact(&artifact)
                .unwrap_err()
                .message
                .contains("missing dynamic call or type query scope")
        );

        artifact.codegen_output.method_call_scopes.push(MethodCallScope {
            func_id: FuncId(0),
            pc: 2,
            scope: 0,
        });
        assert!(
            crate::validate_artifact(&artifact)
                .unwrap_err()
                .message
                .contains("not covered")
        );

        artifact.codegen_output.method_call_scopes[1] = MethodCallScope {
            func_id: FuncId(0),
            pc: 1,
            scope: 9,
        };
        assert!(
            crate::validate_artifact(&artifact)
                .unwrap_err()
                .message
                .contains("out of range")
        );
    }
}
