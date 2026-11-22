use anyhow::{Context, Result, bail};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::NamedTempFile;

const MAX_HFS0_FILES: usize = 16_384;
const MAX_HFS0_STRING_TABLE: usize = 64 * 1024 * 1024;

/// Publish a completed HFS0 without erasing an existing result. Preserve
/// no-clobber semantics even if another process publishes during the write.
fn commit_hfs0(mut staged: NamedTempFile, destination: &Path) -> Result<()> {
    staged.as_file_mut().sync_all()?;
    if let Err(error) = staged.persist_noclobber(destination) {
        let reason = error.error.to_string();
        drop(error.file); // eagerly remove the temp on Windows
        bail!(
            "기존 HFS0 결과물을 덮어쓰지 않았습니다: {} ({reason})",
            destination.display()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hfs0Entry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub hash_target_size: u32,
    pub hash: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct Hfs0Index {
    pub base: u64,
    pub data_start: u64,
    pub entries: Vec<Hfs0Entry>,
}

impl Hfs0Index {
    pub fn parse_at<R: Read + Seek>(reader: &mut R, base: u64, file_size: u64) -> Result<Self> {
        reader.seek(SeekFrom::Start(base))?;
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != b"HFS0" {
            bail!("HFS0 magic 불일치");
        }
        let count = reader.read_u32::<LittleEndian>()?;
        let string_size = reader.read_u32::<LittleEndian>()?;
        let _reserved = reader.read_u32::<LittleEndian>()?;
        if count == 0 || count > 16_384 || string_size > 64 * 1024 * 1024 {
            bail!("비정상 HFS0 헤더");
        }
        // Refuse impossible archive headers before allocating attacker-
        // controlled entry and string tables. The caller's file_size is the
        // absolute end of the containing archive, not a relative length.
        let header_end = base
            .checked_add(16)
            .and_then(|value| value.checked_add(u64::from(count) * 64))
            .and_then(|value| value.checked_add(u64::from(string_size)))
            .context("HFS0 header range overflow")?;
        if header_end > file_size {
            bail!("HFS0 header exceeds container bounds");
        }
        let mut raw = vec![0u8; count as usize * 64];
        reader.read_exact(&mut raw)?;
        let mut strings = vec![0u8; string_size as usize];
        reader.read_exact(&mut strings)?;
        let data_start = reader.stream_position()?;
        let mut entries = Vec::with_capacity(count as usize);
        let mut seen = HashSet::new();
        for i in 0..count as usize {
            let p = &raw[i * 64..(i + 1) * 64];
            let mut c = io::Cursor::new(p);
            let offset = c.read_u64::<LittleEndian>()?;
            let mut size = c.read_u64::<LittleEndian>()?;
            let string_offset = c.read_u32::<LittleEndian>()? as usize;
            let hash_target_size = c.read_u32::<LittleEndian>()?;
            // Entry bytes 0x18..0x1f are reserved. SHA-256 starts at 0x20.
            let _reserved = c.read_u64::<LittleEndian>()?;
            let mut hash = [0u8; 32];
            c.read_exact(&mut hash)?;
            if string_offset >= strings.len() {
                bail!("HFS0 문자열 오프셋 오류");
            }
            let end = strings[string_offset..]
                .iter()
                .position(|&b| b == 0)
                .map(|n| string_offset + n)
                .context("HFS0 파일명 종료 문자 없음")?;
            let name = std::str::from_utf8(&strings[string_offset..end])?.to_owned();
            validate_name(&name)?;
            if !seen.insert(name.to_lowercase()) {
                bail!("중복 HFS0 파일명: {name}");
            }
            let abs_start = data_start
                .checked_add(offset)
                .context("HFS0 오프셋 오버플로")?;
            let abs_end = abs_start.checked_add(size).context("HFS0 크기 오버플로")?;
            if abs_end > file_size {
                let overrun = abs_end - file_size;
                let trimmed_padding = matches!(
                    name.to_ascii_lowercase().as_str(),
                    "secure" | "normal" | "update" | "logo"
                ) && overrun <= 0x200
                    && abs_start < file_size;
                if trimmed_padding {
                    size -= overrun;
                } else {
                    bail!("HFS0 엔트리 범위 초과: {name}");
                }
            }
            entries.push(Hfs0Entry {
                name,
                offset,
                size,
                hash_target_size,
                hash,
            });
        }
        Ok(Self {
            base,
            data_start,
            entries,
        })
    }

    pub fn entry(&self, name: &str) -> Option<&Hfs0Entry> {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }
}

pub fn find_xci_root(path: &Path) -> Result<Hfs0Index> {
    Ok(find_xci_root_with_header_base(path)?.0)
}

pub fn find_xci_root_with_header_base(path: &Path) -> Result<(Hfs0Index, Option<u64>)> {
    let mut file = BufReader::new(File::open(path)?);
    let size = file.get_ref().metadata()?.len();
    let mut candidates: Vec<(u64, Option<u64>)> = Vec::new();
    for base in [0u64, 0x1000, 0x200] {
        if base + 0x140 > size {
            continue;
        }
        file.seek(SeekFrom::Start(base + 0x100))?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        if &magic != b"HEAD" {
            continue;
        }
        file.seek(SeekFrom::Start(base + 0x130))?;
        let addr = file.read_u64::<LittleEndian>()?;
        for candidate in [addr, addr.saturating_mul(0x200)] {
            if candidate > 0
                && candidate < size
                && !candidates.iter().any(|(off, _)| *off == candidate)
            {
                candidates.push((candidate, Some(base)));
            }
        }
    }
    candidates.extend(
        [
            0x200u64, 0x400, 0x1000, 0x2000, 0x4000, 0x8000, 0xf000, 0x10000, 0x20000,
        ]
        .into_iter()
        .map(|off| (off, None)),
    );
    for (off, header_base) in candidates {
        if off + 16 > size {
            continue;
        }
        if let Ok(index) = Hfs0Index::parse_at(&mut file, off, size) {
            return Ok((index, header_base));
        }
    }
    // Last-resort 0x200-aligned scan through the conventional header area.
    for off in (0x200u64..size.min(0x20000)).step_by(0x200) {
        if let Ok(index) = Hfs0Index::parse_at(&mut file, off, size) {
            return Ok((index, None));
        }
    }
    bail!("XCI 루트 HFS0를 찾지 못했습니다")
}

pub fn collect_leaf_entries(
    path: &Path,
    skip_system_partitions: bool,
) -> Result<Vec<(String, u64, u64)>> {
    let mut file = BufReader::new(File::open(path)?);
    let size = file.get_ref().metadata()?.len();
    let root = find_xci_root(path)?;
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    fn walk<R: Read + Seek>(
        reader: &mut R,
        file_size: u64,
        index: Hfs0Index,
        skip_system: bool,
        visited: &mut HashSet<u64>,
        out: &mut Vec<(String, u64, u64)>,
    ) -> Result<()> {
        if !visited.insert(index.base) {
            return Ok(());
        }
        for entry in index.entries {
            let abs = index.data_start + entry.offset;
            let lower = entry.name.to_ascii_lowercase();
            if skip_system && matches!(lower.as_str(), "update" | "logo" | "normal") {
                continue;
            }
            if lower.ends_with(".nca")
                || lower.ends_with(".ncz")
                || lower.ends_with(".tik")
                || lower.ends_with(".cert")
            {
                out.push((entry.name, abs, entry.size));
                continue;
            }
            if abs + 16 <= file_size
                && let Ok(sub) = Hfs0Index::parse_at(reader, abs, file_size)
            {
                walk(reader, file_size, sub, skip_system, visited, out)?;
            }
        }
        Ok(())
    }
    walk(
        &mut file,
        size,
        root,
        skip_system_partitions,
        &mut visited,
        &mut out,
    )?;
    Ok(out)
}

pub fn build_from_paths(entries: &[(String, PathBuf)], out_path: &Path, align: u64) -> Result<()> {
    build_from_paths_layout(entries, out_path, align, align)
}

/// Rebuild an HFS0 hierarchy in a single seekable output without staging each
/// transformed NCA/NCZ or sub-partition. Offsets, payload lengths and SHA-256
/// hashes of the first 0x200 bytes are backfilled after each entry completes.
/// Callers own the output transaction and must discard it on failure.
pub fn build_to_stream<W, F>(
    names: &[String],
    out: &mut W,
    header_align: u64,
    data_align: u64,
    cancel: &AtomicBool,
    mut write_entry: F,
) -> Result<u64>
where
    W: Read + Write + Seek,
    F: FnMut(usize, &mut W) -> Result<()>,
{
    if names.is_empty() || names.len() > 16_384 {
        bail!("HFS0 파일 개수가 비정상입니다");
    }
    let header_align = header_align.max(1);
    let data_align = data_align.max(1);
    let mut strings = Vec::new();
    let mut string_offsets = Vec::with_capacity(names.len());
    let mut seen = HashSet::new();
    for name in names {
        validate_name(name)?;
        if !seen.insert(name.to_lowercase()) {
            bail!("중복 HFS0 파일명: {name}");
        }
        string_offsets.push(u32::try_from(strings.len()).context("HFS0 문자열 테이블 초과")?);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        if strings.len() > 64 * 1024 * 1024 {
            bail!("HFS0 문자열 테이블이 너무 큽니다");
        }
    }
    let raw_header = 16u64
        .checked_add((names.len() as u64) * 64)
        .and_then(|v| v.checked_add(strings.len() as u64))
        .context("HFS0 헤더 크기 오버플로")?;
    let header_size = raw_header
        .checked_add(header_align - 1)
        .map(|v| (v / header_align) * header_align)
        .context("HFS0 헤더 정렬 오버플로")?;
    let padding = usize::try_from(header_size - raw_header)?;
    if padding > 16 * 1024 * 1024 {
        bail!("HFS0 헤더 정렬이 비정상적으로 큽니다");
    }
    strings.resize(strings.len() + padding, 0);

    let header_begin = out.stream_position()?;
    out.write_all(b"HFS0")?;
    out.write_u32::<LittleEndian>(names.len() as u32)?;
    out.write_u32::<LittleEndian>(
        u32::try_from(strings.len()).context("HFS0 문자열 테이블 초과")?,
    )?;
    out.write_u32::<LittleEndian>(0)?;
    out.write_all(&vec![0u8; names.len() * 64])?;
    out.write_all(&strings)?;
    let data_begin = out.stream_position()?;
    let mut written_data = 0u64;
    let mut lengths = Vec::with_capacity(names.len());
    let mut offsets = Vec::with_capacity(names.len());
    let mut hashes = Vec::with_capacity(names.len());
    let mut hash_lengths = Vec::with_capacity(names.len());
    let zero_pad = [0u8; 4096];
    for index in 0..names.len() {
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        let aligned = written_data
            .checked_add(data_align - 1)
            .map(|v| (v / data_align) * data_align)
            .context("HFS0 데이터 정렬 오버플로")?;
        let mut pad = aligned - written_data;
        while pad > 0 {
            let take = pad.min(zero_pad.len() as u64) as usize;
            out.write_all(&zero_pad[..take])?;
            pad -= take as u64;
        }
        let start = out.stream_position()?;
        if start
            != data_begin
                .checked_add(aligned)
                .context("HFS0 오프셋 오버플로")?
        {
            bail!("HFS0 출력 위치가 예상과 다릅니다");
        }
        write_entry(index, out)?;
        let end = out.stream_position()?;
        let size = end.checked_sub(start).context("HFS0 기록 위치 역행")?;
        let hash_size = size.min(0x200) as usize;
        let mut head = [0u8; 0x200];
        out.seek(SeekFrom::Start(start))?;
        out.read_exact(&mut head[..hash_size])?;
        hashes.push(Sha256::digest(&head[..hash_size]));
        out.seek(SeekFrom::Start(end))?;
        offsets.push(aligned);
        lengths.push(size);
        hash_lengths.push(hash_size as u32);
        written_data = aligned
            .checked_add(size)
            .context("HFS0 데이터 크기 오버플로")?;
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("작업이 중단되었습니다");
    }
    let end = out.stream_position()?;
    out.seek(SeekFrom::Start(header_begin + 16))?;
    for index in 0..names.len() {
        out.write_u64::<LittleEndian>(offsets[index])?;
        out.write_u64::<LittleEndian>(lengths[index])?;
        out.write_u32::<LittleEndian>(string_offsets[index])?;
        out.write_u32::<LittleEndian>(hash_lengths[index])?;
        out.write_u64::<LittleEndian>(0)?;
        out.write_all(&hashes[index])?;
    }
    out.seek(SeekFrom::Start(end))?;
    Ok(end - header_begin)
}

pub fn build_from_paths_layout(
    entries: &[(String, PathBuf)],
    out_path: &Path,
    header_align: u64,
    data_align: u64,
) -> Result<()> {
    build_from_paths_layout_cancelled(
        entries,
        out_path,
        header_align,
        data_align,
        &AtomicBool::new(false),
    )
}

/// Atomic HFS0 writer for the XCZ compatibility path. Source lengths,
/// header geometry and names are validated before the first output byte.
/// Cancellation is checked between bounded I/O blocks, and no previously
/// published output is removed or replaced.
pub fn build_from_paths_layout_cancelled(
    entries: &[(String, PathBuf)],
    out_path: &Path,
    header_align: u64,
    data_align: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    if entries.is_empty() || entries.len() > MAX_HFS0_FILES {
        bail!("비정상 HFS0 빌드 파일 수: {}", entries.len());
    }
    let header_align = header_align.max(1);
    let data_align = data_align.max(1);
    let mut strings = Vec::new();
    let mut string_offsets = Vec::with_capacity(entries.len());
    let mut sizes = Vec::with_capacity(entries.len());
    let mut hashes = Vec::with_capacity(entries.len());
    let mut seen = HashSet::with_capacity(entries.len());
    for (name, path) in entries {
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        validate_name(name)?;
        if !seen.insert(name.to_lowercase()) {
            bail!("중복 HFS0 파일명: {name}");
        }
        if strings.len().saturating_add(name.len()).saturating_add(1) > MAX_HFS0_STRING_TABLE {
            bail!("HFS0 문자열 테이블이 너무 큽니다");
        }
        string_offsets.push(strings.len() as u32);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        let size = fs::metadata(path)?.len();
        sizes.push(size);
        let target = size.min(0x200) as usize;
        let mut f = File::open(path)?;
        let mut data = vec![0u8; target];
        f.read_exact(&mut data)?;
        hashes.push(Sha256::digest(&data));
    }
    let raw_header = 16u64
        .checked_add(entries.len() as u64 * 64)
        .and_then(|v| v.checked_add(strings.len() as u64))
        .context("HFS0 헤더 크기 오버플로")?;
    let header_size = align_up_checked(raw_header, header_align)?;
    let header_padding = header_size - raw_header;
    if header_padding > 16 * 1024 * 1024
        || strings.len() as u64 + header_padding > MAX_HFS0_STRING_TABLE as u64
        || data_align > 16 * 1024 * 1024
    {
        bail!("HFS0 정렬 또는 문자열 테이블이 비정상적으로 큽니다");
    }
    strings.resize(strings.len() + header_padding as usize, 0);
    let mut relative_offsets = Vec::with_capacity(entries.len());
    let mut rel = 0u64;
    let mut last_end = 0u64;
    for &size in &sizes {
        relative_offsets.push(rel);
        last_end = rel.checked_add(size).context("HFS0 데이터 크기 오버플로")?;
        rel = align_up_checked(last_end, data_align)?;
    }
    let expected_len = header_size
        .checked_add(last_end)
        .context("HFS0 출력 길이 오버플로")?;
    if cancel.load(Ordering::Relaxed) {
        bail!("작업이 중단되었습니다");
    }
    let parent = out_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    if out_path.exists() {
        bail!(
            "기존 HFS0 결과물을 덮어쓰지 않습니다: {}",
            out_path.display()
        );
    }
    let mut staged = tempfile::Builder::new()
        .prefix(".nsrp-hfs0-")
        .suffix(".partial")
        .tempfile_in(parent)?;
    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, staged.as_file_mut());
    out.write_all(b"HFS0")?;
    out.write_u32::<LittleEndian>(entries.len() as u32)?;
    out.write_u32::<LittleEndian>(strings.len() as u32)?;
    out.write_u32::<LittleEndian>(0)?;
    for i in 0..entries.len() {
        out.write_u64::<LittleEndian>(relative_offsets[i])?;
        out.write_u64::<LittleEndian>(sizes[i])?;
        out.write_u32::<LittleEndian>(string_offsets[i])?;
        out.write_u32::<LittleEndian>(sizes[i].min(0x200) as u32)?;
        out.write_u64::<LittleEndian>(0)?;
        out.write_all(&hashes[i])?;
    }
    out.write_all(&strings)?;
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut written_data = 0u64;
    for ((_, path), &size) in entries.iter().zip(&sizes) {
        if cancel.load(Ordering::Relaxed) {
            bail!("작업이 중단되었습니다");
        }
        if !written_data.is_multiple_of(data_align) {
            let mut pad = data_align - written_data % data_align;
            let zeroes = [0u8; 4096];
            while pad > 0 {
                if cancel.load(Ordering::Relaxed) {
                    bail!("작업이 중단되었습니다");
                }
                let take = pad.min(zeroes.len() as u64) as usize;
                out.write_all(&zeroes[..take])?;
                pad -= take as u64;
                written_data += take as u64;
            }
        }
        let mut src = BufReader::with_capacity(buf.len(), File::open(path)?);
        let mut remaining = size;
        while remaining > 0 {
            if cancel.load(Ordering::Relaxed) {
                bail!("작업이 중단되었습니다");
            }
            let take = remaining.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..take])?;
            out.write_all(&buf[..take])?;
            written_data += take as u64;
            remaining -= take as u64;
        }
    }
    out.flush()?;
    drop(out);
    if staged.as_file().metadata()?.len() != expected_len {
        bail!("HFS0 패키지 출력 크기 검증 실패");
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("작업이 중단되었습니다");
    }
    commit_hfs0(staged, out_path)
}

