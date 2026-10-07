//! Bounded archive container I/O. Section payload decoding belongs to its
//! subsystem. Executable artifacts additionally require a checked target and
//! a nonzero checksum; legacy containers with zero checksums remain readable.

use std::io::{self, Cursor, Read, Write};

use sha2::{Digest, Sha256};

use nsbc::{
    FILE_HEADER_SIZE, FileHeader, MAGIC, SECTION_ENTRY_SIZE, SectionEntry, SectionKind, VERSION,
};

/// Maximum encoded archive size accepted by this reader and writer (64 MiB).
/// This fixed resource limit may become configurable for larger artifacts.
pub const MAX_ARCHIVE_SIZE: usize = 64 * 1024 * 1024;
const MAX_SECTION_COUNT: usize = 1024;
const WRITER_ALIGNMENT: usize = 8;
const HEADER_DEBUG_FLAG: u32 = 1;
const SECTION_STRIPPABLE_FLAG: u32 = 2;

pub(crate) type Sections = Vec<(SectionEntry, Vec<u8>)>;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn write<W: Write>(sections: &[(SectionKind, Vec<u8>)], w: &mut W) -> io::Result<()> {
    let input_error = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    if sections.len() > MAX_SECTION_COUNT {
        return Err(input_error("too many archive sections"));
    }
    let mut kinds = [false; 8];
    let mut offset = FILE_HEADER_SIZE + sections.len() * SECTION_ENTRY_SIZE;
    let mut entries = Vec::with_capacity(sections.len());
    for (kind, data) in sections {
        if std::mem::replace(&mut kinds[*kind as usize], true) {
            return Err(input_error("duplicate archive section"));
        }
        offset = offset
            .checked_add(WRITER_ALIGNMENT - 1)
            .map(|value| value & !(WRITER_ALIGNMENT - 1))
            .ok_or_else(|| input_error("archive size overflow"))?;
        let end = offset
            .checked_add(data.len())
            .filter(|&end| end <= MAX_ARCHIVE_SIZE)
            .ok_or_else(|| input_error("archive exceeds MAX_ARCHIVE_SIZE"))?;
        entries.push(SectionEntry {
            name_idx: 0,
            kind: *kind as u8,
            offset: offset as u64,
            size: data.len() as u64,
            alignment: WRITER_ALIGNMENT as u32,
            flags: 0,
        });
        offset = end;
    }
    let mut table = Vec::with_capacity(entries.len() * SECTION_ENTRY_SIZE);
    for entry in &entries {
        entry.write_to(&mut table)?;
    }
    let padding = [0; WRITER_ALIGNMENT];
    let mut digest = Sha256::new();
    digest.update(&table);
    let mut written = FILE_HEADER_SIZE + table.len();
    for (entry, (_, data)) in entries.iter().zip(sections) {
        let gap = entry.offset as usize - written;
        digest.update(&padding[..gap]);
        digest.update(data);
        written = entry.offset as usize + data.len();
    }
    let header = FileHeader {
        magic: *MAGIC,
        version: VERSION,
        checksum: digest.finalize().into(),
        target_arch: 0,
        target_os: 0,
        flags: 0,
        section_count: sections.len() as u32,
        section_table_offset: FILE_HEADER_SIZE as u64,
    };
    header.write_to(w)?;
    w.write_all(&table)?;
    let mut written = FILE_HEADER_SIZE + sections.len() * SECTION_ENTRY_SIZE;
    for (entry, (_, data)) in entries.iter().zip(sections) {
        let gap = entry.offset as usize - written;
        w.write_all(&padding[..gap])?;
        w.write_all(data)?;
        written = entry.offset as usize + data.len();
    }
    Ok(())
}

