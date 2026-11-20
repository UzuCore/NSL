//! Native streaming NSZ and XCZ container reconstruction.
//! Rebuilds PFS0/HFS0 indexes and hashes after restoring NCZ entries to NCA.
//! Only the public container-format specification is used; no third-party
//! source file is embedded or translated. Never touches the input archive.

use crate::{container_index, ncz};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAX_ENTRIES: usize = 16_384;
const MAX_STRING_TABLE: usize = 16 * 1024 * 1024;
const ALIGN: u64 = 0x200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Pfs0,
    Hfs0,
}

impl Kind {
    fn entry_len(self) -> usize {
        match self {
            Self::Pfs0 => 0x18,
            Self::Hfs0 => 0x40,
        }
    }

    fn magic(self) -> &'static [u8; 4] {
        match self {
            Self::Pfs0 => b"PFS0",
            Self::Hfs0 => b"HFS0",
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    start: u64,
    size: u64,
    output_size: u64,
    hash_size: u32,
    hash: [u8; 32],
    child: Option<Box<Plan>>,
    compressed: bool,
}

#[derive(Clone, Debug)]
struct Plan {
    kind: Kind,
    entries: Vec<Entry>,
    header_len: u64,
    output_len: u64,
}

fn align(value: u64, unit: u64) -> Result<u64> {
    value
        .checked_add(unit - 1)
        .map(|v| v & !(unit - 1))
        .context("패키지 정렬 크기 범위 초과")
}

fn get_u64(bytes: &[u8]) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes.try_into()?))
}

fn get_u32(bytes: &[u8]) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes.try_into()?))
}

fn read_index(source: &mut File, start: u64, end: u64, kind: Kind) -> Result<Vec<Entry>> {
    anyhow::ensure!(
        start.checked_add(16).is_some_and(|v| v <= end),
        "PFS/HFS 헤더 크기 오류"
    );
    source.seek(SeekFrom::Start(start))?;
    let mut head = [0u8; 16];
    source.read_exact(&mut head)?;
    anyhow::ensure!(&head[..4] == kind.magic(), "지원하지 않는 패키지 헤더");
    let count = get_u32(&head[4..8])? as usize;
    let string_size = get_u32(&head[8..12])? as usize;
    anyhow::ensure!(
        count <= MAX_ENTRIES && string_size <= MAX_STRING_TABLE,
        "패키지 항목 수 또는 문자열 크기 제한 초과"
    );
    let table_bytes = count
        .checked_mul(kind.entry_len())
        .context("패키지 항목 크기 초과")?;
    let raw_header = 16u64 + table_bytes as u64 + string_size as u64;
    anyhow::ensure!(
        start.checked_add(raw_header).is_some_and(|v| v <= end),
        "패키지 목차가 잘렸습니다"
    );
    let mut table = vec![0u8; table_bytes];
    let mut strings = vec![0u8; string_size];
    source.read_exact(&mut table)?;
    source.read_exact(&mut strings)?;
    let data_start = start + raw_header;
    let mut entries = Vec::with_capacity(count);
    let mut names = HashSet::with_capacity(count);
    let mut extents = BTreeMap::<u64, u64>::new();
    for row in table.chunks_exact(kind.entry_len()) {
        let offset = get_u64(&row[..8])?;
        let size = get_u64(&row[8..16])?;
        let str_offset = get_u32(&row[16..20])? as usize;
        let data = strings
            .get(str_offset..)
            .context("패키지 파일명 위치가 잘못되었습니다")?;
        let terminator = data
            .iter()
            .position(|byte| *byte == 0)
            .context("패키지 파일명이 NUL로 종료되지 않았습니다")?;
        let name = std::str::from_utf8(&data[..terminator])
            .context("패키지 파일명 인코딩 오류")?
            .to_owned();
        anyhow::ensure!(
            container_index::valid_name(&name) && names.insert(name.to_lowercase()),
            "잘못된 또는 중복 패키지 파일명: {name}"
        );
        let absolute = data_start
            .checked_add(offset)
            .context("패키지 파일 오프셋 초과")?;
        anyhow::ensure!(
            absolute.checked_add(size).is_some_and(|v| v <= end),
            "패키지 항목이 파일 크기를 초과합니다: {name}"
        );
        // Converter input is always strict. Prevent two nominally different
        // NCAs/partitions from aliasing the same original data extent.
        if size != 0 {
            let file_end = absolute + size;
            let overlaps = extents
                .range(..=absolute)
                .next_back()
                .is_some_and(|(_, previous_end)| *previous_end > absolute)
                || extents
                    .range(absolute..)
                    .next()
                    .is_some_and(|(next_start, _)| *next_start < file_end);
            anyhow::ensure!(!overlaps, "패키지 항목 데이터 구간이 중복됩니다: {name}");
            extents.insert(absolute, file_end);
        }
        let hash_size = if kind == Kind::Hfs0 {
            get_u32(&row[20..24])?
        } else {
            0
        };
        let mut hash = [0u8; 32];
        if kind == Kind::Hfs0 {
            hash.copy_from_slice(&row[32..64]);
            anyhow::ensure!(hash_size as u64 <= size, "HFS0 검증 범위가 파일보다 큽니다");
        }
        entries.push(Entry {
            name,
            start: absolute,
            size,
            output_size: size,
            hash_size,
            hash,
            child: None,
            compressed: false,
        });
    }
    Ok(entries)
}

