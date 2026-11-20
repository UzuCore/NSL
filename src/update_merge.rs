//! Atomic, non-destructive native PFS0 update-package merge.
//!
//! Only uncompressed NSP + NSP is supported here. NSZ/XCZ must go through
//! NSRP's separate compression/partition engines; emitting a renamed NSP as
//! an NSZ or silently dropping XCI partitions would be incorrect.

use crate::{container_index, scanner};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Clone)]
struct Part {
    source: usize,
    start: u64,
    len: u64,
    name: String,
}

fn hash_part(input: &mut File, start: u64, len: u64) -> Result<[u8; 32]> {
    input.seek(SeekFrom::Start(start))?;
    let mut limited = input.take(len);
    let mut sha = Sha256::new();
    let mut consumed = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha.update(&buffer[..count]);
        consumed += count as u64;
    }
    anyhow::ensure!(consumed == len, "중복 패키지 엔트리가 잘렸습니다");
    Ok(sha.finalize().into())
}

fn plan_inputs(inputs: &mut [File; 2]) -> Result<Vec<Part>> {
    let mut merged: Vec<Part> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for source in 0..2 {
        let index = {
            let input = &mut inputs[source];
            let end = input.metadata()?.len();
            container_index::read_at(input, 0, end, Some(container_index::Kind::Pfs0))
                .context("원본/업데이트 NSP의 PFS0 인덱스가 올바르지 않습니다")?
        };
        for item in index.entries {
            anyhow::ensure!(
                !item.name.to_ascii_lowercase().ends_with(".ncz"),
                "압축된 NCZ 엔트리는 일반 NSP 병합에 사용할 수 없습니다"
            );
            let start = index
                .data_start
                .checked_add(item.offset)
                .context("NSP 파일 오프셋이 범위를 초과했습니다")?;
            let part = Part {
                source,
                start,
                len: item.size,
                name: item.name,
            };
            let key = part.name.to_ascii_lowercase();
            if let Some(old_index) = seen.get(&key) {
                let old = &merged[*old_index];
                anyhow::ensure!(
                    old.len == part.len,
                    "중복된 NCA 이름의 크기가 다릅니다: {}",
                    part.name
                );
                let (first, second) = inputs.split_at_mut(1);
                let first = &mut first[0];
                let second = &mut second[0];
                let same = match (old.source, part.source) {
                    (0, 1) => {
                        hash_part(first, old.start, old.len)?
                            == hash_part(second, part.start, part.len)?
                    }
                    _ => anyhow::bail!("동일 패키지 안에 중복된 엔트리가 있습니다"),
                };
                anyhow::ensure!(same, "이름이 같지만 내용이 다른 NCA: {}", part.name);
                continue;
            }
            seen.insert(key, merged.len());
            merged.push(part);
        }
    }
    anyhow::ensure!(
        merged.len() >= 2 && merged.len() <= 16_384,
        "병합할 NSP 콘텐츠 수가 잘못되었습니다"
    );
    Ok(merged)
}

fn emit_pfs0(files: &mut [File; 2], parts: &[Part], output: &mut File) -> Result<()> {
    let mut strings = Vec::new();
    let mut table = Vec::with_capacity(parts.len() * 24);
    let mut offset = 0u64;
    for part in parts {
        let str_offset = u32::try_from(strings.len()).context("PFS0 문자열 테이블 초과")?;
        strings.extend_from_slice(part.name.as_bytes());
        strings.push(0);
        table.extend_from_slice(&offset.to_le_bytes());
        table.extend_from_slice(&part.len.to_le_bytes());
        table.extend_from_slice(&str_offset.to_le_bytes());
        table.extend_from_slice(&0u32.to_le_bytes());
        offset = offset
            .checked_add(part.len)
            .context("병합 출력 크기 초과")?;
    }
    let count = u32::try_from(parts.len())?;
    let string_len = u32::try_from(strings.len())?;
    output.write_all(b"PFS0")?;
    output.write_all(&count.to_le_bytes())?;
    output.write_all(&string_len.to_le_bytes())?;
    output.write_all(&0u32.to_le_bytes())?;
    output.write_all(&table)?;
    output.write_all(&strings)?;
    for part in parts {
        let input = &mut files[part.source];
        input.seek(SeekFrom::Start(part.start))?;
        let copied = std::io::copy(&mut input.take(part.len), output)?;
        anyhow::ensure!(
            copied == part.len,
            "병합 중 원본 파일이 변경되었거나 잘렸습니다"
        );
    }
    output.flush()?;
    output.sync_all()?;
    Ok(())
}

