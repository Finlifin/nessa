//! NSBC archive serialization and deserialization.
//!
//! All type definitions live in the `nsbc` crate. This crate handles
//! reading and writing `.nsbc` archive files.

mod artifact;
mod constants;
mod container;
mod relocation;
mod type_metadata;

use std::io::{self, Read, Write};

use nsbc::{CodegenOutput, CompiledFunction, Constant, FileHeader, SectionEntry, SectionKind};

pub use artifact::{read_artifact, write_artifact};
pub use container::MAX_ARCHIVE_SIZE;

// ---------------------------------------------------------------------------
// Archive writer
// ---------------------------------------------------------------------------

/// Writes a complete `.nsbc` archive from codegen output.
pub struct ArchiveWriter {
    sections: Vec<(SectionKind, Vec<u8>)>,
}

impl ArchiveWriter {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
        }
    }

    /// Build a CODE section from compiled functions.
    pub fn add_code_section(&mut self, functions: &[CompiledFunction]) {
        let mut data = Vec::new();
        // func_count
        data.extend_from_slice(&(functions.len() as u32).to_le_bytes());

        // Build func entries + code bytes.
        let mut code_blobs: Vec<Vec<u8>> = Vec::new();
        let mut code_offset: u32 = 0;

        // Reserve space for func table after the count.
        // Each FuncEntry: func_id(4) + code_offset(4) + code_size(4) + reg(1) + param(1) + flags(2) = 16 bytes
        let table_size = functions.len() * 16;
        let mut func_table = Vec::with_capacity(table_size);

        for func in functions {
            let mut code = Vec::new();
            for &word in &func.instructions {
                code.extend_from_slice(&word.to_le_bytes());
            }
            let code_size = code.len() as u32;

            // FuncEntry
            func_table.extend_from_slice(&func.func_id.0.to_le_bytes());
            func_table.extend_from_slice(&code_offset.to_le_bytes());
            func_table.extend_from_slice(&code_size.to_le_bytes());
            func_table.push(func.register_count);
            func_table.push(func.param_count);
            let flags: u16 = if func.is_closure { 1 } else { 0 };
            func_table.extend_from_slice(&flags.to_le_bytes());

            code_offset += code_size;
            code_blobs.push(code);
        }

        data.extend_from_slice(&func_table);
        for blob in code_blobs {
            data.extend_from_slice(&blob);
        }

        self.sections.push((SectionKind::Code, data));
    }

    /// Build a CONSTANTS section. Type constants reference this artifact's
    /// type pool; its loader must validate that each index is present.
    ///
    /// # Errors
    /// Returns `InvalidInput` if a type constant uses `TypeIndex::INVALID`.
    pub fn add_constants_section(&mut self, constants: &[Constant]) -> io::Result<()> {
        let mut data = Vec::new();
        // constant_count
        data.extend_from_slice(&(constants.len() as u32).to_le_bytes());

        for c in constants {
            match c {
                Constant::Int(v) => {
                    data.push(0); // tag
                    data.extend_from_slice(&v.to_le_bytes());
                }
                Constant::UInt(v) => {
                    data.push(1);
                    data.extend_from_slice(&v.to_le_bytes());
                }
                Constant::Float(v) => {
                    data.push(2);
                    data.extend_from_slice(&v.to_le_bytes());
                }
                Constant::Str(s) => {
                    data.push(3);
                    let bytes = s.as_bytes();
                    data.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    data.extend_from_slice(bytes);
                }
                Constant::BigInt(bytes) => {
                    data.push(4);
                    data.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    data.extend_from_slice(bytes);
                }
                Constant::Int128(value) => {
                    data.push(5);
                    data.extend_from_slice(&value.to_le_bytes());
                }
                Constant::UInt128(value) => {
                    data.push(6);
                    data.extend_from_slice(&value.to_le_bytes());
                }
                Constant::Enum {
                    type_index,
                    variant,
                } => {
                    if *type_index == type_pool::TypeIndex::INVALID || *variant >= (1 << 25) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid enum constant representation",
                        ));
                    }
                    data.push(8);
                    data.extend_from_slice(&type_index.as_u32().to_le_bytes());
                    data.extend_from_slice(&variant.to_le_bytes());
                }
                Constant::Char(value) => {
                    data.push(9);
                    data.extend_from_slice(&u32::from(*value).to_le_bytes());
                }
                Constant::Type(value) => {
                    if *value == type_pool::TypeIndex::INVALID {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid type index in type constant",
                        ));
                    }
                    data.push(7);
                    data.extend_from_slice(&value.as_u32().to_le_bytes());
                }
            }
        }

        self.sections.push((SectionKind::Constants, data));
        Ok(())
    }

    /// Build a STACK_MAPS section from compiled functions.
    pub fn add_stack_maps_section(&mut self, functions: &[CompiledFunction]) {
        let mut data = Vec::new();
        data.extend_from_slice(&(functions.len() as u32).to_le_bytes());

        for func in functions {
            data.extend_from_slice(&func.func_id.0.to_le_bytes());
            data.extend_from_slice(&(func.safepoint_pcs.len() as u32).to_le_bytes());
            for &pc in &func.safepoint_pcs {
                data.extend_from_slice(&pc.to_le_bytes());
                // ref_bitmap placeholder (4 bytes)
                data.extend_from_slice(&0u32.to_le_bytes());
                // deopt_id placeholder
                data.extend_from_slice(&0u32.to_le_bytes());
            }
        }

        self.sections.push((SectionKind::StackMaps, data));
    }

    /// Add an arbitrary raw section.
    pub fn add_raw_section(&mut self, kind: SectionKind, data: Vec<u8>) {
        self.sections.push((kind, data));
    }

    /// Write the complete archive.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        container::write(&self.sections, w)
    }
}