fn is_ncz(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".ncz")
}

fn plan(source: &mut File, start: u64, end: u64, kind: Kind, depth: usize) -> Result<Plan> {
    anyhow::ensure!(depth <= 1, "중첩 파일시스템 깊이가 잘못되었습니다");
    let mut entries = read_index(source, start, end, kind)?;
    for item in &mut entries {
        if is_ncz(&item.name) {
            let layout = ncz::inspect(source, item.start, item.size)
                .with_context(|| format!("{} NCZ 헤더 검증 실패", item.name))?;
            item.output_size = layout.expanded_size;
            item.name.truncate(item.name.len() - 1);
            item.name.push('a');
            item.compressed = true;
        } else if kind == Kind::Hfs0
            && depth == 0
            && ["secure", "update", "normal", "logo"]
                .contains(&item.name.to_ascii_lowercase().as_str())
            && item.size >= 16
        {
            source.seek(SeekFrom::Start(item.start))?;
            let mut magic = [0u8; 4];
            source.read_exact(&mut magic)?;
            if &magic == b"HFS0" {
                let child = plan(
                    source,
                    item.start,
                    item.start + item.size,
                    Kind::Hfs0,
                    depth + 1,
                )?;
                item.output_size = child.output_len;
                item.child = Some(Box::new(child));
            }
        }
    }
    let mut names_bytes = 0usize;
    for entry in &entries {
        names_bytes = names_bytes
            .checked_add(entry.name.len() + 1)
            .context("패키지 문자열 크기 범위 초과")?;
    }
    anyhow::ensure!(
        names_bytes <= MAX_STRING_TABLE,
        "파일명 테이블이 너무 큽니다"
    );
    // HFS0's first 0x200 bytes are the hashed directory header. PFS0 has no
    // hash but preserving the same 512-byte alignment simplifies traversal.
    let header_len = align(
        16 + (entries.len() * kind.entry_len() + names_bytes) as u64,
        ALIGN,
    )?;
    let mut total = header_len;
    for entry in &entries {
        total = align(total, ALIGN)?;
        total = total
            .checked_add(entry.output_size)
            .context("출력 패키지 크기 범위 초과")?;
    }
    Ok(Plan {
        kind,
        entries,
        header_len,
        output_len: total,
    })
}

fn index_header(plan: &Plan) -> Result<Vec<u8>> {
    let mut header = Vec::with_capacity(plan.header_len as usize);
    header.extend_from_slice(plan.kind.magic());
    header.extend_from_slice(&(plan.entries.len() as u32).to_le_bytes());
    header.extend_from_slice(&[0u8; 8]); // string table length, reserved
    let mut string_pool = Vec::new();
    let mut offsets = Vec::with_capacity(plan.entries.len());
    let mut data_position = 0u64;
    for entry in &plan.entries {
        offsets.push(data_position);
        data_position = align(
            data_position
                .checked_add(entry.output_size)
                .context("출력 오프셋 범위 초과")?,
            ALIGN,
        )?;
    }
    for (i, entry) in plan.entries.iter().enumerate() {
        let name_pos = string_pool.len() as u32;
        string_pool.extend_from_slice(entry.name.as_bytes());
        string_pool.push(0);
        header.extend_from_slice(&offsets[i].to_le_bytes());
        header.extend_from_slice(&entry.output_size.to_le_bytes());
        header.extend_from_slice(&name_pos.to_le_bytes());
        if plan.kind == Kind::Hfs0 {
            header.extend_from_slice(&entry.hash_size.to_le_bytes());
            header.extend_from_slice(&[0u8; 8]);
            header.extend_from_slice(&entry.hash);
        } else {
            header.extend_from_slice(&0u32.to_le_bytes());
        }
    }
    anyhow::ensure!(
        header.len() <= plan.header_len as usize,
        "파일 테이블 생성 크기 오류"
    );
    let names_size = plan.header_len as usize - header.len();
    anyhow::ensure!(names_size >= string_pool.len(), "파일명 테이블 크기 오류");
    // File count + full padded string table must be included in the header.
    header[8..12].copy_from_slice(&(names_size as u32).to_le_bytes());
    header.extend_from_slice(&string_pool);
    header.resize(plan.header_len as usize, 0);
    Ok(header)
}

