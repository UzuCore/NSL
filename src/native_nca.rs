//! Read-only native extraction of Switch NCA metadata.  The format and AES
//! modes are based on the ISC-licensed hactool (SciresM/hactool); this module
//! is an independent Rust implementation of only the header/CTR/PFS0 subset.
//! Unsupported, encrypted-rights, or malformed NCAs deliberately fall back
//! to the bundled hactool rather than returning guessed title metadata.

use aes::{
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit},
    Aes128,
};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};

use crate::scanner::{parse_cnmt_header, Content};

const NCA_HEADER_LEN: usize = 0xC00;
const NCA_SECTOR: usize = 0x200;
const PFS0_LIMIT: usize = 1024 * 1024;

pub(crate) struct Keys {
    header_key: [u8; 32],
    key_area: HashMap<(u8, u8), [u8; 16]>,
}

struct KeyCache {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    keys: Arc<Keys>,
}

pub(crate) fn cached_keys(path: &Path) -> Option<Arc<Keys>> {
    static CACHE: OnceLock<Mutex<Option<KeyCache>>> = OnceLock::new();
    let lock = CACHE.get_or_init(|| Mutex::new(None));
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok();
    let mut guard = lock.lock().ok()?;
    if let Some(saved) = guard.as_ref() {
        if saved.path == path && saved.modified == modified && saved.len == meta.len() {
            return Some(Arc::clone(&saved.keys));
        }
    }
    // Parsing keys happens once per modified keyset. No key material is
    // written to logs, temporary folders, or release artifacts.
    let keys = Arc::new(Keys::from_file(path)?);
    *guard = Some(KeyCache {
        path: path.to_path_buf(),
        modified,
        len: meta.len(),
        keys: Arc::clone(&keys),
    });
    Some(keys)
}

impl Keys {
    fn from_file(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let mut header_key = None;
        let mut key_area = HashMap::new();
        for line in text.lines() {
            let line = line.split('#').next()?.trim();
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            if name == "header_key" {
                header_key = decode_key::<32>(value);
            } else {
                for (index, kind) in ["application", "ocean", "system"].iter().enumerate() {
                    let prefix = format!("key_area_key_{kind}_");
                    if let Some(suffix) = name.strip_prefix(&prefix) {
                        if let (Ok(revision), Some(key)) =
                            (u8::from_str_radix(suffix, 16), decode_key::<16>(value))
                        {
                            key_area.insert((revision, index as u8), key);
                        }
                        break;
                    }
                }
            }
        }
        Some(Self {
            header_key: header_key?,
            key_area,
        })
    }
}

fn decode_key<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != 2 * N {
        return None;
    }
    let mut result = [0u8; N];
    for (i, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let hi = (pair[0] as char).to_digit(16)? as u8;
        let lo = (pair[1] as char).to_digit(16)? as u8;
        result[i] = (hi << 4) | lo;
    }
    Some(result)
}

fn multiply_tweak(tweak: &mut [u8; 16]) {
    let mut carry = 0;
    for byte in tweak.iter_mut() {
        let next = *byte >> 7;
        *byte = (*byte << 1) | carry;
        carry = next;
    }
    if carry != 0 {
        tweak[0] ^= 0x87;
    }
}

fn decrypt_xts_header(header: &mut [u8; NCA_HEADER_LEN], key: &[u8; 32]) -> Option<()> {
    let data_cipher = Aes128::new_from_slice(&key[..16]).ok()?;
    let tweak_cipher = Aes128::new_from_slice(&key[16..]).ok()?;
    for (sector_index, sector) in header
        .as_chunks_mut::<NCA_SECTOR>()
        .0
        .iter_mut()
        .enumerate()
    {
        let mut tweak = aes::Block::default();
        tweak.copy_from_slice(&(sector_index as u128).to_be_bytes());
        tweak_cipher.encrypt_block(&mut tweak);
        let mut tweak_bytes: [u8; 16] = tweak.into();
        for chunk in sector.as_chunks_mut::<16>().0.iter_mut() {
            for (byte, mask) in chunk.iter_mut().zip(tweak_bytes) {
                *byte ^= mask;
            }
            let mut block = aes::Block::default();
            block.copy_from_slice(chunk);
            data_cipher.decrypt_block(&mut block);
            for (dest, (plain, mask)) in chunk.iter_mut().zip(block.iter().zip(tweak_bytes)) {
                *dest = *plain ^ mask;
            }
            multiply_tweak(&mut tweak_bytes);
        }
    }
    Some(())
}