impl Default for ArchiveWriter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Archive reader
// ---------------------------------------------------------------------------

/// A read view into a parsed `.nsbc` archive.
#[derive(Debug)]
pub struct Archive {
    pub header: FileHeader,
    pub sections: Vec<(SectionEntry, Vec<u8>)>,
}

impl Archive {
    /// Read at most [`MAX_ARCHIVE_SIZE`] bytes, honoring section table and data
    /// offsets and checking section ranges, alignment and supported flags.
    /// Compressed and unknown sections are rejected; nonzero checksums are verified.
    /// Executable archives additionally validate their target in [`read_artifact`].
    ///
    /// # Errors
    /// Returns an I/O error for truncated input or invalid container metadata.
    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let (header, sections) = container::read(r)?;
        let archive = Archive { header, sections };
        if archive.header.version == 3 {
            archive.validate_legacy_code()?;
        }
        Ok(archive)
    }

    fn validate_legacy_code(&self) -> io::Result<()> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid legacy CODE section");
        let Some(code) = self.find_section(SectionKind::Code) else {
            return Ok(());
        };
        let read = |offset: usize| -> io::Result<u32> {
            let bytes = code.get(offset..offset + 4).ok_or_else(invalid)?;
            Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| invalid())?))
        };
        let count = read(0)? as usize;
        let start = count
            .checked_mul(16)
            .and_then(|n| n.checked_add(4))
            .ok_or_else(invalid)?;
        if start > code.len() {
            return Err(invalid());
        }
        let complete = self
            .find_section(SectionKind::Metadata)
            .is_some_and(|metadata| metadata.starts_with(b"NSAM"));
        for index in 0..count {
            let offset = read(4 + index * 16 + 4)? as usize;
            let length = read(4 + index * 16 + 8)? as usize;
            let begin = if complete {
                offset
            } else {
                start.checked_add(offset).ok_or_else(invalid)?
            };
            if begin < start {
                return Err(invalid());
            }
            let end = begin.checked_add(length).ok_or_else(invalid)?;
            let words = code.get(begin..end).ok_or_else(invalid)?;
            if !length.is_multiple_of(4) {
                return Err(invalid());
            }
            for word in words.as_chunks::<4>().0 {
                if (0x6D..=0x70).contains(&(u32::from_le_bytes(*word) >> 24)) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Error opcodes require NSBC version 4",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Find a section by kind.
    pub fn find_section(&self, kind: SectionKind) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|(entry, _)| entry.kind == kind as u8)
            .map(|(_, data)| data.as_slice())
    }

    /// Decode the constants, rejecting malformed tags, lengths and payloads.
    pub fn constants(&self) -> io::Result<Vec<Constant>> {
        let section = self.find_section(SectionKind::Constants).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing CONSTANTS section")
        })?;
        constants::decode(section)
    }
}

// ---------------------------------------------------------------------------
// Convenience: codegen output → archive bytes
// ---------------------------------------------------------------------------

