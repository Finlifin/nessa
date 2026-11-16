use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::{
    Dependency, PackageError, PackageIdentity, PackageManifest, PackageType, Version,
    VersionConstraint,
};

/// A local filesystem path dependency declared in `[dependencies]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDependency {
    /// Original TOML key (`"dep"` or `"domain/dep"`).
    pub key: String,
    /// Explicit domain when key was `"domain/dep"`, or `None` when unqualified `"dep"`.
    pub domain: Option<String>,
    /// Short package name (`"dep"`).
    pub name: String,
    /// Raw path string declared in `package.toml`.
    pub path: String,
    /// Optional version constraint when `version = "..."` was also specified.
    pub constraint: Option<VersionConstraint>,
}

/// One exported WebAssembly function binding inside a `[[ffi.wasm]]` manifest entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiWasmExportSpec {
    pub nessa_name: String,
    pub wasm_export: String,
    pub params: Vec<String>,
    pub ret: String,
}

/// An embedded WebAssembly module declared via `[[ffi.wasm]]` in `package.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiWasmModuleSpec {
    pub name: String,
    pub file: String,
    pub exports: Vec<FfiWasmExportSpec>,
}

/// Declared FFI capability permissions from `[ffi]` in `package.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FfiConfig {
    /// Whether this package is authorized to invoke `__ffi_*` builtins or embed `[[ffi.wasm]]`.
    pub allow: bool,
    /// Optional explicit allowlist of native library names permitted in `__ffi_dlopen`.
    /// When empty and `allow == true`, any library name may be passed to `__ffi_dlopen`.
    pub libraries: Vec<String>,
    /// Embedded WebAssembly modules bound to generated Nessa modules via `Opcode::CallWasm`.
    pub wasm: Vec<FfiWasmModuleSpec>,
}

/// Complete semantic TOML, including fields not yet interpreted by the engine.
/// The legacy manifest remains a projection, not the input to content identity.
#[derive(Debug, Clone)]
pub struct ManifestDocument {
    pub(crate) manifest: PackageManifest,
    pub(crate) metadata: toml::Table,
    pub(crate) source_bytes: usize,
    pub(crate) source_sha256: [u8; 32],
    pub(crate) path_dependencies: Vec<PathDependency>,
    pub(crate) ffi: FfiConfig,
    pub(crate) build_opt_level: Option<u8>,
}

