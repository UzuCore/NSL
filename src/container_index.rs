//! Read-only PFS0/HFS0 index parsing shared by NSP/NSZ and XCI/XCZ readers.
//!
//! The validated bounds, name handling and small game-card padding tolerance
//! are adapted from NSRP's MIT-licensed `formats/pfs0.rs` / `formats/hfs0.rs`.
//! See `licenses/nsrp-MIT.txt`. This module neither extracts nor writes ROMs.

use std::{
    collections::{BTreeMap, HashSet},
    io::{Read, Seek, SeekFrom},
};

const MAX_ENTRIES: usize = 16_384;
const MAX_STRING_TABLE: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Pfs0,
    Hfs0,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) name: String,
    /// Offset relative to the beginning of this index's data region.
    pub(crate) offset: u64,
    pub(crate) size: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Index {
    pub(crate) data_start: u64,
    pub(crate) entries: Vec<Entry>,
}

/// One filename policy for metadata readers and the native package rebuilder.
/// This is a format validation rule, not an output-path sanitization step.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// Strict read-only validation for a nested container or game-card partition.
/// Invalid entries reject the entire index, as in the NSRP reference parser.
pub(crate) fn read_at<R: Read + Seek>(
    input: &mut R,
    base: u64,
    file_end: u64,
    required_kind: Option<Kind>,
) -> Option<Index> {
    read_inner(input, base, file_end, required_kind, true)
}

/// Library metadata is best-effort: malformed *unrelated* entries must not
/// hide a valid CNMT or Control NCA. Only safe in-bounds entries are returned.
/// One existing NSP has a valid first entry and a truncated second entry.
pub(crate) fn read_for_metadata<R: Read + Seek>(
    input: &mut R,
    base: u64,
    file_end: u64,
    required_kind: Option<Kind>,
) -> Option<Index> {
    read_inner(input, base, file_end, required_kind, false)
}

