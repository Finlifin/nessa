//! NSBC archive serialization and deserialization.
//!
//! All type definitions live in the `nsbc` crate. This crate handles
//! reading and writing `.nsbc` archive files.

use nsbc::{
    CodegenOutput, CompiledFunction, Constant, FuncId,
    FileHeader, SectionEntry, SectionKind,
    MAGIC, VERSION, FILE_HEADER_SIZE, SECTION_ENTRY_SIZE,
};
use std::io::{self, Read, Write};

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

    /// Build a CONSTANTS section.
    pub fn add_constants_section(&mut self, constants: &[Constant]) {
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
            }
        }

        self.sections.push((SectionKind::Constants, data));
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
        let section_count = self.sections.len() as u32;
        let section_table_offset = FILE_HEADER_SIZE as u64;
        let data_start = section_table_offset + (section_count as u64 * SECTION_ENTRY_SIZE as u64);

        // Compute section offsets.
        let mut entries = Vec::new();
        let mut offset = data_start;
        for (kind, data) in &self.sections {
            entries.push(SectionEntry {
                name_idx: 0,
                kind: *kind as u8,
                offset,
                size: data.len() as u64,
                alignment: 8,
                flags: 0,
            });
            offset += data.len() as u64;
        }

        // Write header with zero checksum (we'd compute it in a real implementation).
        let header = FileHeader {
            magic: *MAGIC,
            version: VERSION,
            checksum: [0u8; 32],
            target_arch: 0,
            target_os: 0,
            flags: 0,
            section_count,
            section_table_offset,
        };
        header.write_to(w)?;

        // Write section table.
        for entry in &entries {
            entry.write_to(w)?;
        }

        // Write section data.
        for (_kind, data) in &self.sections {
            w.write_all(data)?;
        }

        Ok(())
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
    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let header = FileHeader::read_from(r)?;

        let mut sections = Vec::new();
        // Read section table.
        let mut entries = Vec::new();
        for _ in 0..header.section_count {
            entries.push(SectionEntry::read_from(r)?);
        }

        // Read section data (assume sections are in order immediately after table).
        for entry in &entries {
            let mut data = vec![0u8; entry.size as usize];
            r.read_exact(&mut data)?;
            sections.push((entry.clone(), data));
        }

        Ok(Archive { header, sections })
    }

    /// Find a section by kind.
    pub fn find_section(&self, kind: SectionKind) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|(entry, _)| entry.kind == kind as u8)
            .map(|(_, data)| data.as_slice())
    }
}

// ---------------------------------------------------------------------------
// Convenience: codegen output → archive bytes
// ---------------------------------------------------------------------------

/// Serialize a CodegenOutput into a complete `.nsbc` archive.
pub fn write_archive(output: &CodegenOutput) -> io::Result<Vec<u8>> {
    let mut writer = ArchiveWriter::new();
    writer.add_code_section(&output.functions);
    writer.add_constants_section(&output.constants);
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

    #[test]
    fn roundtrip_empty_archive() {
        let output = CodegenOutput {
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
            func_id: FuncId(0),
            name: str_interner::StrId::from_raw(0),
            instructions: vec![nsbc::Instruction::nop().encode()],
            register_count: 1,
            param_count: 0,
            is_closure: false,
            safepoint_pcs: vec![0],
        };
        let output = CodegenOutput {
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
    fn magic_validation() {
        let bad = b"XXXX";
        let result = Archive::read_from(&mut &bad[..]);
        assert!(result.is_err());
    }
}