fn align_up_checked(v: u64, a: u64) -> Result<u64> {
    v.checked_add(a - 1)
        .map(|sum| sum / a * a)
        .context("HFS0 정렬 크기 오버플로")
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        bail!("안전하지 않은 HFS0 파일명: {name:?}");
    }
    Ok(())
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use std::fs::OpenOptions;

    /// Only a user-owned, temporary NTFS sparse fixture is accepted here.
    /// No 36 GiB allocation, scan, or user-library modification is needed.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires isolated Stage36 NTFS sparse fixture created by scripts/qa_stage36_sparse_hfs0.ps1"]
    fn stage36_real_ntfs_sparse_36gib_hfs0() -> Result<()> {
        use std::os::windows::fs::MetadataExt;
        let path = PathBuf::from(
            std::env::var_os("NSL_STAGE36_SPARSE_XCI")
                .context("Stage36 QA sparse fixture path must be provided")?,
        );
        let qa = Path::new(r"E:\NSL_QA_20261120");
        let parent = path.parent().context("missing sparse fixture parent")?;
        anyhow::ensure!(
            parent.starts_with(qa)
                && parent
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("stage36-sparse-hfs0-"))
                && path
                    .file_name()
                    .is_some_and(|name| name == "sparse_36gib.xci"),
            "refusing sparse input outside the owned Stage36 QA fixture"
        );
        let meta = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "sparse fixture must be a real regular file"
        );
        anyhow::ensure!(
            meta.file_attributes() & 0x200 != 0,
            "NTFS sparse flag missing"
        );
        anyhow::ensure!(meta.len() == (36u64 << 30), "incorrect 36 GiB logical size");
        let index = find_xci_root(&path)?;
        assert_eq!(index.base, 512);
        assert_eq!(index.data_start, 600);
        let entry = index
            .entry("big.nca")
            .context("missing sparse HFS0 entry")?;
        assert_eq!(entry.offset, 34u64 << 30);
        assert_eq!(entry.size, 1u64 << 20);
        let absolute = index.data_start + entry.offset;
        assert!(absolute > u64::from(u32::MAX));
        assert_eq!(
            collect_leaf_entries(&path, false)?,
            vec![("big.nca".to_owned(), absolute, 1u64 << 20)]
        );
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(absolute))?;
        let mut marker = [0; 15];
        file.read_exact(&mut marker)?;
        assert_eq!(&marker, b"STAGE36_SPARSE!");
        file.seek(SeekFrom::Start(absolute + (1 << 20) - 4))?;
        let mut tail = [0; 4];
        file.read_exact(&mut tail)?;
        assert_eq!(&tail, b"TAIL");
        assert!(Hfs0Index::parse_at(&mut file, 512, absolute + (1 << 20) - 1).is_err());
        Ok(())
    }

    #[test]
    fn stage36_virtual_36gib_hfs0_bounds_and_overflow() -> Result<()> {
        // 36 GiB is advertised but never allocated or scanned.
        let offset = 34u64 << 30;
        let payload = 24u64 << 20;
        let logical_size = 36u64 << 30;
        let mut header = Vec::new();
        header.extend_from_slice(b"HFS0");
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&10u32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        let mut entry = [0u8; 64];
        entry[..8].copy_from_slice(&offset.to_le_bytes());
        entry[8..16].copy_from_slice(&payload.to_le_bytes());
        header.extend_from_slice(&entry);
        header.extend_from_slice(b"large.nca\0");
        let data_start = header.len() as u64;

        let parsed = Hfs0Index::parse_at(&mut io::Cursor::new(&header), 0, logical_size)?;
        assert_eq!(parsed.data_start, data_start);
        assert_eq!(parsed.entries[0].name, "large.nca");
        assert_eq!(parsed.entries[0].offset, offset);
        assert_eq!(parsed.entries[0].size, payload);
        assert!(
            Hfs0Index::parse_at(
                &mut io::Cursor::new(&header),
                0,
                data_start + offset + payload - 1,
            )
            .is_err()
        );

        // Rejected before the large, untrusted string-table allocation.
        assert!(Hfs0Index::parse_at(&mut io::Cursor::new(&header), 0, data_start - 1,).is_err());
        header[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Hfs0Index::parse_at(&mut io::Cursor::new(&header), 0, u64::MAX).is_err());
        assert!(Hfs0Index::parse_at(&mut io::Cursor::new(&header), u64::MAX, u64::MAX).is_err());
        Ok(())
    }

    #[test]
    fn stage36_hfs0_digest_at_standard_entry_offset() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let payload = vec![0x71u8; 0x500];
        let source = temp.path().join("part.nca");
        fs::write(&source, &payload)?;
        let expected = Sha256::digest(&payload[..0x200]);
        let staged = temp.path().join("staged.hfs0");
        build_from_paths_layout(
            &[("part.nca".to_owned(), source.clone())],
            &staged,
            0x200,
            1,
        )?;
        let mut streaming = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(temp.path().join("streaming.hfs0"))?;
        let names = vec!["part.nca".to_owned()];
        build_to_stream(
            &names,
            &mut streaming,
            0x200,
            1,
            &AtomicBool::new(false),
            |_, output| {
                output.write_all(&payload)?;
                Ok(())
            },
        )?;
        for archive in [staged, temp.path().join("streaming.hfs0")] {
            let bytes = fs::read(&archive)?;
            assert_eq!(&bytes[16 + 24..16 + 32], &[0u8; 8]);
            assert_eq!(&bytes[16 + 32..16 + 64], expected.as_slice());
            let mut file = File::open(&archive)?;
            let index = Hfs0Index::parse_at(&mut file, 0, bytes.len() as u64)?;
            assert_eq!(index.entries[0].hash, expected.as_slice());
            assert_eq!(index.entries[0].hash_target_size, 0x200);
        }
        Ok(())
    }

    fn readwrite(path: &Path) -> Result<File> {
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?)
    }

    #[test]
    fn staged_layout_refuses_existing_output_and_keeps_bytes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("payload.bin");
        let target = temp.path().join("previous.hfs0");
        fs::write(&source, vec![0xa5; 2 * 1024 * 1024 + 77])?;
        fs::write(&target, b"existing published archive")?;
        assert!(
            build_from_paths_layout(&[("main.nca".into(), source)], &target, 0x200, 1).is_err()
        );
        assert_eq!(fs::read(&target)?, b"existing published archive");
        assert_eq!(fs::read_dir(temp.path())?.count(), 2);
        Ok(())
    }

    #[test]
    fn staged_layout_rejects_names_overflow_and_pre_cancel() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("payload.bin");
        let target = temp.path().join("output.hfs0");
        fs::write(&source, b"sample content")?;
        let names = [
            ("GAME.NCA".into(), source.clone()),
            ("game.nca".into(), source.clone()),
        ];
        assert!(build_from_paths_layout(&names, &target, 0x200, 1).is_err());
        assert!(
            build_from_paths_layout(
                &[("normal.nca".into(), source.clone())],
                &target,
                u64::MAX,
                1,
            )
            .is_err()
        );
        assert!(
            build_from_paths_layout(
                &[("normal.nca".into(), source.clone())],
                &target,
                0x200,
                u64::MAX,
            )
            .is_err()
        );
        assert!(
            build_from_paths_layout_cancelled(
                &[("normal.nca".into(), source)],
                &target,
                0x200,
                1,
                &AtomicBool::new(true),
            )
            .is_err()
        );
        assert!(!target.exists());
        assert_eq!(fs::read_dir(temp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn simultaneous_hfs0_writers_publish_exactly_once() -> Result<()> {
        use std::sync::{Arc, Barrier};

        let temp = tempfile::tempdir()?;
        let source = temp.path().join("big.nca");
        let target = temp.path().join("merged.hfs0");
        fs::write(&source, vec![0x71u8; 3 * 1024 * 1024 + 11])?;
        let barrier = Arc::new(Barrier::new(2));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let (source, target, barrier) = (source.clone(), target.clone(), Arc::clone(&barrier));
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                build_from_paths_layout(&[("main.nca".into(), source)], &target, 0x200, 1).is_ok()
            }));
        }
        assert_eq!(
            threads
                .into_iter()
                .map(|t| usize::from(t.join().expect("HFS0 writer panicked")))
                .sum::<usize>(),
            1
        );
        let mut file = File::open(&target)?;
        let size = file.metadata()?.len();
        let index = Hfs0Index::parse_at(&mut file, 0, size)?;
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].size, 3 * 1024 * 1024 + 11);
        assert_eq!(fs::read_dir(temp.path())?.count(), 2);
        Ok(())
    }

    #[test]
    fn streamed_hfs0_matches_previous_staged_builder() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let a = temp.path().join("a.bin");
        let b = temp.path().join("b.bin");
        fs::write(
            &a,
            (0..1234u32).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
        )?;
        fs::write(&b, vec![0x5au8; 37])?;
        let names = vec!["some_game.nca".to_string(), "control.tik".to_string()];
        let paths = vec![(names[0].clone(), a.clone()), (names[1].clone(), b.clone())];
        let staged = temp.path().join("staged.hfs0");
        let streamed = temp.path().join("streamed.hfs0");
        build_from_paths_layout(&paths, &staged, 0x200, 1)?;
        let mut output = readwrite(&streamed)?;
        build_to_stream(
            &names,
            &mut output,
            0x200,
            1,
            &AtomicBool::new(false),
            |i, dst| {
                let mut src = File::open(&paths[i].1)?;
                io::copy(&mut src, dst)?;
                Ok(())
            },
        )?;
        drop(output);
        assert_eq!(fs::read(&staged)?, fs::read(&streamed)?);
        Ok(())
    }

    #[test]
    fn nested_hfs0_stream_is_byte_identical_to_two_stage_build() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let payload = temp.path().join("content.nca");
        fs::write(&payload, vec![0x42u8; 8193])?;
        let partition = temp.path().join("old_partition.hfs0");
        build_from_paths_layout(
            &[("content.nca".into(), payload.clone())],
            &partition,
            0x200,
            1,
        )?;
        let root = temp.path().join("old_root.hfs0");
        build_from_paths_layout(&[("secure".into(), partition)], &root, 0x200, 1)?;
        let nested = temp.path().join("nested.hfs0");
        let mut output = readwrite(&nested)?;
        let cancel = AtomicBool::new(false);
        build_to_stream(
            &["secure".into()],
            &mut output,
            0x200,
            1,
            &cancel,
            |_, out| {
                build_to_stream(&["content.nca".into()], out, 0x200, 1, &cancel, |_, sub| {
                    io::copy(&mut File::open(&payload)?, sub)?;
                    Ok(())
                })?;
                Ok(())
            },
        )?;
        drop(output);
        assert_eq!(fs::read(&root)?, fs::read(&nested)?);
        Ok(())
    }

    #[test]
    fn streamed_hfs0_cancellation_rejects_payload_without_finishing_index() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut output = readwrite(&temp.path().join("cancelled.hfs0"))?;
        let cancel = AtomicBool::new(false);
        let result = build_to_stream(
            &["first.nca".into(), "second.nca".into()],
            &mut output,
            0x200,
            1,
            &cancel,
            |index, out| {
                out.write_all(b"first entry")?;
                if index == 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(cancel.load(Ordering::Relaxed));
        Ok(())
    }

    #[test]
    #[ignore = "sparse 4 GiB+ output test on a filesystem supporting large files"]
    fn streamed_hfs0_handles_sparse_file_larger_than_four_gib() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("large_sparse.hfs0");
        let mut output = readwrite(&path)?;
        let file_size = (4u64 << 30) + 97;
        build_to_stream(
            &["large.nca".into()],
            &mut output,
            0x200,
            1,
            &AtomicBool::new(false),
            |_, out| {
                let start = out.stream_position()?;
                out.write_all(&[0x41; 0x200])?;
                out.seek(SeekFrom::Start(start + file_size - 1))?;
                out.write_all(&[0x5a])?;
                Ok(())
            },
        )?;
        assert_eq!(output.metadata()?.len(), 0x200 + file_size);
        let len = output.metadata()?.len();
        let index = Hfs0Index::parse_at(&mut output, 0, len)?;
        assert_eq!(index.entries[0].size, file_size);
        Ok(())
    }
}