fn read_header<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    length: u64,
    keys: &Keys,
) -> Option<[u8; NCA_HEADER_LEN]> {
    if length < NCA_HEADER_LEN as u64 {
        return None;
    }
    let mut header = [0u8; NCA_HEADER_LEN];
    reader.seek(SeekFrom::Start(start)).ok()?;
    reader.read_exact(&mut header).ok()?;
    decrypt_xts_header(&mut header, &keys.header_key)?;
    if !matches!(&header[0x200..0x204], b"NCA2" | b"NCA3") {
        return None;
    }
    Some(header)
}

/// Return `None` when even the NCA header is unsupported: callers must then
/// retain hactool's conservative behavior instead of skipping the candidate.
pub(crate) fn content_kind<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    length: u64,
    keys: &Keys,
) -> Option<u8> {
    Some(read_header(reader, start, length, keys)?[0x205])
}

fn decrypt_ctr_range<R: Read + Seek>(
    reader: &mut R,
    nca_start: u64,
    nca_length: u64,
    relative: u64,
    length: usize,
    nonce: [u8; 8],
    key: &Aes128,
) -> Option<Vec<u8>> {
    if length > PFS0_LIMIT || relative.checked_add(length as u64)? > nca_length {
        return None;
    }
    let absolute = nca_start.checked_add(relative)?;
    reader.seek(SeekFrom::Start(absolute)).ok()?;
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes).ok()?;
    if length == 0 {
        return Some(bytes);
    }
    let first = relative / 16;
    let last = (relative + length as u64 - 1) / 16;
    for block_index in first..=last {
        let mut counter = aes::Block::default();
        counter[..8].copy_from_slice(&nonce);
        counter[8..].copy_from_slice(&block_index.to_be_bytes());
        key.encrypt_block(&mut counter);
        let block_start = block_index * 16;
        let start = relative.max(block_start);
        let end = (relative + length as u64).min(block_start + 16);
        for index in start..end {
            bytes[(index - relative) as usize] ^= counter[(index - block_start) as usize];
        }
    }
    Some(bytes)
}

fn section_cipher(header: &[u8; NCA_HEADER_LEN], keys: &Keys) -> Option<Aes128> {
    // Rights-controlled NCAs need ticket-derived keys; let hactool handle
    // those instead of guessing a plaintext key.
    if header[0x230..0x240].iter().any(|byte| *byte != 0) {
        return None;
    }
    let generation = header[0x206].max(header[0x220]).max(1) - 1;
    let area_key = keys.key_area.get(&(generation, header[0x207]))?;
    let area_cipher = Aes128::new_from_slice(area_key).ok()?;
    let mut key_slot = aes::Block::default();
    key_slot.copy_from_slice(&header[0x320..0x330]);
    area_cipher.decrypt_block(&mut key_slot);
    Aes128::new_from_slice(key_slot.as_slice()).ok()
}

fn section_nonce(header: &[u8; NCA_HEADER_LEN], fs: usize) -> [u8; 8] {
    let mut nonce = [0u8; 8];
    for (i, byte) in nonce.iter_mut().enumerate() {
        *byte = header[fs + 0x147 - i];
    }
    nonce
}

// RomFS entries are linked by their parent directory, not guaranteed to be
// stored in traversal order. A nested `control.nacp` can appear earlier in
// the flat table than the one hactool exposes in the application's root.
fn root_nacp_entry(table: &[u8], mut offset: usize) -> Option<(u64, u64)> {
    let mut visited = 0usize;
    while offset != u32::MAX as usize && offset.checked_add(0x20)? <= table.len() {
        if visited > 4096 {
            break;
        }
        let entry = &table[offset..];
        let name_len = u32::from_le_bytes(entry[0x1c..0x20].try_into().ok()?) as usize;
        let next = offset.checked_add(0x20)?.checked_add(name_len)?;
        if name_len > 1024 || next > table.len() {
            break;
        }
        if entry[0x20..0x20 + name_len].eq_ignore_ascii_case(b"control.nacp") {
            let file_offset = u64::from_le_bytes(entry[8..16].try_into().ok()?);
            let file_len = u64::from_le_bytes(entry[16..24].try_into().ok()?);
            return Some((file_offset, file_len));
        }
        let sibling = u32::from_le_bytes(entry[4..8].try_into().ok()?) as usize;
        if sibling == offset {
            break;
        }
        offset = sibling;
        visited += 1;
    }
    None
}