impl ManifestDocument {
    pub fn parse(source: &str) -> Result<Self, PackageError> {
        if source.len() > 16 * 1024 * 1024 {
            return Err(PackageError::Capacity("manifest bytes (16 MiB)"));
        }
        let source_sha256: [u8; 32] = Sha256::digest(source.as_bytes()).into();
        let metadata: toml::Table =
            toml::from_str(source).map_err(|error| PackageError::Manifest(error.to_string()))?;
        let package = metadata
            .get("package")
            .and_then(toml::Value::as_table)
            .ok_or_else(|| PackageError::Manifest("missing [package] table".into()))?;
        let name = required_string(package, "name")?.to_owned();
        let domain = required_string(package, "domain")?.to_owned();
        validate_name(&domain, &name)?;
        let version = Version::parse(required_string(package, "version")?).ok_or_else(|| {
            PackageError::Manifest("package.version must be a full semantic version".into())
        })?;
        let package_type =
            PackageType::parse(required_string(package, "type")?).ok_or_else(|| {
                PackageError::Manifest("package.type must be exe, lib, or tmp".into())
            })?;
        let mut dependencies = Vec::new();
        let mut path_dependencies = Vec::new();
        let mut seen_path_keys = BTreeSet::new();
        if let Some(value) = metadata.get("dependencies") {
            let table = value
                .as_table()
                .ok_or_else(|| PackageError::Manifest("dependencies must be a table".into()))?;
            if table.len() > 65_536 {
                return Err(PackageError::Capacity("manifest dependencies (65536)"));
            }
            for (key, value) in table {
                if let Some(dep_table) = value.as_table()
                    && dep_table.contains_key("path")
                {
                    let raw_path = required_string(dep_table, "path")?;
                    if raw_path.contains('\0') {
                        return Err(PackageError::Manifest(
                            "dependency path must not contain NUL bytes".into(),
                        ));
                    }
                    let (opt_domain, short_name) = if key.contains('/') {
                        let (dep_domain, dep_name) = split_name(key)?;
                        (Some(dep_domain.to_owned()), dep_name.to_owned())
                    } else {
                        validate_part(key)?;
                        (None, key.to_owned())
                    };
                    if !seen_path_keys.insert((opt_domain.clone(), short_name.clone())) {
                        return Err(PackageError::Manifest(format!(
                            "duplicate local path dependency: {key}"
                        )));
                    }
                    let opt_constraint = if dep_table.contains_key("version") {
                        let ver_str = required_string(dep_table, "version")?;
                        let parsed = VersionConstraint::parse(ver_str).ok_or_else(|| {
                            PackageError::Manifest(format!(
                                "invalid dependency version constraint: {ver_str}"
                            ))
                        })?;
                        Some(parsed)
                    } else {
                        None
                    };
                    let effective_domain = opt_domain.clone().unwrap_or_else(|| domain.clone());
                    let effective_constraint = opt_constraint
                        .clone()
                        .unwrap_or_else(VersionConstraint::any);
                    path_dependencies.push(PathDependency {
                        key: key.clone(),
                        domain: opt_domain,
                        name: short_name.clone(),
                        path: raw_path.to_owned(),
                        constraint: opt_constraint,
                    });
                    dependencies.push(Dependency {
                        domain: effective_domain,
                        name: short_name,
                        constraint: effective_constraint,
                    });
                } else {
                    let (dep_domain, dep_name) = split_name(key)?;
                    let constraint = dependency_constraint(value)?;
                    dependencies.push(Dependency {
                        domain: dep_domain.to_owned(),
                        name: dep_name.to_owned(),
                        constraint,
                    });
                }
            }
        }
        dependencies.sort_by_key(|dependency| format!("{}/{}", dependency.domain, dependency.name));
        for pair in dependencies.windows(2) {
            if pair[0].domain == pair[1].domain && pair[0].name == pair[1].name {
                return Err(PackageError::Manifest(format!(
                    "duplicate dependency: {}/{}",
                    pair[0].domain, pair[0].name
                )));
            }
        }
        path_dependencies.sort_by(|a, b| a.key.cmp(&b.key));
        let ffi = parse_ffi_config(metadata.get("ffi"))?;
        let build_opt_level = parse_build_opt_level(metadata.get("build"))?;
        Ok(Self {
            manifest: PackageManifest {
                name,
                domain,
                version,
                package_type,
                dependencies,
            },
            metadata,
            source_bytes: source.len(),
            source_sha256,
            path_dependencies,
            ffi,
            build_opt_level,
        })
    }

    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }
    pub fn metadata(&self) -> &toml::Table {
        &self.metadata
    }
    pub fn path_dependencies(&self) -> &[PathDependency] {
        &self.path_dependencies
    }
    pub fn ffi(&self) -> &FfiConfig {
        &self.ffi
    }
    pub fn build_opt_level(&self) -> Option<u8> {
        self.build_opt_level
    }
    pub fn source_sha256(&self) -> &[u8; 32] {
        &self.source_sha256
    }
    pub fn local_identity(&self) -> PackageIdentity {
        crate::identity::identity(self, &BTreeMap::new())
    }

    /// Bind an unqualified (or wildcard-versioned) local path dependency to the
    /// actual `domain` and `version` discovered from its target `package.toml`.
    pub(crate) fn bind_path_dependency(
        &mut self,
        key: &str,
        resolved_domain: &str,
        resolved_version: &Version,
    ) -> Result<(), PackageError> {
        let Some(path_dep) = self.path_dependencies.iter().find(|d| d.key == key) else {
            return Ok(());
        };
        let placeholder_domain = path_dep
            .domain
            .as_deref()
            .unwrap_or(self.manifest.domain.as_str());
        if let Some(dep) = self
            .manifest
            .dependencies
            .iter_mut()
            .find(|d| d.name == path_dep.name && d.domain == placeholder_domain)
        {
            dep.domain = resolved_domain.to_owned();
            if path_dep.constraint.is_none() {
                dep.constraint = VersionConstraint::Exact(resolved_version.clone());
            }
        }
        self.manifest
            .dependencies
            .sort_by_key(|d| format!("{}/{}", d.domain, d.name));
        for pair in self.manifest.dependencies.windows(2) {
            if pair[0].domain == pair[1].domain && pair[0].name == pair[1].name {
                return Err(PackageError::Manifest(format!(
                    "duplicate dependency: {}/{}",
                    pair[0].domain, pair[0].name
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn identity_metadata(&self) -> toml::Table {
        let mut metadata = self.metadata.clone();
        if let Some(package) = metadata
            .get_mut("package")
            .and_then(toml::Value::as_table_mut)
        {
            package.remove("version");
        }
        // Constraints have their own typed representation. Unknown attributes
        // of inline dependency tables remain semantic manifest input.
        if let Some(dependencies) = metadata
            .get_mut("dependencies")
            .and_then(toml::Value::as_table_mut)
        {
            dependencies.retain(|_, value| {
                if let Some(attributes) = value.as_table_mut() {
                    attributes.remove("version");
                    !attributes.is_empty()
                } else {
                    false
                }
            });
            if dependencies.is_empty() {
                metadata.remove("dependencies");
            }
        }
        // Build optimization flags do not alter package/type semantic identity.
        metadata.remove("build");
        metadata
    }
}

fn parse_build_opt_level(value: Option<&toml::Value>) -> Result<Option<u8>, PackageError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value
        .as_table()
        .ok_or_else(|| PackageError::Manifest("[build] must be a table".into()))?;
    for key in table.keys() {
        if key != "opt_level" {
            return Err(PackageError::Manifest(format!(
                "unknown field in [build]: {key}"
            )));
        }
    }
    let Some(opt_val) = table.get("opt_level") else {
        return Ok(None);
    };
    let level = opt_val
        .as_integer()
        .and_then(|v| u8::try_from(v).ok())
        .filter(|&v| v <= 3)
        .ok_or_else(|| {
            PackageError::Manifest("build.opt_level must be an integer 0, 1, 2, or 3".into())
        })?;
    Ok(Some(level))
}

fn parse_ffi_config(value: Option<&toml::Value>) -> Result<FfiConfig, PackageError> {
    let Some(value) = value else {
        return Ok(FfiConfig::default());
    };
    let table = value
        .as_table()
        .ok_or_else(|| PackageError::Manifest("[ffi] must be a table".into()))?;
    for key in table.keys() {
        if key != "allow" && key != "libraries" && key != "wasm" {
            return Err(PackageError::Manifest(format!(
                "unknown field in [ffi]: {key}"
            )));
        }
    }
    let allow = match table.get("allow") {
        Some(v) => v
            .as_bool()
            .ok_or_else(|| PackageError::Manifest("ffi.allow must be a boolean".into()))?,
        None => false,
    };
    let mut libraries = Vec::new();
    if let Some(libs_val) = table.get("libraries") {
        let arr = libs_val.as_array().ok_or_else(|| {
            PackageError::Manifest("ffi.libraries must be an array of strings".into())
        })?;
        if arr.len() > 4096 {
            return Err(PackageError::Capacity("ffi.libraries (4096)"));
        }
        for item in arr {
            let s = item.as_str().ok_or_else(|| {
                PackageError::Manifest("ffi.libraries entries must be strings".into())
            })?;
            if s.is_empty() || s.contains('\0') {
                return Err(PackageError::Manifest(
                    "ffi.libraries entries must be non-empty and must not contain NUL bytes".into(),
                ));
            }
            libraries.push(s.to_owned());
        }
    }
    if !allow && !libraries.is_empty() {
        return Err(PackageError::Manifest(
            "ffi.libraries requires `allow = true` in [ffi]".into(),
        ));
    }

    let mut wasm = Vec::new();
    if let Some(wasm_val) = table.get("wasm") {
        if !allow {
            return Err(PackageError::Manifest(
                "[[ffi.wasm]] requires `allow = true` in [ffi]".into(),
            ));
        }
        let arr = wasm_val.as_array().ok_or_else(|| {
            PackageError::Manifest("ffi.wasm must be an array of [[ffi.wasm]] tables".into())
        })?;
        if arr.len() > 1024 {
            return Err(PackageError::Capacity("ffi.wasm (1024)"));
        }
        let mut seen_mod_names = BTreeSet::new();
        for item in arr {
            let mod_tbl = item.as_table().ok_or_else(|| {
                PackageError::Manifest("[[ffi.wasm]] entry must be a table".into())
            })?;
            for key in mod_tbl.keys() {
                if key != "name" && key != "file" && key != "exports" {
                    return Err(PackageError::Manifest(format!(
                        "unknown field in [[ffi.wasm]]: {key}"
                    )));
                }
            }
            let mod_name = required_string(mod_tbl, "name")?.to_owned();
            validate_nessa_ident(&mod_name, "[[ffi.wasm]] name")?;
            if !seen_mod_names.insert(mod_name.clone()) {
                return Err(PackageError::Manifest(format!(
                    "duplicate [[ffi.wasm]] module name: {mod_name}"
                )));
            }
            let file_path = required_string(mod_tbl, "file")?.to_owned();
            if file_path.contains('\0')
                || std::path::Path::new(&file_path).is_absolute()
                || std::path::Path::new(&file_path)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(PackageError::Manifest(format!(
                    "[[ffi.wasm]] file must be a safe package-relative path: {file_path}"
                )));
            }
            let exports_tbl = mod_tbl
                .get("exports")
                .and_then(toml::Value::as_table)
                .ok_or_else(|| {
                    PackageError::Manifest(format!(
                        "[[ffi.wasm]] `{mod_name}` missing [ffi.wasm.exports] table"
                    ))
                })?;
            let mut exports = Vec::new();
            for (nessa_name, exp_val) in exports_tbl {
                validate_nessa_ident(nessa_name, "[[ffi.wasm]] export binding")?;
                let exp_tbl = exp_val.as_table().ok_or_else(|| {
                    PackageError::Manifest(format!(
                        "[[ffi.wasm]] export `{nessa_name}` must be an inline table"
                    ))
                })?;
                for k in exp_tbl.keys() {
                    if k != "export" && k != "params" && k != "ret" {
                        return Err(PackageError::Manifest(format!(
                            "unknown field in [[ffi.wasm]] export `{nessa_name}`: {k}"
                        )));
                    }
                }
                let wasm_export = match exp_tbl.get("export") {
                    Some(v) => v
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            PackageError::Manifest(format!(
                                "[[ffi.wasm]] `{nessa_name}.export` must be a non-empty string"
                            ))
                        })?
                        .to_owned(),
                    None => nessa_name.clone(),
                };
                let mut params = Vec::new();
                if let Some(p_val) = exp_tbl.get("params") {
                    let p_arr = p_val.as_array().ok_or_else(|| {
                        PackageError::Manifest(format!(
                            "[[ffi.wasm]] `{nessa_name}.params` must be an array of type names"
                        ))
                    })?;
                    if p_arr.len() > 31 {
                        return Err(PackageError::Manifest(format!(
                            "[[ffi.wasm]] `{nessa_name}` exceeds 31 parameter limit"
                        )));
                    }
                    for p in p_arr {
                        let ty = p.as_str().ok_or_else(|| {
                            PackageError::Manifest(format!(
                                "[[ffi.wasm]] `{nessa_name}.params` entries must be strings"
                            ))
                        })?;
                        validate_wasm_param_type(ty, nessa_name)?;
                        params.push(ty.to_owned());
                    }
                }
                let ret = match exp_tbl.get("ret") {
                    Some(v) => {
                        let ty = v.as_str().ok_or_else(|| {
                            PackageError::Manifest(format!(
                                "[[ffi.wasm]] `{nessa_name}.ret` must be a string"
                            ))
                        })?;
                        let normalized = if ty == "()" { "Unit" } else { ty };
                        validate_wasm_ret_type(normalized, nessa_name)?;
                        normalized.to_owned()
                    }
                    None => "Unit".to_owned(),
                };
                exports.push(FfiWasmExportSpec {
                    nessa_name: nessa_name.clone(),
                    wasm_export,
                    params,
                    ret,
                });
            }
            wasm.push(FfiWasmModuleSpec {
                name: mod_name,
                file: file_path,
                exports,
            });
        }
    }

    Ok(FfiConfig {
        allow,
        libraries,
        wasm,
    })
}

fn validate_nessa_ident(ident: &str, ctx: &str) -> Result<(), PackageError> {
    let mut chars = ident.chars();
    let Some(first) = chars.next() else {
        return Err(PackageError::Manifest(format!("{ctx} must not be empty")));
    };
    if !(first == '_' || first.is_ascii_alphabetic())
        || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        return Err(PackageError::Manifest(format!(
            "{ctx} `{ident}` must be a valid ASCII Nessa identifier"
        )));
    }
    Ok(())
}