/// Serialize legacy code/constants/stack-map sections, without the type pool,
/// entry or function metadata required for independent execution.
pub fn write_archive(output: &CodegenOutput) -> io::Result<Vec<u8>> {
    if !output.globals.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "global schemas require full artifact metadata serialization",
        ));
    }
    let mut writer = ArchiveWriter::new();
    writer.add_code_section(&output.functions);
    writer.add_constants_section(&output.constants)?;
    writer.add_stack_maps_section(&output.functions);
    let mut buf = Vec::new();
    writer.write_to(&mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nsbc::{FuncId, VERSION};

    #[test]
    fn archives_reject_globals_until_the_schema_can_be_persisted() {
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            functions: vec![],
            constants: vec![],
            globals: vec![nsbc::GlobalInfo {
                type_index: type_pool::Intrinsic::I64.type_index(),
                is_mutable: false,
            }],
        };
        let error = write_archive(&output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("global schemas"));
    }

    #[test]
    fn roundtrip_empty_archive() {
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            functions: vec![],
            constants: vec![],
        };
        let bytes = write_archive(&output).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        assert_eq!(archive.header.version, VERSION);
        assert_eq!(archive.sections.len(), 3); // code, constants, stack_maps
    }

    #[test]
    fn roundtrip_with_function() {
        let func = CompiledFunction {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            name: str_interner::StrId::from_raw(0),
            instructions: vec![nsbc::Instruction::nop().encode()],
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
            safepoint_pcs: vec![0],
        };
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            functions: vec![func],
            constants: vec![Constant::Int(42)],
        };
        let bytes = write_archive(&output).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        assert!(archive.find_section(SectionKind::Code).is_some());
        assert!(archive.find_section(SectionKind::Constants).is_some());
        assert!(archive.find_section(SectionKind::StackMaps).is_some());
    }

    #[test]
    fn roundtrip_signed_and_unsigned_128_bit_boundaries() {
        let signed = [
            i128::MIN,
            i64::MIN as i128 - 1,
            -1,
            0,
            1,
            i64::MAX as i128 + 1,
            i128::MAX,
        ];
        let unsigned = [0, 1, u64::MAX as u128 + 1, 1 << 127, u128::MAX];
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            functions: vec![],
            constants: signed
                .into_iter()
                .map(Constant::Int128)
                .chain(unsigned.into_iter().map(Constant::UInt128))
                .collect(),
        };
        let bytes = write_archive(&output).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        let constants = archive.constants().unwrap();
        assert_eq!(constants.len(), signed.len() + unsigned.len());
        for (&expected, actual) in signed.iter().zip(&constants) {
            assert!(matches!(actual, Constant::Int128(value) if *value == expected));
        }
        for (&expected, actual) in unsigned.iter().zip(&constants[signed.len()..]) {
            assert!(matches!(actual, Constant::UInt128(value) if *value == expected));
        }
    }

    #[test]
    fn wide_integer_constants_have_distinct_tags_and_little_endian_payloads() {
        let mut writer = ArchiveWriter::new();
        writer
            .add_constants_section(&[
                Constant::Int128(i128::MIN),
                Constant::UInt128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff),
            ])
            .unwrap();
        let mut bytes = Vec::new();
        writer.write_to(&mut bytes).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        let section = archive.find_section(SectionKind::Constants).unwrap();
        assert_eq!(section.len(), 4 + 2 * 17);
        assert_eq!(&section[..5], &[2, 0, 0, 0, 5]);
        assert_eq!(&section[5..20], &[0; 15]);
        assert_eq!(section[20], 0x80);
        assert_eq!(section[21], 6);
        assert_eq!(
            &section[22..],
            &[
                0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22,
                0x11, 0x00,
            ]
        );
    }

    #[test]
    fn mixed_constant_tags_preserve_existing_payloads_and_float_bits() {
        let float = f64::from_bits(0x7ff8_0000_0000_0123);
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            functions: vec![],
            constants: vec![
                Constant::Int(i64::MIN),
                Constant::UInt(u64::MAX),
                Constant::Float(float),
                Constant::Str("Nessa 中文\0".into()),
                Constant::BigInt(vec![0, 128, 255]),
                Constant::Int128(i128::MAX),
                Constant::UInt128(u128::MAX),
            ],
        };
        let bytes = write_archive(&output).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        let constants = archive.constants().unwrap();
        assert_eq!(constants.len(), 7);
        assert!(matches!(constants[0], Constant::Int(i64::MIN)));
        assert!(matches!(constants[1], Constant::UInt(u64::MAX)));
        assert!(
            matches!(constants[2], Constant::Float(value) if value.to_bits() == float.to_bits())
        );
        assert!(matches!(&constants[3], Constant::Str(value) if value == "Nessa 中文\0"));
        assert!(matches!(&constants[4], Constant::BigInt(value) if value == &[0, 128, 255]));
        assert!(matches!(constants[5], Constant::Int128(i128::MAX)));
        assert!(matches!(constants[6], Constant::UInt128(u128::MAX)));
    }

    #[test]
    fn constants_requires_a_constants_section() {
        let mut bytes = Vec::new();
        ArchiveWriter::new().write_to(&mut bytes).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        assert_eq!(
            archive.constants().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn enum_constants_preserve_pool_indices_and_25_bit_tags() {
        let pairs = [(0, 0), (u32::MAX - 1, (1 << 25) - 1)];
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            functions: vec![],
            globals: vec![],
            constants: pairs
                .iter()
                .map(|&(ty, variant)| Constant::Enum {
                    type_index: type_pool::TypeIndex::from_raw(ty),
                    variant,
                })
                .collect(),
        };
        let bytes = write_archive(&output).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        let section = archive.find_section(SectionKind::Constants).unwrap();
        assert_eq!(section.len(), 4 + 9 * pairs.len());
        for (chunk, (ty, tag)) in section[4..].as_chunks::<9>().0.iter().zip(pairs) {
            assert_eq!(chunk[0], 8);
            assert_eq!(&chunk[1..5], &ty.to_le_bytes());
            assert_eq!(&chunk[5..9], &tag.to_le_bytes());
        }
        for (constant, (ty, tag)) in archive.constants().unwrap().iter().zip(pairs) {
            assert!(
                matches!(constant, Constant::Enum {type_index,variant} if type_index.as_u32()==ty && *variant==tag)
            );
        }
        for (ty, tag) in [(u32::MAX, 0), (0, 1 << 25)] {
            let mut writer = ArchiveWriter::new();
            assert!(
                writer
                    .add_constants_section(&[Constant::Enum {
                        type_index: type_pool::TypeIndex::from_raw(ty),
                        variant: tag
                    }])
                    .is_err()
            );
            assert!(writer.sections.is_empty());
        }
    }

    #[test]
    fn character_constants_roundtrip_as_unicode_scalars() {
        let values = ['你', '🦀', '\n', '\t', '\0', '\'', '\\', '\u{10ffff}'];
        let mut writer = ArchiveWriter::new();
        writer
            .add_constants_section(&values.map(Constant::Char))
            .unwrap();
        let mut bytes = Vec::new();
        writer.write_to(&mut bytes).unwrap();
        let archive = Archive::read_from(&mut bytes.as_slice()).unwrap();
        let section = archive.find_section(SectionKind::Constants).unwrap();
        assert_eq!(&section[..4], &(values.len() as u32).to_le_bytes());
        for (encoded, expected) in section[4..].as_chunks::<5>().0.iter().zip(values) {
            assert_eq!(encoded[0], 9);
            assert_eq!(&encoded[1..], &u32::from(expected).to_le_bytes());
        }
        for (decoded, expected) in archive.constants().unwrap().iter().zip(values) {
            assert!(matches!(decoded, Constant::Char(value) if *value == expected));
        }
    }

    #[test]
    fn type_constants_roundtrip_as_pool_local_little_endian_indices() {
        let indices = [0, 0x0123_4567, u32::MAX - 1];
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            functions: vec![],
            constants: indices
                .into_iter()
                .map(|raw| Constant::Type(type_pool::TypeIndex::from_raw(raw)))
                .collect(),
        };
        let bytes = write_archive(&output).unwrap();
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        let section = archive.find_section(SectionKind::Constants).unwrap();
        assert_eq!(&section[..4], &3_u32.to_le_bytes());
        assert_eq!(section.len(), 4 + indices.len() * 5);
        let (payloads, trailing) = section[4..].as_chunks::<5>();
        assert!(trailing.is_empty());
        for (&index, payload) in indices.iter().zip(payloads) {
            assert_eq!(payload[0], 7);
            assert_eq!(&payload[1..], &index.to_le_bytes());
        }
        let constants = archive.constants().unwrap();
        for (constant, index) in constants.iter().zip(indices) {
            assert!(matches!(constant, Constant::Type(ty) if ty.as_u32() == index));
        }
    }

    #[test]
    fn serializing_invalid_type_constants_returns_an_error_without_adding_a_section() {
        let mut writer = ArchiveWriter::new();
        let error = writer
            .add_constants_section(&[
                Constant::Int(42),
                Constant::Type(type_pool::TypeIndex::INVALID),
            ])
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(writer.sections.is_empty());
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            functions: vec![],
            constants: vec![Constant::Type(type_pool::TypeIndex::INVALID)],
        };
        assert_eq!(
            write_archive(&output).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn magic_validation() {
        let bad = b"XXXX";
        let result = Archive::read_from(&mut &bad[..]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_archives_with_incompatible_continuation_abi() {
        let mut bytes = Vec::new();
        ArchiveWriter::new().write_to(&mut bytes).unwrap();
        for version in [1u32, 2, VERSION + 1] {
            bytes[4..8].copy_from_slice(&version.to_le_bytes());
            let error = Archive::read_from(&mut &bytes[..]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("unsupported NSBC version"));
        }
    }
}
