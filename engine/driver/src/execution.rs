//! Install validated source or archive artifacts into a fresh VM.

use std::fmt;

use interpreter::{Vm, VmError};
use nsbc::{CompiledArtifact, FuncId};
use runtime::FunctionCode;

#[derive(Debug)]
pub enum ArtifactLoadError {
    InvalidMetadata(nsbc::ArtifactError),
    IncompatibleBuiltin(String),
    Runtime(VmError),
}

impl fmt::Display for ArtifactLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMetadata(error) => write!(f, "invalid artifact: {error}"),
            Self::IncompatibleBuiltin(message) => write!(f, "incompatible builtin ABI: {message}"),
            Self::Runtime(error) => write!(f, "artifact installation failed: {error:?}"),
        }
    }
}

impl std::error::Error for ArtifactLoadError {}

/// Install an artifact into a fresh VM before spawning any tasks.
///
/// Metadata and native imports are checked before any VM state is changed.
/// The VM must have the standard builtin implementations registered. A failed
/// installation leaves the VM unsuitable for reusing with another artifact.
pub fn install_artifact(
    vm: &mut Vm,
    artifact: CompiledArtifact,
) -> Result<Option<FuncId>, ArtifactLoadError> {
    nsbc::validate_artifact(&artifact).map_err(ArtifactLoadError::InvalidMetadata)?;
    let compatible_legacy = match artifact.builtin_abi_version {
        1 => artifact
            .builtins
            .iter()
            .all(|import| import.id < runtime::ids::LIST_INIT),
        2 => artifact
            .builtins
            .iter()
            .all(|import| import.id < runtime::ids::MAP_INIT),
        3 => artifact
            .builtins
            .iter()
            .all(|import| import.id < runtime::ids::DERIVED_DISPLAY),
        4 => artifact
            .builtins
            .iter()
            .all(|import| import.id < runtime::ids::SCALAR_EQ),
        5 => artifact
            .builtins
            .iter()
            .all(|import| import.id < runtime::ids::SCALAR_CMP),
        6 => artifact
            .builtins
            .iter()
            .all(|import| import.id < runtime::ids::DISPLAY_QUOTE),
        7 => artifact
            .builtins
            .iter()
            .all(|import| import.id < runtime::ids::MAP_KEYS),
        _ => false,
    };
    if artifact.builtin_abi_version != runtime::BUILTIN_ABI_VERSION && !compatible_legacy {
        return Err(ArtifactLoadError::IncompatibleBuiltin(format!(
            "revision {}, expected {}",
            artifact.builtin_abi_version,
            runtime::BUILTIN_ABI_VERSION,
        )));
    }
    for import in &artifact.builtins {
        if runtime::lookup_builtin_fn_meta(import.id).map(|metadata| metadata.name)
            != Some(import.name.as_str())
        {
            return Err(ArtifactLoadError::IncompatibleBuiltin(format!(
                "unknown ID/name pair {} ({})",
                import.id, import.name,
            )));
        }
    }

    if artifact.builtins.iter().any(|import| {
        (runtime::ids::LIST_INIT..=runtime::ids::LIST_POP).contains(&import.id)
            || import.id == runtime::ids::MAP_KEYS
    }) && (artifact.type_pool.list_type().is_none()
        || artifact.type_pool.list_buffer_type().is_none())
    {
        return Err(ArtifactLoadError::InvalidMetadata(
            nsbc::ArtifactError::new(
                "native imports",
                "List imports require List and Buffer roles",
            ),
        ));
    }

    if artifact.builtins.iter().any(|import| {
        (runtime::ids::MAP_INIT..=runtime::ids::MAP_CONTAINS).contains(&import.id)
            || import.id == runtime::ids::MAP_KEYS
    }) && (artifact.type_pool.map_type().is_none()
        || artifact.type_pool.map_buffer_type().is_none())
    {
        return Err(ArtifactLoadError::InvalidMetadata(
            nsbc::ArtifactError::new(
                "native imports",
                "Map imports require Map and MapBuffer roles",
            ),
        ));
    }

    vm.install_type_pool(artifact.type_pool);
    vm.initialize_globals(&artifact.codegen_output.globals)
        .map_err(ArtifactLoadError::Runtime)?;
    for function in artifact.codegen_output.functions {
        vm.add_function(FunctionCode {
            display_owner: function.display_owner,
            abi: function.abi,
            func_id: function.func_id,
            instructions: function.instructions,
            register_count: function.register_count,
            param_count: function.param_count,
            is_closure: function.is_closure,
            function_type: function.function_type,
        });
    }
    if let Some(contexts) = artifact.codegen_output.method_call_scopes {
        vm.install_method_call_scopes(contexts)
            .map_err(ArtifactLoadError::Runtime)?;
    }
    for constant in artifact.codegen_output.constants {
        vm.push_constant(&constant)
            .map_err(ArtifactLoadError::Runtime)?;
    }
    Ok(artifact.entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_list_imports_require_collection_roles_before_vm_installation() {
        let make = |pool| CompiledArtifact {
            codegen_output: nsbc::CodegenOutput {
                scope_coverage: nsbc::ScopeCoverage::Calls,
                method_call_scopes: None,
                functions: vec![],
                constants: vec![],
                globals: vec![],
            },
            type_pool: pool,
            entry: None,
            builtin_abi_version: runtime::BUILTIN_ABI_VERSION,
            builtins: vec![nsbc::BuiltinImport {
                id: runtime::ids::LIST_INIT,
                name: "__list_init".into(),
            }],
        };
        let pool = type_pool::TypePool::with_intrinsics();
        let mut snapshot = pool.snapshot();
        snapshot.types.truncate(snapshot.types.len() - 4);
        snapshot.methods.truncate(snapshot.methods.len() - 4);
        let legacy_pool = type_pool::TypePool::restore(snapshot).unwrap();
        let mut engine = initialization::Engine::with_defaults();
        let error = install_artifact(engine.vm_mut(), make(legacy_pool)).unwrap_err();
        assert!(error.to_string().contains("require List and Buffer roles"));
        assert!(install_artifact(engine.vm_mut(), make(pool)).is_ok());
    }

    #[test]
    fn legacy_abi_accepts_unchanged_imports_but_rejects_collection_and_new_ids() {
        let legacy = || {
            let mut snapshot = type_pool::TypePool::with_intrinsics().snapshot();
            snapshot.types.truncate(snapshot.types.len() - 4);
            snapshot.methods.truncate(snapshot.methods.len() - 4);
            CompiledArtifact {
                codegen_output: nsbc::CodegenOutput {
                    scope_coverage: nsbc::ScopeCoverage::Calls,
                    method_call_scopes: None,
                    functions: vec![],
                    constants: vec![],
                    globals: vec![],
                },
                type_pool: type_pool::TypePool::restore(snapshot).unwrap(),
                entry: None,
                builtin_abi_version: 1,
                builtins: vec![],
            }
        };
        let mut artifact = legacy();
        artifact.builtins.push(nsbc::BuiltinImport {
            id: runtime::ids::TO_I64,
            name: "to_i64".into(),
        });
        let mut engine = initialization::Engine::with_defaults();
        assert!(install_artifact(engine.vm_mut(), artifact).is_ok());
        for id in [runtime::ids::LIST_INIT, runtime::ids::LIST_LEN, 106] {
            let mut artifact = legacy();
            artifact.builtins.push(nsbc::BuiltinImport {
                id,
                name: runtime::lookup_builtin_fn_meta(id)
                    .map_or("unknown", |meta| meta.name)
                    .into(),
            });
            let mut engine = initialization::Engine::with_defaults();
            assert!(matches!(
                install_artifact(engine.vm_mut(), artifact),
                Err(ArtifactLoadError::IncompatibleBuiltin(_))
            ));
        }
    }
    #[test]
    fn abi_three_retains_old_imports_but_cannot_claim_native_display() {
        let make = |revision, id| CompiledArtifact {
            codegen_output: nsbc::CodegenOutput {
                scope_coverage: nsbc::ScopeCoverage::Calls,
                method_call_scopes: None,
                functions: vec![],
                constants: vec![],
                globals: vec![],
            },
            type_pool: type_pool::TypePool::with_intrinsics(),
            entry: None,
            builtin_abi_version: revision,
            builtins: vec![nsbc::BuiltinImport {
                id,
                name: runtime::lookup_builtin_fn_meta(id).unwrap().name.into(),
            }],
        };
        let mut engine = initialization::Engine::with_defaults();
        assert!(install_artifact(engine.vm_mut(), make(3, runtime::ids::MAP_CONTAINS)).is_ok());
        for revision in [1, 2, 3] {
            let mut engine = initialization::Engine::with_defaults();
            assert!(matches!(
                install_artifact(
                    engine.vm_mut(),
                    make(revision, runtime::ids::DERIVED_DISPLAY)
                ),
                Err(ArtifactLoadError::IncompatibleBuiltin(_))
            ));
        }
        let mut engine = initialization::Engine::with_defaults();
        assert!(
            install_artifact(
                engine.vm_mut(),
                make(runtime::BUILTIN_ABI_VERSION, runtime::ids::DERIVED_DISPLAY)
            )
            .is_ok()
        );
        let mut engine = initialization::Engine::with_defaults();
        assert!(install_artifact(engine.vm_mut(), make(4, runtime::ids::DERIVED_DISPLAY)).is_ok());
        for revision in [1, 2, 3, 4] {
            let mut engine = initialization::Engine::with_defaults();
            assert!(matches!(
                install_artifact(engine.vm_mut(), make(revision, runtime::ids::SCALAR_EQ)),
                Err(ArtifactLoadError::IncompatibleBuiltin(_))
            ));
        }
        let mut engine = initialization::Engine::with_defaults();
        assert!(
            install_artifact(
                engine.vm_mut(),
                make(runtime::BUILTIN_ABI_VERSION, runtime::ids::SCALAR_EQ)
            )
            .is_ok()
        );
        let mut bad = make(runtime::BUILTIN_ABI_VERSION, runtime::ids::DERIVED_DISPLAY);
        bad.builtins[0].name = "to_string".into();
        let mut engine = initialization::Engine::with_defaults();
        assert!(matches!(
            install_artifact(engine.vm_mut(), bad),
            Err(ArtifactLoadError::IncompatibleBuiltin(_))
        ));
    }

    #[test]
    fn map_key_snapshots_require_new_capability_and_both_collection_roles() {
        let make = |revision, id| CompiledArtifact {
            codegen_output: nsbc::CodegenOutput {
                scope_coverage: nsbc::ScopeCoverage::Calls,
                method_call_scopes: None,
                functions: vec![],
                constants: vec![],
                globals: vec![],
            },
            type_pool: type_pool::TypePool::with_intrinsics(),
            entry: None,
            builtin_abi_version: revision,
            builtins: vec![nsbc::BuiltinImport {
                id,
                name: runtime::lookup_builtin_fn_meta(id).unwrap().name.into(),
            }],
        };
        for revision in 1..=7 {
            let mut engine = initialization::Engine::with_defaults();
            assert!(matches!(
                install_artifact(engine.vm_mut(), make(revision, runtime::ids::MAP_KEYS)),
                Err(ArtifactLoadError::IncompatibleBuiltin(_))
            ));
        }
        for (revision, id) in [
            (3, runtime::ids::MAP_CONTAINS),
            (7, runtime::ids::DISPLAY_EXIT_TUPLE),
            (8, runtime::ids::MAP_KEYS),
        ] {
            let mut engine = initialization::Engine::with_defaults();
            assert!(install_artifact(engine.vm_mut(), make(revision, id)).is_ok());
        }
        for (roles, message) in [
            (
                [
                    type_pool::CollectionRole::List,
                    type_pool::CollectionRole::Buffer,
                ],
                "List imports require",
            ),
            (
                [
                    type_pool::CollectionRole::Map,
                    type_pool::CollectionRole::MapBuffer,
                ],
                "Map imports require",
            ),
        ] {
            let mut artifact = make(8, runtime::ids::MAP_KEYS);
            let mut snapshot = artifact.type_pool.snapshot();
            let mut indices: Vec<_> = snapshot
                .types
                .iter()
                .enumerate()
                .filter_map(|(index, info)| {
                    roles
                        .iter()
                        .any(|role| role.type_id() == info.type_id)
                        .then_some(index)
                })
                .collect();
            assert_eq!(indices.len(), 2);
            indices.sort_unstable_by(|a, b| b.cmp(a));
            for index in indices {
                snapshot.types.remove(index);
                snapshot.methods.remove(index);
            }
            artifact.type_pool = type_pool::TypePool::restore(snapshot).unwrap();
            let mut engine = initialization::Engine::with_defaults();
            assert!(
                install_artifact(engine.vm_mut(), artifact)
                    .unwrap_err()
                    .to_string()
                    .contains(message)
            );
        }
        let mut bad = make(8, runtime::ids::MAP_KEYS);
        bad.builtins[0].name = "__map_get".into();
        let mut engine = initialization::Engine::with_defaults();
        assert!(matches!(
            install_artifact(engine.vm_mut(), bad),
            Err(ArtifactLoadError::IncompatibleBuiltin(_))
        ));
    }

    #[test]
    fn abi_two_accepts_lists_but_cannot_claim_new_map_imports() {
        let make = |revision, id| CompiledArtifact {
            codegen_output: nsbc::CodegenOutput {
                scope_coverage: nsbc::ScopeCoverage::Calls,
                method_call_scopes: None,
                functions: vec![],
                constants: vec![],
                globals: vec![],
            },
            type_pool: type_pool::TypePool::with_intrinsics(),
            entry: None,
            builtin_abi_version: revision,
            builtins: vec![nsbc::BuiltinImport {
                id,
                name: runtime::lookup_builtin_fn_meta(id).unwrap().name.into(),
            }],
        };
        let mut engine = initialization::Engine::with_defaults();
        assert!(install_artifact(engine.vm_mut(), make(2, runtime::ids::LIST_INIT)).is_ok());
        for revision in [1, 2] {
            let mut engine = initialization::Engine::with_defaults();
            assert!(matches!(
                install_artifact(engine.vm_mut(), make(revision, runtime::ids::MAP_INIT)),
                Err(ArtifactLoadError::IncompatibleBuiltin(_))
            ));
        }
        let mut artifact = make(runtime::BUILTIN_ABI_VERSION, runtime::ids::MAP_INIT);
        let mut snapshot = artifact.type_pool.snapshot();
        snapshot.types.truncate(snapshot.types.len() - 2);
        snapshot.methods.truncate(snapshot.methods.len() - 2);
        artifact.type_pool = type_pool::TypePool::restore(snapshot).unwrap();
        let mut engine = initialization::Engine::with_defaults();
        let error = install_artifact(engine.vm_mut(), artifact).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Map imports require Map and MapBuffer roles")
        );
        let mut engine = initialization::Engine::with_defaults();
        assert!(
            install_artifact(
                engine.vm_mut(),
                make(runtime::BUILTIN_ABI_VERSION, runtime::ids::MAP_INIT)
            )
            .is_ok()
        );
    }
}
