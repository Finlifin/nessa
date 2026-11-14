use std::collections::BTreeMap;

use crate::{
    Dependency, PackageError, PackageIdentity, PackageManifest, PackageType, Version,
    VersionConstraint,
};

/// Complete semantic TOML, including fields not yet interpreted by the engine.
/// The legacy manifest remains a projection, not the input to content identity.
#[derive(Debug, Clone)]
pub struct ManifestDocument {
    pub(crate) manifest: PackageManifest,
    pub(crate) metadata: toml::Table,
    pub(crate) source_bytes: usize,
}

impl ManifestDocument {
    pub fn parse(source: &str) -> Result<Self, PackageError> {
        if source.len() > 16 * 1024 * 1024 {
            return Err(PackageError::Capacity("manifest bytes (16 MiB)"));
        }
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
        if let Some(value) = metadata.get("dependencies") {
            let table = value
                .as_table()
                .ok_or_else(|| PackageError::Manifest("dependencies must be a table".into()))?;
            if table.len() > 65_536 {
                return Err(PackageError::Capacity("manifest dependencies (65536)"));
            }
            for (key, value) in table {
                let (domain, name) = split_name(key)?;
                let constraint = dependency_constraint(value)?;
                dependencies.push(Dependency {
                    domain: domain.to_owned(),
                    name: name.to_owned(),
                    constraint,
                });
            }
        }
        dependencies.sort_by_key(|dependency| format!("{}/{}", dependency.domain, dependency.name));
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
        })
    }

    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }
    pub fn metadata(&self) -> &toml::Table {
        &self.metadata
    }
    pub fn local_identity(&self) -> PackageIdentity {
        crate::identity::identity(self, &BTreeMap::new())
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
        metadata
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

fn validate_name(domain: &str, name: &str) -> Result<(), PackageError> {
    if [domain, name].iter().any(|part| {
        part.is_empty()
            || part.chars().any(|character| {
                character == '/' || character.is_whitespace() || character.is_control()
            })
    }) {
        return Err(PackageError::Manifest(
            "domain and name must be nonempty, without whitespace or slashes".into(),
        ));
    }
    Ok(())
}
