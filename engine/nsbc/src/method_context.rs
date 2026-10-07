//! Validate the complete lexical context table before dynamic code is loaded.

use std::collections::{BTreeMap, BTreeSet};

use crate::{ArtifactError, CompiledArtifact, Instruction, ScopeCoverage};

pub(crate) fn validate_method_contexts(artifact: &CompiledArtifact) -> Result<(), ArtifactError> {
    let output = &artifact.codegen_output;
    let dynamic = |opcode| output.scope_coverage.covers(opcode);
    let mut calls = BTreeSet::new();
    for function in &output.functions {
        for (pc, &word) in function.instructions.iter().enumerate() {
            // The main validator has already checked each instruction.
            if Instruction::decode(word).is_some_and(|instruction| dynamic(instruction.opcode)) {
                calls.insert((function.func_id.0, pc as u32));
            }
        }
    }
    let Some(contexts) = &output.method_call_scopes else {
        if output.scope_coverage == ScopeCoverage::CallsAndTypes
            || (!calls.is_empty() && !artifact.type_pool.scopes().is_empty())
        {
            return Err(ArtifactError::new(
                "method contexts",
                "missing complete method call contexts",
            ));
        }
        // Old metadata retains no lexical authority. Runtime closure calls can
        // still work, but unknown method access cannot be authorized.
        return Ok(());
    };
    let functions: BTreeMap<_, _> = output
        .functions
        .iter()
        .map(|function| (function.func_id.0, function))
        .collect();
    let mut seen = BTreeSet::new();
    for context in contexts {
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
        if !calls.contains(&key) {
            return Err(ArtifactError::new(
                &location,
                "context PC is not covered by this metadata version",
            ));
        }
        if !seen.insert(key) {
            return Err(ArtifactError::new(&location, "duplicate method context"));
        }
    }
    if let Some((function, pc)) = calls.difference(&seen).next() {
        return Err(ArtifactError::new(
            format!("function {function} PC {pc} method context"),
            if output.scope_coverage == ScopeCoverage::CallsAndTypes {
                "missing dynamic call or type query scope"
            } else {
                "missing dynamic call scope"
            },
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{CodegenOutput, CompiledFunction, FuncId, MethodCallScope, Opcode, Reg};
    use type_pool::{ScopeContext, TypeIndex, TypePool};

    use super::*;

    #[test]
    fn complete_contexts_cannot_be_downgraded_to_legacy_with_a_lexical_graph() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        }])
        .unwrap();
        let mut artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                scope_coverage: crate::ScopeCoverage::Calls,
                method_call_scopes: Some(vec![MethodCallScope {
                    func_id: FuncId(0),
                    pc: 0,
                    scope: 0,
                }]),
                functions: vec![CompiledFunction {
                    display_owner: None,
                    func_id: FuncId(0),
                    name: str_interner::intern("context_entry"),
                    instructions: vec![
                        Instruction::call_indirect(Reg(0), 0).encode(),
                        Instruction::return_unit().encode(),
                    ],
                    register_count: 1,
                    param_count: 0,
                    is_closure: false,
                    function_type: TypeIndex::INVALID,
                    abi: None,
                    safepoint_pcs: vec![],
                }],
                constants: vec![],
                globals: vec![],
            },
            type_pool: pool,
            entry: Some(FuncId(0)),
            builtin_abi_version: 1,
            builtins: vec![],
        };
        crate::validate_artifact(&artifact).unwrap();
        artifact.codegen_output.method_call_scopes = Some(vec![]);
        assert!(
            crate::validate_artifact(&artifact)
                .unwrap_err()
                .message
                .contains("missing dynamic call scope")
        );
        artifact.codegen_output.method_call_scopes = None;
        assert!(
            crate::validate_artifact(&artifact)
                .unwrap_err()
                .message
                .contains("missing complete method call contexts")
        );
        artifact.type_pool.install_scopes(vec![]).unwrap();
        crate::validate_artifact(&artifact).unwrap();
    }
    #[test]
    fn type_contexts_require_revision_three_and_complete_coverage() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        }])
        .unwrap();
        let mut artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                scope_coverage: ScopeCoverage::CallsAndTypes,
                method_call_scopes: Some(vec![MethodCallScope {
                    func_id: FuncId(0),
                    pc: 0,
                    scope: 0,
                }]),
                functions: vec![CompiledFunction {
                    display_owner: None,
                    func_id: FuncId(0),
                    name: str_interner::intern("type_context_entry"),
                    instructions: vec![
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
                    abi: None,
                    safepoint_pcs: vec![],
                }],
                constants: vec![],
                globals: vec![],
            },
            type_pool: pool,
            entry: Some(FuncId(0)),
            builtin_abi_version: 1,
            builtins: vec![],
        };
        crate::validate_artifact(&artifact).unwrap();
        artifact.codegen_output.scope_coverage = ScopeCoverage::Calls;
        assert!(
            crate::validate_artifact(&artifact)
                .unwrap_err()
                .message
                .contains("not covered")
        );
        artifact.codegen_output.method_call_scopes = Some(vec![]);
        crate::validate_artifact(&artifact).unwrap(); // Revision two never promised type contexts.
        artifact.codegen_output.scope_coverage = ScopeCoverage::CallsAndTypes;
        assert!(
            crate::validate_artifact(&artifact)
                .unwrap_err()
                .message
                .contains("type query scope")
        );
        artifact.codegen_output.method_call_scopes = None;
        assert!(crate::validate_artifact(&artifact).is_err());
    }
}