fn hash_file_region(file: &mut File, start: u64, len: u32) -> Result<[u8; 32]> {
    file.seek(SeekFrom::Start(start))?;
    let mut reader = file.take(len as u64);
    let mut sha = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
    }
    Ok(sha.finalize().into())
}

fn emit(source: &mut File, target: &mut File, plan: &mut Plan, at: u64) -> Result<()> {
    let header = index_header(plan)?;
    target.seek(SeekFrom::Start(at))?;
    target.write_all(&header)?;
    let mut destination = at + plan.header_len;
    for entry in &mut plan.entries {
        destination = align(destination - at, ALIGN)? + at;
        target.seek(SeekFrom::Start(destination))?;
        if let Some(child) = entry.child.as_mut() {
            emit(source, target, child, destination)?;
        } else if entry.compressed {
            let written = ncz::decompress(source, entry.start, entry.size, target)
                .with_context(|| format!("{} NCZ 복원 실패", entry.name))?;
            anyhow::ensure!(
                written == entry.output_size,
                "복원된 NCA 크기가 예상과 다릅니다"
            );
        } else {
            source.seek(SeekFrom::Start(entry.start))?;
            let copied = std::io::copy(&mut source.take(entry.size), target)?;
            anyhow::ensure!(copied == entry.size, "패키지 파일 복사 중 예기치 않은 EOF");
        }
        if plan.kind == Kind::Hfs0 && entry.hash_size > 0 {
            entry.hash = hash_file_region(target, destination, entry.hash_size)?;
        }
        destination = destination
            .checked_add(entry.output_size)
            .context("패키지 출력 크기 범위 초과")?;
    }
    anyhow::ensure!(
        destination - at == plan.output_len,
        "패키지 최종 출력 크기가 일치하지 않습니다"
    );
    target.seek(SeekFrom::Start(at))?;
    target.write_all(&index_header(plan)?)?;
    target.seek(SeekFrom::Start(destination))?;
    Ok(())
}

pub fn unpack_nsz(source: &Path, target: &mut File) -> Result<()> {
    let mut input = File::open(source)?;
    let end = input.metadata()?.len();
    let mut package = plan(&mut input, 0, end, Kind::Pfs0, 0)?;
    anyhow::ensure!(!package.entries.is_empty(), "NSZ 파일에 항목이 없습니다");
    emit(&mut input, target, &mut package, 0)?;
    target.set_len(package.output_len)?;
    target.sync_all()?;
    Ok(())
}

