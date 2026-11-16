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

/// True when a native builtin accepts or constructs a runtime `List`.
pub const fn builtin_requires_list_roles(id: runtime::BuiltinFnId) -> bool {
    matches!(
        id,
        runtime::ids::LIST_INIT..=runtime::ids::LIST_POP
            | runtime::ids::MAP_KEYS
            | runtime::ids::STR_SPLIT..=runtime::ids::STR_CHARS
            | runtime::ids::STR_SPLIT_WHITESPACE
            | runtime::ids::STR_SPLITN..=runtime::ids::STR_RSPLITN
            | runtime::ids::STR_SPLIT_INCLUSIVE..=runtime::ids::STR_FIND_ALL
            | runtime::ids::STR_SPLIT_ASCII_WHITESPACE
            | runtime::ids::CHAR_ENCODE_UTF8..=runtime::ids::CHAR_ENCODE_UTF16
            | runtime::ids::STR_ENCODE_UTF8..=runtime::ids::STR_FROM_UTF16_LOSSY
            | runtime::ids::STR_SPLIT_AT
            | runtime::ids::INT_TO_BE_BYTES..=runtime::ids::U128_FROM_LE_BYTES
            | runtime::ids::F64_TO_BE_BYTES..=runtime::ids::F64_FROM_LE_BYTES
            | runtime::ids::STR_SPLIT_AT_CHECKED
            | runtime::ids::STR_SPLIT_TERMINATOR..=runtime::ids::STR_RSPLIT_TERMINATOR
            | runtime::ids::STR_FIND_INDICES..=runtime::ids::STR_BYTE_RFIND_INDICES
            | runtime::ids::LIST_WINDOWS..=runtime::ids::MAP_VALUES
            | runtime::ids::LIST_COPY..=runtime::ids::LIST_LAST
            | runtime::ids::F64_TO_NE_BYTES..=runtime::ids::F64_FROM_NE_BYTES
            | runtime::ids::STR_BYTES..=runtime::ids::STR_CHAR_INDICES
            | runtime::ids::LIST_APPEND..=runtime::ids::STR_LINES
            | runtime::ids::MAP_ENTRIES
            | runtime::ids::LIST_CHUNKS..=runtime::ids::LIST_ZIP
            | runtime::ids::FMT_SPRINTF
            | runtime::ids::PAT_FIND
            | runtime::ids::BYTES_PACK
            | runtime::ids::BYTES_UNPACK
            | runtime::ids::STR_FROM_CODES
            | runtime::ids::FILE_READ_BYTES
            | runtime::ids::FILE_WRITE_BYTES
            | runtime::ids::FS_READ_DIR
            | runtime::ids::FS_READ_DIR_ENTRIES
            | runtime::ids::FS_WALK_DIR
            | runtime::ids::ENV_ARGS
            | runtime::ids::PROC_EXEC
            | runtime::ids::PROC_EXEC_CWD
            | runtime::ids::TASK_AWAIT_ALL..=runtime::ids::TASK_AWAIT_ANY
            | runtime::ids::TASK_AWAIT_INDEXED
            | runtime::ids::HOST_CALL
            | runtime::ids::BYTES_FROM_LIST
            | runtime::ids::BYTES_TO_LIST
            | runtime::ids::FFI_CALL
            | runtime::ids::NET_TCP_READ
            | runtime::ids::NET_TCP_WRITE_BYTES
            | runtime::ids::NET_UDP_SEND_BYTES
            | runtime::ids::NET_UDP_RECV
            | runtime::ids::NET_RESOLVE_HOST
    )
}

/// True when a native builtin accepts or constructs a runtime `Map`.
pub const fn builtin_requires_map_roles(id: runtime::BuiltinFnId) -> bool {
    matches!(
        id,
        runtime::ids::MAP_INIT..=runtime::ids::MAP_CONTAINS
            | runtime::ids::MAP_KEYS
            | runtime::ids::MAP_VALUES..=runtime::ids::MAP_INSERT_ABSENT
            | runtime::ids::MAP_ENTRIES..=runtime::ids::MAP_IS_EMPTY
            | runtime::ids::MAP_COPY
            | runtime::ids::ENV_VARS
            | runtime::ids::HOST_CALL
    )
}

