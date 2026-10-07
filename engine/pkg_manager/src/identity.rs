use std::collections::BTreeMap;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::ManifestDocument;

pub(crate) const SCHEMA: u32 = 1;
const DOMAIN: &[u8] = b"nessa.package.identity\0";

/// Schema-1 Merkle identity: the first 128 bits of domain-separated SHA-256.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageIdentity([u8; 16]);

impl PackageIdentity {
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
    pub(crate) fn parse(source: &str) -> Option<Self> {
        if source.len() != 32
            || !source
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        let mut bytes = [0; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&source[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
impl fmt::Debug for PackageIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PackageIdentity({self})")
    }
}

pub(crate) fn identity(
    document: &ManifestDocument,
    children: &BTreeMap<String, PackageIdentity>,
) -> PackageIdentity {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(DOMAIN);
    bytes.extend_from_slice(&SCHEMA.to_be_bytes());
    encode_value(
        &toml::Value::Table(document.identity_metadata()),
        &mut bytes,
    );
    let mut dependencies = document.manifest.dependencies.iter().collect::<Vec<_>>();
    dependencies.sort_by_key(|dependency| format!("{}/{}", dependency.domain, dependency.name));
    number(dependencies.len() as u64, &mut bytes);
    for dependency in dependencies {
        string(
            &format!("{}/{}", dependency.domain, dependency.name),
            &mut bytes,
        );
        let comparators = dependency.constraint.canonical();
        number(comparators.len() as u64, &mut bytes);
        for comparator in comparators {
            bytes.extend_from_slice(&comparator);
        }
    }
    number(children.len() as u64, &mut bytes);
    for (name, identity) in children {
        string(name, &mut bytes);
        bytes.extend_from_slice(identity.as_bytes());
    }
    let digest = Sha256::digest(bytes);
    let mut identity = [0; 16];
    identity.copy_from_slice(&digest[..16]);
    PackageIdentity(identity)
}

pub(crate) fn number(value: u64, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
pub(crate) fn string(value: &str, bytes: &mut Vec<u8>) {
    number(value.len() as u64, bytes);
    bytes.extend_from_slice(value.as_bytes());
}

fn encode_value(value: &toml::Value, bytes: &mut Vec<u8>) {
    match value {
        toml::Value::String(value) => {
            bytes.push(1);
            string(value, bytes);
        }
        toml::Value::Integer(value) => {
            bytes.push(2);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        toml::Value::Float(value) => {
            bytes.push(3);
            let bits = if value.is_nan() {
                0x7ff8000000000000
            } else {
                value.to_bits()
            };
            bytes.extend_from_slice(&bits.to_be_bytes());
        }
        toml::Value::Boolean(value) => {
            bytes.push(4);
            bytes.push(u8::from(*value));
        }
        toml::Value::Datetime(value) => {
            bytes.push(5);
            bytes.push(u8::from(value.date.is_some()));
            if let Some(date) = value.date {
                bytes.extend_from_slice(&date.year.to_be_bytes());
                bytes.extend_from_slice(&[date.month, date.day]);
            }
            bytes.push(u8::from(value.time.is_some()));
            if let Some(time) = value.time {
                bytes.extend_from_slice(&[time.hour, time.minute, time.second]);
                bytes.extend_from_slice(&time.nanosecond.to_be_bytes());
            }
            bytes.push(u8::from(value.offset.is_some()));
            if let Some(offset) = value.offset {
                let minutes = match offset {
                    toml::value::Offset::Z => 0,
                    toml::value::Offset::Custom { minutes } => minutes,
                };
                bytes.extend_from_slice(&i32::from(minutes).to_be_bytes());
            }
        }
        toml::Value::Array(values) => {
            bytes.push(6);
            number(values.len() as u64, bytes);
            for value in values {
                encode_value(value, bytes);
            }
        }
        toml::Value::Table(table) => {
            bytes.push(7);
            number(table.len() as u64, bytes);
            let sorted: BTreeMap<_, _> = table.iter().collect();
            for (key, value) in sorted {
                string(key, bytes);
                encode_value(value, bytes);
            }
        }
    }
}