pub(crate) fn read<R: Read>(r: &mut R) -> io::Result<(FileHeader, Sections)> {
    // Bound bytes before trusting section lengths or counts. Reading the extra
    // byte distinguishes an archive at the limit from an oversized input.
    let mut bytes = Vec::new();
    r.take(MAX_ARCHIVE_SIZE as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ARCHIVE_SIZE {
        return Err(invalid("archive exceeds MAX_ARCHIVE_SIZE"));
    }
    let mut cursor = Cursor::new(bytes.as_slice());
    let header = FileHeader::read_from(&mut cursor)?;
    if header.version != VERSION && header.version != 3 {
        return Err(invalid(format!(
            "unsupported NSBC version {}; expected {VERSION}",
            header.version
        )));
    }
    if header.flags & !HEADER_DEBUG_FLAG != 0 {
        return Err(invalid("unsupported archive flags (including compression)"));
    }
    if header.target_arch > 3 || header.target_os > 3 {
        return Err(invalid("unknown archive target"));
    }
    let count =
        usize::try_from(header.section_count).map_err(|_| invalid("section count overflow"))?;
    if count > MAX_SECTION_COUNT {
        return Err(invalid("too many archive sections"));
    }
    let table_start = usize::try_from(header.section_table_offset)
        .map_err(|_| invalid("section table offset overflow"))?;
    let table_end = table_start
        .checked_add(count * SECTION_ENTRY_SIZE)
        .filter(|&end| table_start >= FILE_HEADER_SIZE && end <= bytes.len())
        .ok_or_else(|| invalid("section table is outside archive data"))?;
    cursor.set_position(table_start as u64);
    let mut kinds = [false; 8];
    let mut entries = Vec::with_capacity(count);
    let mut ranges = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = SectionEntry::read_from(&mut cursor)?;
        let kind =
            SectionKind::from_u8(entry.kind).ok_or_else(|| invalid("unknown section kind"))?;
        if std::mem::replace(&mut kinds[kind as usize], true) {
            return Err(invalid("duplicate archive section"));
        }
        if entry.flags & !SECTION_STRIPPABLE_FLAG != 0 {
            return Err(invalid("unsupported section flags (including compression)"));
        }
        if !entry.alignment.is_power_of_two() {
            return Err(invalid("invalid section alignment"));
        }
        if entry.offset % u64::from(entry.alignment) != 0 {
            return Err(invalid("section offset does not satisfy alignment"));
        }
        let start =
            usize::try_from(entry.offset).map_err(|_| invalid("section offset overflow"))?;
        let size = usize::try_from(entry.size).map_err(|_| invalid("section size overflow"))?;
        let end = start
            .checked_add(size)
            .filter(|&end| start >= FILE_HEADER_SIZE && end <= bytes.len())
            .ok_or_else(|| invalid("section is outside archive data"))?;
        if start < table_end && end > table_start
            || size == 0 && start >= table_start && start < table_end
        {
            return Err(invalid("section overlaps section table"));
        }
        ranges.push((start, end));
        entries.push(entry);
    }
    let mut sorted = ranges.clone();
    sorted.sort_unstable();
    let mut previous_end = FILE_HEADER_SIZE;
    for (start, end) in sorted {
        if start < previous_end && start != end {
            return Err(invalid("archive sections overlap"));
        }
        previous_end = previous_end.max(end);
    }
    if header.checksum != [0; 32]
        && header.checksum != <[u8; 32]>::from(Sha256::digest(&bytes[FILE_HEADER_SIZE..]))
    {
        return Err(invalid("archive checksum mismatch"));
    }
    // Decode payloads only after every range has been validated; section sizes
    // cannot drive allocations beyond the bounded input bytes.
    let sections = entries
        .into_iter()
        .zip(ranges)
        .map(|(entry, (start, end))| (entry, bytes[start..end].to_vec()))
        .collect();
    Ok((header, sections))
}