fn validate_wasm_param_type(ty: &str, binding: &str) -> Result<(), PackageError> {
    if matches!(
        ty,
        "i8" | "u8"
            | "i16"
            | "u16"
            | "i32"
            | "u32"
            | "bool"
            | "char"
            | "i64"
            | "u64"
            | "isize"
            | "usize"
            | "f32"
            | "f64"
            | "String"
            | "Bytes"
    ) {
        Ok(())
    } else {
        Err(PackageError::Manifest(format!(
            "unsupported [[ffi.wasm]] parameter type `{ty}` in `{binding}`"
        )))
    }
}

fn validate_wasm_ret_type(ty: &str, binding: &str) -> Result<(), PackageError> {
    if matches!(
        ty,
        "Unit"
            | "i8"
            | "u8"
            | "i16"
            | "u16"
            | "i32"
            | "u32"
            | "bool"
            | "char"
            | "i64"
            | "u64"
            | "isize"
            | "usize"
            | "f32"
            | "f64"
    ) {
        Ok(())
    } else {
        Err(PackageError::Manifest(format!(
            "unsupported [[ffi.wasm]] return type `{ty}` in `{binding}`"
        )))
    }
}

fn dependency_constraint(value: &toml::Value) -> Result<VersionConstraint, PackageError> {
    let source = match value {
        toml::Value::String(source) => source.as_str(),
        toml::Value::Table(table) => required_string(table, "version")?,
        _ => {
            return Err(PackageError::Manifest(
                "dependency must be a constraint string or table with version".into(),
            ));
        }
    };
    VersionConstraint::parse(source).ok_or_else(|| {
        PackageError::Manifest(format!("invalid dependency version constraint: {source}"))
    })
}

fn required_string<'a>(table: &'a toml::Table, key: &str) -> Result<&'a str, PackageError> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PackageError::Manifest(format!("missing or invalid string field: {key}")))
}

pub(crate) fn split_name(name: &str) -> Result<(&str, &str), PackageError> {
    let (domain, package) = name.split_once('/').ok_or_else(|| {
        PackageError::Manifest(format!("dependency name must be domain/package: {name}"))
    })?;
    validate_name(domain, package)?;
    Ok((domain, package))
}

pub(crate) fn validate_part(part: &str) -> Result<(), PackageError> {
    if part.is_empty()
        || part.chars().any(|character| {
            character == '/' || character.is_whitespace() || character.is_control()
        })
    {
        return Err(PackageError::Manifest(
            "domain and name must be nonempty, without whitespace or slashes".into(),
        ));
    }
    Ok(())
}

fn validate_name(domain: &str, name: &str) -> Result<(), PackageError> {
    validate_part(domain)?;
    validate_part(name)?;
    Ok(())
}