fn read_inner<R: Read + Seek>(
    input: &mut R,
    base: u64,
    file_end: u64,
    required_kind: Option<Kind>,
    strict: bool,
) -> Option<Index> {
    let header_end = base.checked_add(16)?;
    if header_end > file_end {
        return None;
    }
    input.seek(SeekFrom::Start(base)).ok()?;
    let mut header = [0u8; 16];
    input.read_exact(&mut header).ok()?;
    let kind = if &header[..4] == b"PFS0" {
        Kind::Pfs0
    } else if &header[..4] == b"HFS0" {
        Kind::Hfs0
    } else {
        return None;
    };
    if required_kind.is_some_and(|expected| expected != kind) {
        return None;
    }
    let count = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
    let string_len = u32::from_le_bytes(header[8..12].try_into().ok()?) as usize;
    if count == 0 || count > MAX_ENTRIES || string_len > MAX_STRING_TABLE {
        return None;
    }
    let entry_size = match kind {
        Kind::Pfs0 => 24usize,
        Kind::Hfs0 => 64usize,
    };
    let table_len = count.checked_mul(entry_size)?;
    let data_start = header_end
        .checked_add(u64::try_from(table_len).ok()?)?
        .checked_add(u64::try_from(string_len).ok()?)?;
    if data_start > file_end {
        return None;
    }

    let mut table = vec![0u8; table_len];
    let mut strings = vec![0u8; string_len];
    input.read_exact(&mut table).ok()?;
    input.read_exact(&mut strings).ok()?;

    let mut entries = Vec::with_capacity(count);
    let mut seen = HashSet::with_capacity(count);
    // Reject aliases: independent entries must not point to overlapping
    // regions, even when each individual extent lies within the file.
    let mut extents = BTreeMap::<u64, u64>::new();
    for record in table.chunks_exact(entry_size) {
        let Some(entry) = parse_entry(record, &strings, data_start, file_end, kind) else {
            if strict {
                return None;
            }
            continue;
        };
        let start = data_start.checked_add(entry.offset)?;
        let end = start.checked_add(entry.size)?;
        let overlaps = entry.size != 0
            && (extents
                .range(..=start)
                .next_back()
                .is_some_and(|(_, previous_end)| *previous_end > start)
                || extents
                    .range(start..)
                    .next()
                    .is_some_and(|(next_start, _)| *next_start < end));
        if overlaps {
            if strict {
                return None;
            }
            // In metadata mode, retain the first valid nonoverlapping entry.
            continue;
        }
        if !seen.insert(entry.name.to_lowercase()) {
            if strict {
                return None;
            }
            continue;
        }
        if entry.size != 0 {
            extents.insert(start, end);
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return None;
    }
    Some(Index {
        data_start,
        entries,
    })
}

fn parse_entry(
    record: &[u8],
    strings: &[u8],
    data_start: u64,
    file_end: u64,
    kind: Kind,
) -> Option<Entry> {
    let relative = u64::from_le_bytes(record[..8].try_into().ok()?);
    let mut size = u64::from_le_bytes(record[8..16].try_into().ok()?);
    let string_at = u32::from_le_bytes(record[16..20].try_into().ok()?) as usize;
    let name_data = strings.get(string_at..)?;
    let name_len = name_data.iter().position(|b| *b == 0)?;
    let name = std::str::from_utf8(&name_data[..name_len]).ok()?;
    if !valid_name(name) {
        return None;
    }
    let start = data_start.checked_add(relative)?;
    let end = start.checked_add(size)?;
    if end > file_end {
        // NSRP tolerates one missing 0x200-byte media sector of a named HFS0
        // partition, never a truncated NCA or arbitrary NSP entry.
        let partition = matches!(
            name.to_ascii_lowercase().as_str(),
            "secure" | "normal" | "update" | "logo"
        );
        if kind != Kind::Hfs0 || !partition || end - file_end > 0x200 || start >= file_end {
            return None;
        }
        size = file_end - start;
    }
    Some(Entry {
        name: name.to_owned(),
        offset: relative,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture(kind: Kind, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut table = Vec::new();
        let mut data = Vec::new();
        for (name, payload) in files {
            let at = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            table.extend_from_slice(&(data.len() as u64).to_le_bytes());
            table.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            table.extend_from_slice(&at.to_le_bytes());
            table.extend_from_slice(&0u32.to_le_bytes());
            if kind == Kind::Hfs0 {
                table.extend_from_slice(&[0u8; 40]);
            }
            data.extend_from_slice(payload);
        }
        let mut result = Vec::new();
        result.extend_from_slice(if kind == Kind::Hfs0 { b"HFS0" } else { b"PFS0" });
        result.extend_from_slice(&(files.len() as u32).to_le_bytes());
        result.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        result.extend_from_slice(&0u32.to_le_bytes());
        result.extend(table);
        result.extend(strings);
        result.extend(data);
        result
    }

    fn parse(data: &[u8], kind: Kind) -> Option<Index> {
        read_at(&mut Cursor::new(data), 0, data.len() as u64, Some(kind))
    }

    #[test]
    fn valid_pfs_index_preserves_entry_names_and_absolute_offsets() {
        let bytes = fixture(
            Kind::Pfs0,
            &[("meta.cnmt.nca", b"alpha"), ("control.ncz", b"beta")],
        );
        let index = parse(&bytes, Kind::Pfs0).expect("valid PFS0");
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[1].name, "control.ncz");
        let start = (index.data_start + index.entries[1].offset) as usize;
        assert_eq!(
            &bytes[start..start + index.entries[1].size as usize],
            b"beta"
        );
    }

    #[test]
    fn reads_embedded_hfs0_from_nonzero_offset() {
        let mut bytes = vec![0x55u8; 0x400];
        bytes.extend(fixture(Kind::Hfs0, &[("secure", b"game-card")]));
        let index = read_at(
            &mut Cursor::new(&bytes),
            0x400,
            bytes.len() as u64,
            Some(Kind::Hfs0),
        )
        .expect("embedded HFS0");
        assert_eq!(index.entries[0].name, "secure");
        assert_eq!(index.entries[0].size, 9);
        assert!(read_at(
            &mut Cursor::new(&bytes),
            0x400,
            bytes.len() as u64,
            Some(Kind::Pfs0)
        )
        .is_none());
    }

    #[test]
    fn rejects_duplicate_names_without_case_sensitivity() {
        let bytes = fixture(Kind::Pfs0, &[("META.NCA", b"a"), ("meta.nca", b"b")]);
        assert!(parse(&bytes, Kind::Pfs0).is_none());
    }

    #[test]
    fn rejects_missing_name_terminator_and_unsafe_names() {
        let mut bytes = fixture(Kind::Pfs0, &[("normal.nca", b"a")]);
        let terminator = 16 + 24 + "normal.nca".len();
        bytes[terminator] = b'X';
        assert!(parse(&bytes, Kind::Pfs0).is_none());
        for name in ["../escape", "folder\\name", "..", ""] {
            assert!(parse(&fixture(Kind::Pfs0, &[(name, b"x")]), Kind::Pfs0).is_none());
        }
    }

    #[test]
    fn rejects_out_of_bounds_and_integer_overflow() {
        let mut bytes = fixture(Kind::Pfs0, &[("file.nca", b"x")]);
        bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse(&bytes, Kind::Pfs0).is_none());
        let mut bytes = fixture(Kind::Pfs0, &[("file.nca", b"x")]);
        bytes[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse(&bytes, Kind::Pfs0).is_none());
        assert!(read_at(&mut Cursor::new(&bytes), u64::MAX, bytes.len() as u64, None).is_none());
    }

    #[test]
    fn metadata_scan_keeps_valid_entries_without_exposing_truncated_ones() {
        let mut bytes = fixture(
            Kind::Pfs0,
            &[("valid.cnmt.nca", b"good"), ("broken.nca", b"bad")],
        );
        // A real NSP has one good entry and a truncated unrelated entry.
        // Metadata reading can use the good one; structural readers reject it.
        bytes[16 + 24 + 8..16 + 24 + 16].copy_from_slice(&0x1000u64.to_le_bytes());
        assert!(parse(&bytes, Kind::Pfs0).is_none());
        let index = read_for_metadata(&mut Cursor::new(&bytes), 0, bytes.len() as u64, None)
            .expect("valid first entry must survive");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].name, "valid.cnmt.nca");
        assert_eq!(index.entries[0].size, 4);
    }

    #[test]
    fn rejects_overlapping_data_ranges_but_accepts_adjacent_and_empty_files() {
        let mut bytes = fixture(
            Kind::Pfs0,
            &[("first.nca", b"abcd"), ("second.nca", b"efgh")],
        );
        assert_eq!(parse(&bytes, Kind::Pfs0).unwrap().entries.len(), 2);
        // The second entry now aliases the final two bytes of the first.
        bytes[16 + 24..16 + 24 + 8].copy_from_slice(&2u64.to_le_bytes());
        assert!(parse(&bytes, Kind::Pfs0).is_none());
        let partial = read_for_metadata(
            &mut Cursor::new(&bytes),
            0,
            bytes.len() as u64,
            Some(Kind::Pfs0),
        )
        .expect("first entry remains valid for metadata");
        assert_eq!(partial.entries.len(), 1);
        assert_eq!(partial.entries[0].name, "first.nca");

        let with_empty = fixture(
            Kind::Pfs0,
            &[("empty-a", b""), ("empty-b", b""), ("data.nca", b"x")],
        );
        assert_eq!(parse(&with_empty, Kind::Pfs0).unwrap().entries.len(), 3);
    }

    #[test]
    fn rejects_overlapping_hfs0_partitions() {
        let mut bytes = fixture(Kind::Hfs0, &[("secure", b"abcd"), ("normal", b"efgh")]);
        assert!(parse(&bytes, Kind::Hfs0).is_some());
        // HFS0 table entries are 64 bytes wide.
        bytes[16 + 64..16 + 64 + 8].copy_from_slice(&1u64.to_le_bytes());
        assert!(parse(&bytes, Kind::Hfs0).is_none());
    }

    #[test]
    fn tolerates_one_sector_of_truncated_hfs_partition_only() {
        let mut bytes = fixture(Kind::Hfs0, &[("secure", b"partition")]);
        bytes[24..32].copy_from_slice(&(9u64 + 0x200).to_le_bytes());
        let parsed = parse(&bytes, Kind::Hfs0).expect("known HFS0 padding");
        assert_eq!(parsed.entries[0].size, 9);
        bytes[24..32].copy_from_slice(&(9u64 + 0x201).to_le_bytes());
        assert!(parse(&bytes, Kind::Hfs0).is_none());
        let mut other = fixture(Kind::Hfs0, &[("control.nca", b"partition")]);
        other[24..32].copy_from_slice(&(9u64 + 0x100).to_le_bytes());
        assert!(parse(&other, Kind::Hfs0).is_none());
    }

    #[test]
    fn enforces_parent_region_boundaries() {
        let bytes = fixture(Kind::Pfs0, &[("content.nca", b"payload")]);
        assert!(read_at(&mut Cursor::new(&bytes), 0, (bytes.len() - 1) as u64, None).is_none());
        let mut bytes = vec![0x11u8; 0x200];
        bytes.extend(fixture(Kind::Pfs0, &[("content.nca", b"payload")]));
        assert!(read_at(&mut Cursor::new(&bytes), 0x200, 0x200 + 10, None).is_none());
    }
}