/// Executable archives must target this platform or portable bytecode.
pub(crate) fn validate_target(header: &FileHeader) -> io::Result<()> {
    let architecture = if cfg!(target_arch = "x86_64") {
        1
    } else if cfg!(target_arch = "aarch64") {
        2
    } else if cfg!(target_arch = "riscv64") {
        3
    } else {
        0
    };
    let operating_system = if cfg!(target_os = "linux") {
        1
    } else if cfg!(target_os = "macos") {
        2
    } else if cfg!(target_os = "windows") {
        3
    } else {
        0
    };
    if header.target_arch != 0 && header.target_arch != architecture
        || header.target_os != 0 && header.target_os != operating_system
    {
        return Err(invalid("archive target is incompatible with this platform"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        write(
            &[
                (SectionKind::Code, vec![10, 11, 12]),
                (SectionKind::Constants, vec![20, 21, 22, 23]),
            ],
            &mut bytes,
        )
        .unwrap();
        bytes
    }

    fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn rejects(mut bytes: &[u8], message: &str) {
        let error = read(&mut bytes).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(message), "{error}");
    }

    fn update_checksum(bytes: &mut [u8]) {
        let checksum = Sha256::digest(&bytes[FILE_HEADER_SIZE..]);
        bytes[8..40].copy_from_slice(&checksum);
    }

    #[test]
    fn header_and_sections_use_their_declared_offsets_and_padding() {
        let bytes = fixture();
        let (header, sections) = read(&mut bytes.as_slice()).unwrap();
        assert_eq!(header.section_table_offset, FILE_HEADER_SIZE as u64);
        assert_eq!(sections[0].0.offset, 128);
        assert_eq!(sections[1].0.offset, 136);
        assert_eq!(&bytes[124..128], &[0; 4]);
        assert_eq!(&bytes[131..136], &[0; 5]);
        assert_eq!(sections[0].1, [10, 11, 12]);
        assert_eq!(sections[1].1, [20, 21, 22, 23]);
        assert_eq!(bytes.len(), 140);
    }

    #[test]
    fn reads_relocated_tables_and_out_of_order_sections() {
        let original = fixture();
        let mut bytes = vec![0; 180];
        bytes[..FILE_HEADER_SIZE].copy_from_slice(&original[..FILE_HEADER_SIZE]);
        set_u64(&mut bytes, 52, 80);
        // Place CONSTANTS first in the table, with CODE earlier in the file.
        bytes[80..112].copy_from_slice(&original[92..124]);
        bytes[112..144].copy_from_slice(&original[60..92]);
        set_u64(&mut bytes, 88, 176);
        set_u64(&mut bytes, 120, 160);
        bytes[160..163].copy_from_slice(&[10, 11, 12]);
        bytes[176..180].copy_from_slice(&[20, 21, 22, 23]);
        update_checksum(&mut bytes);
        let (header, sections) = read(&mut bytes.as_slice()).unwrap();
        assert_eq!(header.section_table_offset, 80);
        assert_eq!(sections[0].0.kind, SectionKind::Constants as u8);
        assert_eq!(sections[0].1, [20, 21, 22, 23]);
        assert_eq!(sections[1].1, [10, 11, 12]);
    }

    #[test]
    fn rejects_invalid_table_and_section_ranges() {
        for (offset, value, message) in [
            (52, 0, "section table"),
            (52, u64::MAX, "section table"),
            (68, 0, "outside archive"),
            (68, 64, "overlaps section table"),
            (68, 144, "outside archive"),
            (76, u64::MAX, "section"),
            (68, 136, "sections overlap"),
        ] {
            let mut bytes = fixture();
            set_u64(&mut bytes, offset, value);
            rejects(&bytes, message);
        }
        let mut bytes = fixture();
        set_u32(&mut bytes, 48, MAX_SECTION_COUNT as u32 + 1);
        rejects(&bytes, "too many");
        let mut bytes = fixture();
        bytes.pop();
        rejects(&bytes, "outside archive");
    }

    #[test]
    fn rejects_duplicate_unknown_and_unsupported_section_settings() {
        for (offset, value, message) in [
            (96, SectionKind::Code as u32, "duplicate"),
            (64, 255, "unknown section"),
            (64, 256, "invalid section kind"),
            (84, 0, "invalid section alignment"),
            (84, 3, "invalid section alignment"),
            (88, 1, "unsupported section flags"),
            (88, 4, "unsupported section flags"),
            (44, 2, "unsupported archive flags"),
            (44, 4, "unsupported archive flags"),
        ] {
            let mut bytes = fixture();
            set_u32(&mut bytes, offset, value);
            rejects(&bytes, message);
        }
        let mut bytes = fixture();
        set_u64(&mut bytes, 68, 129);
        rejects(&bytes, "does not satisfy alignment");
    }

    #[test]
    fn accepts_debug_and_strippable_flags_but_checks_target_enums() {
        let mut bytes = fixture();
        set_u32(&mut bytes, 44, HEADER_DEBUG_FLAG);
        set_u32(&mut bytes, 88, SECTION_STRIPPABLE_FLAG);
        update_checksum(&mut bytes);
        let (header, sections) = read(&mut bytes.as_slice()).unwrap();
        assert_eq!(header.flags, HEADER_DEBUG_FLAG);
        assert_eq!(sections[0].0.flags, SECTION_STRIPPABLE_FLAG);
        bytes[40..42].copy_from_slice(&4u16.to_le_bytes());
        rejects(&bytes, "unknown archive target");
        bytes[40..42].copy_from_slice(&3u16.to_le_bytes());
        bytes[42..44].copy_from_slice(&4u16.to_le_bytes());
        rejects(&bytes, "unknown archive target");
    }

    #[test]
    fn duplicate_writer_sections_fail_before_writing_bytes() {
        let mut bytes = Vec::new();
        let error = write(
            &[(SectionKind::Code, vec![]), (SectionKind::Code, vec![])],
            &mut bytes,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(bytes.is_empty());
    }

    #[test]
    fn input_size_is_bounded_before_parsing_metadata() {
        let mut input = io::repeat(0).take(MAX_ARCHIVE_SIZE as u64 + 1);
        let error = read(&mut input).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("MAX_ARCHIVE_SIZE"));
    }

    #[test]
    fn checksum_detects_payload_and_padding_corruption() {
        let original = fixture();
        assert_ne!(&original[8..40], &[0; 32]);
        for offset in [124, 128, 132, 136] {
            let mut bytes = original.clone();
            bytes[offset] ^= 1;
            rejects(&bytes, "checksum mismatch");
        }
        // Low-level legacy inspection still accepts explicitly absent checksums.
        let mut legacy = original;
        legacy[8..40].fill(0);
        read(&mut legacy.as_slice()).unwrap();
    }

    #[test]
    fn executable_targets_must_match_the_current_platform() {
        let (mut header, _) = read(&mut fixture().as_slice()).unwrap();
        validate_target(&header).unwrap();
        for target in 1..=3 {
            header.target_arch = target;
            let expected = match target {
                1 => cfg!(target_arch = "x86_64"),
                2 => cfg!(target_arch = "aarch64"),
                3 => cfg!(target_arch = "riscv64"),
                _ => unreachable!(),
            };
            assert_eq!(validate_target(&header).is_ok(), expected);
        }
        header.target_arch = 0;
        for target in 1..=3 {
            header.target_os = target;
            let expected = match target {
                1 => cfg!(target_os = "linux"),
                2 => cfg!(target_os = "macos"),
                3 => cfg!(target_os = "windows"),
                _ => unreachable!(),
            };
            assert_eq!(validate_target(&header).is_ok(), expected);
        }
    }
}