/// True when a native builtin accepts or constructs a runtime `Bytes`.
pub const fn builtin_requires_bytes_roles(id: runtime::BuiltinFnId) -> bool {
    matches!(
        id,
        runtime::ids::BYTES_NEW..=runtime::ids::BYTES_EQ | runtime::ids::BYTES_READ_U64_LE
    )
}

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
    if artifact.builtin_abi_version != runtime::BUILTIN_ABI_VERSION {
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

    if artifact
        .builtins
        .iter()
        .any(|import| builtin_requires_list_roles(import.id))
        && (artifact.type_pool.list_type().is_none()
            || artifact.type_pool.list_buffer_type().is_none())
    {
        return Err(ArtifactLoadError::InvalidMetadata(
            nsbc::ArtifactError::new(
                "native imports",
                "List imports require List and Buffer roles",
            ),
        ));
    }

    if artifact
        .builtins
        .iter()
        .any(|import| builtin_requires_map_roles(import.id))
        && (artifact.type_pool.map_type().is_none()
            || artifact.type_pool.map_buffer_type().is_none())
    {
        return Err(ArtifactLoadError::InvalidMetadata(
            nsbc::ArtifactError::new(
                "native imports",
                "Map imports require Map and MapBuffer roles",
            ),
        ));
    }

    if artifact
        .builtins
        .iter()
        .any(|import| builtin_requires_bytes_roles(import.id))
        && (artifact.type_pool.bytes_type().is_none()
            || artifact.type_pool.bytes_buffer_type().is_none())
    {
        return Err(ArtifactLoadError::InvalidMetadata(
            nsbc::ArtifactError::new(
                "native imports",
                "Bytes imports require Bytes and BytesBuffer roles",
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
    vm.install_method_call_scopes(artifact.codegen_output.method_call_scopes)
        .map_err(ArtifactLoadError::Runtime)?;
    for constant in artifact.codegen_output.constants {
        vm.push_constant(&constant)
            .map_err(ArtifactLoadError::Runtime)?;
    }
    Ok(artifact.entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_without_roles(roles: &[type_pool::CollectionRole]) -> type_pool::TypePool {
        let mut snapshot = type_pool::TypePool::with_intrinsics().snapshot();
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
        assert_eq!(indices.len(), roles.len());
        indices.sort_unstable_by(|a, b| b.cmp(a));
        for index in indices {
            snapshot.types.remove(index);
            snapshot.methods.remove(index);
        }
        type_pool::TypePool::restore(snapshot).unwrap()
    }

    fn artifact_with_import(
        revision: u32,
        pool: type_pool::TypePool,
        import: nsbc::BuiltinImport,
    ) -> CompiledArtifact {
        CompiledArtifact {
            codegen_output: nsbc::CodegenOutput {
                method_call_scopes: Vec::new(),
                functions: vec![],
                constants: vec![],
                globals: vec![],
            },
            type_pool: pool,
            entry: None,
            builtin_abi_version: revision,
            builtins: vec![import],
        }
    }

    #[test]
    fn current_list_imports_require_collection_roles_before_vm_installation() {
        let make = |pool| {
            artifact_with_import(
                runtime::BUILTIN_ABI_VERSION,
                pool,
                nsbc::BuiltinImport {
                    id: runtime::ids::LIST_INIT,
                    name: "__list_init".into(),
                },
            )
        };
        let legacy_pool = pool_without_roles(&[
            type_pool::CollectionRole::List,
            type_pool::CollectionRole::Buffer,
        ]);
        let mut engine = initialization::Engine::with_defaults();
        let error = install_artifact(engine.vm_mut(), make(legacy_pool)).unwrap_err();
        assert!(error.to_string().contains("require List and Buffer roles"));
        assert!(
            install_artifact(
                engine.vm_mut(),
                make(type_pool::TypePool::with_intrinsics())
            )
            .is_ok()
        );
    }

    #[test]
    fn non_current_builtin_abi_revisions_and_retired_derived_display_id_are_rejected() {
        for revision in [
            0,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            9,
            10,
            11,
            runtime::BUILTIN_ABI_VERSION + 1,
        ] {
            let artifact = artifact_with_import(
                revision,
                type_pool::TypePool::with_intrinsics(),
                nsbc::BuiltinImport {
                    id: runtime::ids::PRINT,
                    name: "print".into(),
                },
            );
            let mut engine = initialization::Engine::with_defaults();
            assert!(
                matches!(
                    install_artifact(engine.vm_mut(), artifact),
                    Err(ArtifactLoadError::IncompatibleBuiltin(_))
                ),
                "accepted non-current builtin ABI revision {revision}"
            );
        }

        let retired = artifact_with_import(
            runtime::BUILTIN_ABI_VERSION,
            type_pool::TypePool::with_intrinsics(),
            nsbc::BuiltinImport {
                id: 120,
                name: "__derived_display".into(),
            },
        );
        let mut engine = initialization::Engine::with_defaults();
        assert!(matches!(
            install_artifact(engine.vm_mut(), retired),
            Err(ArtifactLoadError::IncompatibleBuiltin(_))
        ));
    }

    #[test]
    fn map_snapshot_imports_require_current_abi_and_both_collection_roles() {
        for id in [
            runtime::ids::MAP_KEYS,
            runtime::ids::MAP_VALUES,
            runtime::ids::MAP_ENTRIES,
        ] {
            let name = runtime::lookup_builtin_fn_meta(id).unwrap().name;
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
                let artifact = artifact_with_import(
                    runtime::BUILTIN_ABI_VERSION,
                    pool_without_roles(&roles),
                    nsbc::BuiltinImport {
                        id,
                        name: name.into(),
                    },
                );
                let mut engine = initialization::Engine::with_defaults();
                assert!(
                    install_artifact(engine.vm_mut(), artifact)
                        .unwrap_err()
                        .to_string()
                        .contains(message),
                    "ID {id} ({name}) missing {message}"
                );
            }
        }
    }

    #[test]
    fn abi_nine_imports_enforce_exact_revision_and_complete_list_and_map_role_contracts() {
        let abi_nine_ids = runtime::ids::STR_CONTAINS..=runtime::ids::HOST_CALL;
        for id in abi_nine_ids {
            let name = runtime::lookup_builtin_fn_meta(id).unwrap().name;
            let valid = artifact_with_import(
                runtime::BUILTIN_ABI_VERSION,
                type_pool::TypePool::with_intrinsics(),
                nsbc::BuiltinImport {
                    id,
                    name: name.into(),
                },
            );
            let mut engine = initialization::Engine::with_defaults();
            assert!(
                install_artifact(engine.vm_mut(), valid).is_ok(),
                "ID {id} ({name})"
            );

            let no_list = artifact_with_import(
                runtime::BUILTIN_ABI_VERSION,
                pool_without_roles(&[
                    type_pool::CollectionRole::List,
                    type_pool::CollectionRole::Buffer,
                ]),
                nsbc::BuiltinImport {
                    id,
                    name: name.into(),
                },
            );
            let mut engine = initialization::Engine::with_defaults();
            let list_result = install_artifact(engine.vm_mut(), no_list);
            if builtin_requires_list_roles(id) {
                assert!(
                    list_result
                        .unwrap_err()
                        .to_string()
                        .contains("List imports require"),
                    "ID {id} ({name}) should require List roles"
                );
            } else {
                assert!(
                    list_result.is_ok(),
                    "ID {id} ({name}) should not require List roles"
                );
            }

            let no_map = artifact_with_import(
                runtime::BUILTIN_ABI_VERSION,
                pool_without_roles(&[
                    type_pool::CollectionRole::Map,
                    type_pool::CollectionRole::MapBuffer,
                ]),
                nsbc::BuiltinImport {
                    id,
                    name: name.into(),
                },
            );
            let mut engine = initialization::Engine::with_defaults();
            let map_result = install_artifact(engine.vm_mut(), no_map);
            if builtin_requires_map_roles(id) {
                assert!(
                    map_result
                        .unwrap_err()
                        .to_string()
                        .contains("Map imports require"),
                    "ID {id} ({name}) should require Map roles"
                );
            } else {
                assert!(
                    map_result.is_ok(),
                    "ID {id} ({name}) should not require Map roles"
                );
            }
        }

        let bad_name = artifact_with_import(
            runtime::BUILTIN_ABI_VERSION,
            type_pool::TypePool::with_intrinsics(),
            nsbc::BuiltinImport {
                id: runtime::ids::STR_TRIM,
                name: "str_trim_start".into(),
            },
        );
        let mut engine = initialization::Engine::with_defaults();
        assert!(matches!(
            install_artifact(engine.vm_mut(), bad_name),
            Err(ArtifactLoadError::IncompatibleBuiltin(_))
        ));
    }
}