/// Extract a CNMT directly from the package stream, with no NCA copy,
/// temporary directory, subprocess, or decrypted data persisted on disk.
/// The metadata is small; at most 1 MiB of its PFS0 index is ever allocated.
pub(crate) fn extract_cnmt<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    length: u64,
    keys: &Keys,
) -> Option<Vec<Content>> {
    let header = read_header(reader, start, length, keys)?;
    if header[0x205] != 1 {
        return None;
    }
    let content_cipher = section_cipher(&header, keys)?;

    let mut contents = Vec::new();
    for index in 0..4usize {
        let media = 0x240 + index * 0x10;
        let first = u32::from_le_bytes(header[media..media + 4].try_into().ok()?) as u64 * 512;
        let last = u32::from_le_bytes(header[media + 4..media + 8].try_into().ok()?) as u64 * 512;
        if first >= last || last > length {
            continue;
        }
        let fs = 0x400 + index * 0x200;
        if header[fs + 3] != 2 || header[fs + 4] != 3 {
            continue;
        }
        let pfs_offset = u64::from_le_bytes(header[fs + 0x40..fs + 0x48].try_into().ok()?);
        let pfs_start = first.checked_add(pfs_offset)?;
        if pfs_start.checked_add(16)? > last {
            continue;
        }
        let nonce = section_nonce(&header, fs);
        let pfs_header =
            decrypt_ctr_range(reader, start, length, pfs_start, 16, nonce, &content_cipher)?;
        if &pfs_header[..4] != b"PFS0" {
            continue;
        }
        let count = u32::from_le_bytes(pfs_header[4..8].try_into().ok()?) as usize;
        let names_len = u32::from_le_bytes(pfs_header[8..12].try_into().ok()?) as usize;
        let table_len = count.checked_mul(24)?.checked_add(names_len)?;
        if count == 0 || count > 16_384 || table_len > PFS0_LIMIT {
            continue;
        }
        let data_start = pfs_start.checked_add(16)?.checked_add(table_len as u64)?;
        if data_start > last {
            continue;
        }
        let table = decrypt_ctr_range(
            reader,
            start,
            length,
            pfs_start + 16,
            table_len,
            nonce,
            &content_cipher,
        )?;
        let names = &table[count * 24..];
        for row in table[..count * 24].as_chunks::<24>().0 {
            let offset = u64::from_le_bytes(row[..8].try_into().ok()?);
            let entry_len = u64::from_le_bytes(row[8..16].try_into().ok()?);
            let name_index = u32::from_le_bytes(row[16..20].try_into().ok()?) as usize;
            if name_index >= names.len() || entry_len < 13 {
                continue;
            }
            let name = names[name_index..].split(|byte| *byte == 0).next()?;
            if !name.to_ascii_lowercase().ends_with(b".cnmt") {
                continue;
            }
            let offset = data_start.checked_add(offset)?;
            if offset.checked_add(entry_len)? > last {
                continue;
            }
            let data =
                decrypt_ctr_range(reader, start, length, offset, 13, nonce, &content_cipher)?;
            if let Some(content) = parse_cnmt_header(&data) {
                if !contents
                    .iter()
                    .any(|old: &Content| old.title_id == content.title_id)
                {
                    contents.push(content);
                }
            }
        }
    }
    (!contents.is_empty()).then_some(contents)
}

