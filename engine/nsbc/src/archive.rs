//! NSBC archive format type definitions.
//!
//! The `.nsbc` archive consists of a 60-byte file header, a section table
//! (32 bytes per entry), and section data blobs.

use std::io::{self, Read, Write};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAGIC: &[u8; 4] = b"NSBC";
pub const VERSION: u32 = 2;

/// Size of the file header in bytes.
pub const FILE_HEADER_SIZE: usize = 60;
/// Size of a section table entry in bytes.
pub const SECTION_ENTRY_SIZE: usize = 32;

// ---------------------------------------------------------------------------
// SectionKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SectionKind {
    Code = 0,
    Metadata = 1,
    StackMaps = 2,
    Constants = 3,
    DebugInfo = 4,
    Imports = 5,
    Exports = 6,
    Wasm = 7,
}

impl SectionKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Code),
            1 => Some(Self::Metadata),
            2 => Some(Self::StackMaps),
            3 => Some(Self::Constants),
            4 => Some(Self::DebugInfo),
            5 => Some(Self::Imports),
            6 => Some(Self::Exports),
            7 => Some(Self::Wasm),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// FileHeader
// ---------------------------------------------------------------------------

/// The 60-byte file header.
#[derive(Debug, Clone)]
pub struct FileHeader {
    /// b"NSBC"
    pub magic: [u8; 4],
    /// Format version.
    pub version: u32,
    /// SHA-256 checksum of all data after the header (32 bytes).
    pub checksum: [u8; 32],
    /// Target architecture (0 = portable).
    pub target_arch: u8,
    /// Target OS (0 = portable).
    pub target_os: u8,
    /// Flags (reserved).
    pub flags: u16,
    /// Number of sections.
    pub section_count: u32,
    /// Offset of the section table from start of file.
    pub section_table_offset: u64,
}

impl FileHeader {
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.magic)?;
        w.write_all(&self.version.to_le_bytes())?;
        w.write_all(&self.checksum)?;
        w.write_all(&[self.target_arch])?;
        w.write_all(&[self.target_os])?;
        w.write_all(&self.flags.to_le_bytes())?;
        w.write_all(&self.section_count.to_le_bytes())?;
        w.write_all(&self.section_table_offset.to_le_bytes())?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad NSBC magic"));
        }

        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);

        let mut checksum = [0u8; 32];
        r.read_exact(&mut checksum)?;

        let mut buf1 = [0u8; 1];
        r.read_exact(&mut buf1)?;
        let target_arch = buf1[0];
        r.read_exact(&mut buf1)?;
        let target_os = buf1[0];

        let mut buf2 = [0u8; 2];
        r.read_exact(&mut buf2)?;
        let flags = u16::from_le_bytes(buf2);

        r.read_exact(&mut buf4)?;
        let section_count = u32::from_le_bytes(buf4);

        let mut buf8 = [0u8; 8];
        r.read_exact(&mut buf8)?;
        let section_table_offset = u64::from_le_bytes(buf8);

        Ok(FileHeader {
            magic,
            version,
            checksum,
            target_arch,
            target_os,
            flags,
            section_count,
            section_table_offset,
        })
    }
}

// ---------------------------------------------------------------------------
// SectionEntry
// ---------------------------------------------------------------------------

/// A 32-byte section table entry.
#[derive(Debug, Clone)]
pub struct SectionEntry {
    /// Section name index (into string pool; 0 if unnamed).
    pub name_idx: u32,
    /// Section type.
    pub kind: u8,
    /// Offset of section data from start of file.
    pub offset: u64,
    /// Size of section data in bytes.
    pub size: u64,
    /// Required alignment.
    pub alignment: u32,
    /// Section flags.
    pub flags: u32,
}

impl SectionEntry {
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.name_idx.to_le_bytes())?;
        w.write_all(&[self.kind])?;
        w.write_all(&[0u8; 3])?; // padding
        w.write_all(&self.offset.to_le_bytes())?;
        w.write_all(&self.size.to_le_bytes())?;
        w.write_all(&self.alignment.to_le_bytes())?;
        w.write_all(&self.flags.to_le_bytes())?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut buf4 = [0u8; 4];
        r.read_exact(&mut buf4)?;
        let name_idx = u32::from_le_bytes(buf4);

        let mut buf1 = [0u8; 1];
        r.read_exact(&mut buf1)?;
        let kind = buf1[0];

        let mut _pad = [0u8; 3];
        r.read_exact(&mut _pad)?; // padding

        let mut buf8 = [0u8; 8];
        r.read_exact(&mut buf8)?;
        let offset = u64::from_le_bytes(buf8);

        r.read_exact(&mut buf8)?;
        let size = u64::from_le_bytes(buf8);

        r.read_exact(&mut buf4)?;
        let alignment = u32::from_le_bytes(buf4);

        r.read_exact(&mut buf4)?;
        let flags = u32::from_le_bytes(buf4);

        Ok(SectionEntry {
            name_idx,
            kind,
            offset,
            size,
            alignment,
            flags,
        })
    }
}