/// Construct a distinct file next to the original. Originals and downloads
/// are read-only, a staging file is deleted on failure, and existing output
/// names are never overwritten.
pub(crate) fn merge_verified_nsp(
    original: &Path,
    patch: &Path,
    title_id: &str,
    version: i64,
) -> Result<PathBuf> {
    anyhow::ensure!(
        original
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("nsp"))
            && patch
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("nsp")),
        "NSP와 NSP 파일만 이 방식으로 병합할 수 있습니다"
    );
    anyhow::ensure!(original != patch, "입력 파일 경로가 중복됐습니다");
    let parent = original
        .parent()
        .context("기본 게임 폴더를 알 수 없습니다")?;
    let output = parent.join(format!("[NSL Merged] [{title_id}][v{version}].nsp"));
    anyhow::ensure!(
        !output.exists(),
        "이미 같은 버전의 병합 결과 파일이 있습니다: {}",
        output.display()
    );
    let mut files = [File::open(original)?, File::open(patch)?];
    let parts = plan_inputs(&mut files)?;
    let mut staging = tempfile::Builder::new()
        .prefix(".nsl-merging-")
        .suffix(".nsp")
        .tempfile_in(parent)?;
    emit_pfs0(&mut files, &parts, staging.as_file_mut())?;

    // Verify the *new container*, including its native update metadata,
    // before exposing it as a user-visible NSP.
    let file = staging.path();
    let scan = scanner::scan_file_hashed(file, "merged.nsp", scanner::fast_hash(file)?)?;
    let game = u64::from_str_radix(title_id, 16)? & !0x1fff;
    let patch_id = format!("{:016X}", game | 0x800);
    anyhow::ensure!(
        scan.contents
            .iter()
            .any(|c| c.title_id.as_deref() == Some(patch_id.as_str())
                && c.content_type == "update"
                && c.version >= version
                && matches!(c.source.as_str(), "header_native" | "header_cnmt")),
        "병합 파일의 업데이트 CNMT를 검증하지 못했습니다"
    );
    staging.persist_noclobber(&output).map_err(|error| {
        anyhow::anyhow!(
            "원본 보호를 위해 병합 파일 저장을 중단했습니다: {}",
            error.error
        )
    })?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pfs0_merge_preserves_entries_without_overwriting_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = File::create(dir.path().join("base.nsp")).unwrap();
        let mut second = File::create(dir.path().join("patch.nsp")).unwrap();
        let raw = |file: &mut File, name: &str, content: &[u8]| {
            file.write_all(b"PFS0").unwrap();
            file.write_all(&1u32.to_le_bytes()).unwrap();
            file.write_all(&u32::try_from(name.len() + 1).unwrap().to_le_bytes())
                .unwrap();
            file.write_all(&0u32.to_le_bytes()).unwrap();
            file.write_all(&0u64.to_le_bytes()).unwrap();
            file.write_all(&(content.len() as u64).to_le_bytes())
                .unwrap();
            file.write_all(&0u32.to_le_bytes()).unwrap();
            file.write_all(&0u32.to_le_bytes()).unwrap();
            file.write_all(name.as_bytes()).unwrap();
            file.write_all(&[0]).unwrap();
            file.write_all(content).unwrap();
        };
        raw(&mut first, "a.nca", &[1; 32]);
        raw(&mut second, "b.nca", &[2; 48]);
        drop(first);
        drop(second);
        let mut src = [
            File::open(dir.path().join("base.nsp")).unwrap(),
            File::open(dir.path().join("patch.nsp")).unwrap(),
        ];
        let original = [
            std::fs::read(dir.path().join("base.nsp")).unwrap(),
            std::fs::read(dir.path().join("patch.nsp")).unwrap(),
        ];
        let plan = plan_inputs(&mut src).unwrap();
        let mut out = File::create(dir.path().join("merged.nsp")).unwrap();
        emit_pfs0(&mut src, &plan, &mut out).unwrap();
        drop(out);
        let mut output = File::open(dir.path().join("merged.nsp")).unwrap();
        let end = output.metadata().unwrap().len();
        let index =
            container_index::read_at(&mut output, 0, end, Some(container_index::Kind::Pfs0))
                .unwrap();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(
            (
                index.entries[0].name.as_str(),
                index.entries[1].name.as_str()
            ),
            ("a.nca", "b.nca")
        );
        assert_eq!(
            std::fs::read(dir.path().join("base.nsp")).unwrap(),
            original[0]
        );
        assert_eq!(
            std::fs::read(dir.path().join("patch.nsp")).unwrap(),
            original[1]
        );
    }
}