/// Resolve `control.nacp` directly from a Control NCA RomFS. The IVFC
/// superblock identifies the final RomFS level; only its metadata index and
/// NACP name fields are decrypted, not icons or unrelated assets.
pub(crate) fn extract_nacp<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    length: u64,
    keys: &Keys,
) -> Option<(String, String, String)> {
    let header = read_header(reader, start, length, keys)?;
    if header[0x205] != 2 {
        return None;
    }
    let cipher = section_cipher(&header, keys)?;
    for index in 0..4usize {
        let media = 0x240 + index * 16;
        let first = u32::from_le_bytes(header[media..media + 4].try_into().ok()?) as u64 * 512;
        let last = u32::from_le_bytes(header[media + 4..media + 8].try_into().ok()?) as u64 * 512;
        let fs = 0x400 + index * 0x200;
        if first >= last
            || last > length
            || header[fs + 3] != 3
            || header[fs + 4] != 3
            || &header[fs + 8..fs + 12] != b"IVFC"
        {
            continue;
        }
        let levels = u32::from_le_bytes(header[fs + 20..fs + 24].try_into().ok()?) as usize;
        // IVFC's `num_levels` includes its master level (normally 7), while
        // only six on-disk level descriptors are present in the FS header.
        if !(1..=7).contains(&levels) {
            continue;
        }
        let level_header = fs + 8 + 16 + (levels - 1).min(5) * 24;
        let romfs_offset =
            u64::from_le_bytes(header[level_header..level_header + 8].try_into().ok()?);
        let romfs_size = u64::from_le_bytes(
            header[level_header + 8..level_header + 16]
                .try_into()
                .ok()?,
        );
        let romfs_start = first.checked_add(romfs_offset)?;
        if romfs_size < 0x50 || romfs_start.checked_add(romfs_size)? > last {
            continue;
        }
        let nonce = section_nonce(&header, fs);
        let romfs = decrypt_ctr_range(reader, start, length, romfs_start, 0x50, nonce, &cipher)?;
        if u64::from_le_bytes(romfs[..8].try_into().ok()?) != 0x50 {
            continue;
        }
        let file_meta_offset = u64::from_le_bytes(romfs[0x38..0x40].try_into().ok()?);
        let file_meta_size = u64::from_le_bytes(romfs[0x40..0x48].try_into().ok()?);
        let file_data_offset = u64::from_le_bytes(romfs[0x48..0x50].try_into().ok()?);
        let dir_meta_offset = u64::from_le_bytes(romfs[0x18..0x20].try_into().ok()?);
        let dir_meta_size = u64::from_le_bytes(romfs[0x20..0x28].try_into().ok()?);
        if file_meta_size < 0x20
            || file_meta_size > PFS0_LIMIT as u64
            || file_meta_offset.checked_add(file_meta_size)? > romfs_size
            || dir_meta_size < 0x18
            || dir_meta_offset.checked_add(0x18)? > romfs_size
        {
            continue;
        }
        // RomFS metadata is not ordered by directory traversal. The linear
        // first `control.nacp` might be from a child folder containing a
        // Switch 2 Edition name instead of hactool's root application name.
        // Follow the root directory's `file` pointer and sibling linked list,
        // exactly as the legacy RomFS directory reader does.
        let root = decrypt_ctr_range(
            reader,
            start,
            length,
            romfs_start.checked_add(dir_meta_offset)?,
            0x18,
            nonce,
            &cipher,
        )?;
        let root_file_ptr = u32::from_le_bytes(root[12..16].try_into().ok()?) as usize;
        let table = decrypt_ctr_range(
            reader,
            start,
            length,
            romfs_start.checked_add(file_meta_offset)?,
            file_meta_size as usize,
            nonce,
            &cipher,
        )?;
        if let Some((file_offset, file_len)) = root_nacp_entry(&table, root_file_ptr) {
            let min_len = 15 * 0x300u64;
            if file_len < min_len || file_len > 0x10000 {
                return None;
            }
            let position = file_data_offset.checked_add(file_offset)?;
            if position.checked_add(min_len)? > romfs_size {
                return None;
            }
            let bytes = decrypt_ctr_range(
                reader,
                start,
                length,
                romfs_start.checked_add(position)?,
                min_len as usize,
                nonce,
                &cipher,
            )?;
            return crate::scanner::nacp_names_from_bytes(&bytes);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn romfs_nacp_uses_root_directory_chain_not_first_flat_entry() {
        // Two identically named files represent the original game (root) and
        // an optional remaster (nested directory); the nested entry occupies
        // the first slot. The original hactool chooses the root name.
        let mut table = vec![0u8; 0xB0];
        for (position, file_offset) in [(0usize, 0x100u64), (0x50, 0x800u64)] {
            table[position + 4..position + 8].copy_from_slice(&u32::MAX.to_le_bytes());
            table[position + 8..position + 16].copy_from_slice(&file_offset.to_le_bytes());
            table[position + 16..position + 24].copy_from_slice(&(0x4000u64).to_le_bytes());
            table[position + 0x1c..position + 0x20].copy_from_slice(&12u32.to_le_bytes());
            table[position + 0x20..position + 0x2C].copy_from_slice(b"control.nacp");
        }
        assert_eq!(root_nacp_entry(&table, 0x50), Some((0x800, 0x4000)));
        assert_eq!(root_nacp_entry(&table, 0), Some((0x100, 0x4000)));
        assert_eq!(root_nacp_entry(&table, u32::MAX as usize), None);
        assert_eq!(root_nacp_entry(&table, 0xFFFF), None);
    }

    #[test]
    fn crypto_bounds_and_key_parsing() {
        assert_eq!(decode_key::<2>("0aFf"), Some([10, 255]));
        assert!(decode_key::<2>("nothex").is_none());
        let keys = Keys {
            header_key: [0; 32],
            key_area: HashMap::new(),
        };
        let mut source = std::io::Cursor::new(vec![0u8; 0xD00]);
        assert!(content_kind(&mut source, 0, 0xD00, &keys).is_none());
        let cipher = Aes128::new_from_slice(&[0u8; 16]).unwrap();
        assert!(decrypt_ctr_range(&mut source, 0, 0xD00, 0xD00, 1, [0; 8], &cipher).is_none());
        assert!(
            decrypt_ctr_range(&mut source, 0, 0xD00, 0, PFS0_LIMIT + 1, [0; 8], &cipher).is_none()
        );
    }
}