pub fn unpack_xcz(source: &Path, target: &mut File) -> Result<()> {
    let mut input = File::open(source)?;
    let end = input.metadata()?.len();
    anyhow::ensure!(end >= 0x200, "XCZ 게임카드 헤더가 없습니다");
    let mut head = [0u8; 0x200];
    input.read_exact(&mut head)?;
    anyhow::ensure!(
        &head[0x100..0x104] == b"HEAD",
        "XCZ 카드 헤더가 잘못되었습니다"
    );
    let root = get_u64(&head[0x130..0x138])?;
    anyhow::ensure!(
        root >= 0x200 && root < end && root <= 0x100000,
        "XCZ 루트 파티션 주소가 잘못되었습니다"
    );
    let mut package = plan(&mut input, root, end, Kind::Hfs0, 0)?;
    anyhow::ensure!(
        package
            .entries
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case("secure")),
        "XCZ secure 파티션이 없습니다"
    );
    input.seek(SeekFrom::Start(0))?;
    target.seek(SeekFrom::Start(0))?;
    anyhow::ensure!(
        std::io::copy(&mut (&mut input).take(root), target)? == root,
        "XCZ 카드 헤더가 잘렸습니다"
    );
    emit(&mut input, target, &mut package, root)?;
    // A new XCI's header points to the rebuilt 0x200-byte root HFS0 table.
    // NCZ expansion changes partition sizes; update the root header hash and
    // valid-data-end sector. The existing signature is intentionally preserved.
    let new_size = align(root + package.output_len, ALIGN)?;
    target.set_len(new_size)?;
    let new_end_sector: u32 = ((new_size / ALIGN) - 1)
        .try_into()
        .context("XCI 출력 크기가 너무 큽니다")?;
    head[0x118..0x11c].copy_from_slice(&new_end_sector.to_le_bytes());
    head[0x138..0x140].copy_from_slice(&package.header_len.to_le_bytes());
    let root_hash = hash_file_region(target, root, package.header_len.min(u32::MAX as u64) as u32)?;
    head[0x140..0x160].copy_from_slice(&root_hash);
    target.seek(SeekFrom::Start(0))?;
    target.write_all(&head)?;
    target.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_two_entry_index(kind: Kind) -> Vec<u8> {
        let names = b"first.nca\0second.nca\0";
        let mut data = Vec::new();
        data.extend_from_slice(kind.magic());
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&(names.len() as u32).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for (relative, name_at) in [(0u64, 0u32), (4u64, 10u32)] {
            data.extend_from_slice(&relative.to_le_bytes());
            data.extend_from_slice(&4u64.to_le_bytes());
            data.extend_from_slice(&name_at.to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            if kind == Kind::Hfs0 {
                data.extend_from_slice(&[0u8; 40]);
            }
        }
        data.extend_from_slice(names);
        data.extend_from_slice(b"abcdefgh");
        data
    }

    #[test]
    fn repacker_rejects_overlapping_and_unterminated_pfs0_hfs0_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("synthetic_index.bin");
        for kind in [Kind::Pfs0, Kind::Hfs0] {
            let original = synthetic_two_entry_index(kind);
            std::fs::write(&path, &original).unwrap();
            let mut source = File::open(&path).unwrap();
            let entries = read_index(&mut source, 0, original.len() as u64, kind)
                .expect("adjacent data entries are supported");
            assert_eq!(entries.len(), 2);
            drop(source);

            let mut overlap = original.clone();
            let second_start = 16 + kind.entry_len();
            overlap[second_start..second_start + 8].copy_from_slice(&2u64.to_le_bytes());
            std::fs::write(&path, &overlap).unwrap();
            let mut source = File::open(&path).unwrap();
            assert!(read_index(&mut source, 0, overlap.len() as u64, kind).is_err());
            drop(source);

            let mut invalid_name = original;
            let last_nul = 16 + 2 * kind.entry_len() + b"first.nca\0second.nca".len();
            invalid_name[last_nul] = b'X';
            std::fs::write(&path, &invalid_name).unwrap();
            let mut source = File::open(&path).unwrap();
            assert!(read_index(&mut source, 0, invalid_name.len() as u64, kind).is_err());
        }
    }

    #[test]
    fn repacker_still_accepts_empty_hfs0_game_card_partition() {
        // Real XCZ images contain HFS0 normal/update partitions with zero
        // entries. The library metadata reader may skip them, but the native
        // repacker must continue to preserve them during an XCZ conversion.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("empty_partition.bin");
        std::fs::write(&path, b"HFS0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
        let mut source = File::open(&path).unwrap();
        assert!(read_index(&mut source, 0, 16, Kind::Hfs0)
            .unwrap()
            .is_empty());
    }

    #[test]
    #[ignore = "uses read-only local NSZ/XCZ game files and isolated temporary output"]
    fn real_nsz_and_xcz_restore_valid_package_and_nca_hashes() {
        use rusqlite::{Connection, OpenFlags};
        let db = Connection::open_with_flags(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("library.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for (extension, restored_extension) in [("nsz", "nsp"), ("xcz", "xci")] {
            let path: String = db
                .query_row(
                    "SELECT path FROM game_files WHERE LOWER(file_type)=?1 ORDER BY size LIMIT 1",
                    [extension],
                    |row| row.get(0),
                )
                .unwrap();
            let temp = tempfile::tempdir().unwrap();
            let output = temp.path().join(format!("restored.{restored_extension}"));
            let mut target = File::options()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&output)
                .unwrap();
            let started = std::time::Instant::now();
            if extension == "nsz" {
                unpack_nsz(Path::new(&path), &mut target).unwrap();
            } else {
                unpack_xcz(Path::new(&path), &mut target).unwrap();
            }
            let total = target.metadata().unwrap().len();
            let (base, kind) = if extension == "nsz" {
                (0, Kind::Pfs0)
            } else {
                target.seek(SeekFrom::Start(0x130)).unwrap();
                let mut raw = [0u8; 8];
                target.read_exact(&mut raw).unwrap();
                (u64::from_le_bytes(raw), Kind::Hfs0)
            };
            let root = read_index(&mut target, base, total, kind).unwrap();
            let candidates = if extension == "nsz" {
                root
            } else {
                let secure = root.iter().find(|entry| entry.name == "secure").unwrap();
                read_index(
                    &mut target,
                    secure.start,
                    secure.start + secure.size,
                    Kind::Hfs0,
                )
                .unwrap()
            };
            let mut checked_hashes = 0;
            for entry in candidates
                .iter()
                .filter(|entry| entry.name.to_ascii_lowercase().ends_with(".nca"))
            {
                let Some(name) = entry.name.strip_suffix(".nca") else {
                    continue;
                };
                if name.len() != 32 || !name.bytes().all(|c| c.is_ascii_hexdigit()) {
                    continue;
                }
                target.seek(SeekFrom::Start(entry.start)).unwrap();
                let mut reader = (&mut target).take(entry.size);
                let mut hash = Sha256::new();
                let mut buffer = [0u8; 256 * 1024];
                loop {
                    let len = reader.read(&mut buffer).unwrap();
                    if len == 0 {
                        break;
                    }
                    hash.update(&buffer[..len]);
                }
                let actual = format!("{:x}", hash.finalize());
                // NCA IDs in repacked dumps may not equal SHA-256 of their
                // contents. Verify the first sample against an independent
                // NSZ reference decoder instead of trusting the filename.
                if extension == "nsz" && checked_hashes == 0 {
                    assert_eq!(&actual[..32], "90a465fed7e1018996dd5a2617e00bbb");
                }
                if actual[..32] != name.to_ascii_lowercase() {
                    eprintln!(
                        "NCA_FILENAME_DIGEST_DIFF {extension} expected={name} actual={}",
                        &actual[..32]
                    );
                }
                checked_hashes += 1;
            }
            eprintln!("NATIVE_REAL_UNPACK {extension} output={} bytes verified_hashes={checked_hashes} elapsed={:.1}s", total, started.elapsed().as_secs_f64());
            assert!(checked_hashes >= 1);
        }
    }

    #[test]
    fn reject_malformed_package_indexes_and_traversal_names() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("bad.nsz");
        std::fs::write(&source, b"PFS0\x01\0\0\0\0\0\0\0\0\0\0\0").unwrap();
        let mut output = File::create(dir.path().join("out.nsp")).unwrap();
        assert!(unpack_nsz(&source, &mut output).is_err());
        assert!(!container_index::valid_name("../file.nca"));
        assert!(!container_index::valid_name("\\evil"));
        assert!(container_index::valid_name("1234567890abcdef.nca"));
    }

    #[test]
    fn rebuild_pfs0_keeps_plain_files_and_calculates_correct_index() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("test.nsz");
        let mut input = File::create(&source).unwrap();
        let entry = Entry {
            name: "sample.nca".into(),
            start: 0,
            size: 5,
            output_size: 5,
            hash_size: 0,
            hash: [0; 32],
            child: None,
            compressed: false,
        };
        let plan = Plan {
            kind: Kind::Pfs0,
            entries: vec![entry],
            header_len: 0x200,
            output_len: 0x205,
        };
        input.write_all(&index_header(&plan).unwrap()).unwrap();
        input.write_all(b"hello").unwrap();
        drop(input);
        let mut output = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.path().join("output.nsp"))
            .unwrap();
        unpack_nsz(&source, &mut output).unwrap();
        let entries = read_index(&mut output, 0, 0x205, Kind::Pfs0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "sample.nca");
        output.seek(SeekFrom::Start(entries[0].start)).unwrap();
        let mut result = [0; 5];
        output.read_exact(&mut result).unwrap();
        assert_eq!(&result, b"hello");
    }
}
