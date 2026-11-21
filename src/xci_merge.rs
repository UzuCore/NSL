//! Native XCI assembly using a validated source game-card header.
//!
//! This keeps the original card's HEAD region and non-secure partitions,
//! replaces its secure HFS0 with the NCA/ticket set selected by NSRP's CNMT
//! merger, and recalculates the nested HFS0 hashes and root HEAD pointers.
//! The original Nintendo header signature is not regenerated. Emulator and
//! console compatibility need independent end-to-end testing.

use anyhow::{Context, Result};
use nsrp_core::formats::hfs0;
#[cfg(test)]
use nsrp_core::formats::pfs0::Pfs0Index;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Bounded XCI copy that stays cancellable even inside a huge NCA or opaque
/// gamecard partition. Reuse a buffer across all nested output entries.
fn copy_region(
    input: &mut File,
    start: u64,
    len: u64,
    out: &mut File,
    cancel: &AtomicBool,
    buffer: &mut [u8],
) -> Result<()> {
    anyhow::ensure!(!buffer.is_empty(), "XCI 복사 버퍼가 비었습니다");
    let end = start.checked_add(len).context("XCI 복사 크기 오버플로")?;
    anyhow::ensure!(
        end <= input.metadata()?.len(),
        "XCI 파티션 데이터가 원본 파일 범위를 초과합니다"
    );
    input.seek(SeekFrom::Start(start))?;
    let mut remaining = len;
    while remaining > 0 {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "XCI 복사가 취소되었습니다");
        let take = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..take])?;
        out.write_all(&buffer[..take])?;
        remaining -= take as u64;
    }
    Ok(())
}

/// Either a region in the legacy merged NSP or an already selected materialized
/// NCA/ticket. Both paths use the same HFS0 and gamecard HEAD writer.
struct XciContentSource {
    name: String,
    path: PathBuf,
    offset: u64,
    size: u64,
}

#[cfg(test)]
pub(crate) fn build_merged_xci_from_template(
    template: &Path,
    merged_nsp: &Path,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "XCI 병합이 취소되었습니다");
    anyhow::ensure!(
        template != output && merged_nsp != output,
        "입출력 경로 충돌"
    );
    let index = Pfs0Index::parse(merged_nsp).context("CNMT 병합 NSP를 파싱할 수 없습니다")?;
    let sources = index
        .entries
        .iter()
        .map(|entry| -> Result<XciContentSource> {
            Ok(XciContentSource {
                name: entry.name.clone(),
                path: merged_nsp.to_path_buf(),
                offset: index
                    .data_start
                    .checked_add(entry.offset)
                    .context("NSP 엔트리 오프셋 초과")?,
                size: entry.size,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    build_merged_xci_from_sources(template, &sources, output, cancel)
}

/// Assemble the merged XCI directly from NSRP's CNMT-selected files. No
/// full-size PFS0 intermediate is generated, and each source is read once
/// while the callback's temporary workspace is still alive.
pub(crate) fn build_merged_xci_from_selected_files(
    template: &Path,
    files: &[(String, PathBuf)],
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut sources = Vec::with_capacity(files.len());
    for (name, path) in files {
        anyhow::ensure!(path != output, "선택된 XCI 입력이 출력과 같습니다");
        sources.push(XciContentSource {
            name: name.clone(),
            path: path.clone(),
            offset: 0,
            size: std::fs::metadata(path)?.len(),
        });
    }
    build_merged_xci_from_sources(template, &sources, output, cancel)
}

fn build_merged_xci_from_sources(
    template: &Path,
    sources: &[XciContentSource],
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "XCI 병합이 취소되었습니다");
    anyhow::ensure!(template != output, "XCI 템플릿과 출력 경로 충돌");
    anyhow::ensure!(
        sources
            .iter()
            .any(|entry| entry.name.to_ascii_lowercase().ends_with(".nca")),
        "선택된 병합 결과에 NCA가 없습니다"
    );
    let (root, head_base) = hfs0::find_xci_root_with_header_base(template)?;
    let head_base = head_base.context("게임카드 HEAD가 없는 XCI를 변환할 수 없습니다")?;
    anyhow::ensure!(
        root.base >= head_base + 0x200 && root.base <= 0x20000,
        "XCI 루트 파티션 위치가 올바르지 않습니다"
    );
    anyhow::ensure!(
        root.entry("secure").is_some(),
        "게임카드 secure 파티션이 없습니다"
    );

    let inner_names: Vec<String> = sources.iter().map(|entry| entry.name.clone()).collect();
    let root_names: Vec<String> = root
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    let mut template_file = File::open(template)?;
    let mut buffer = vec![0u8; 2 * 1024 * 1024];
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| format!("XCI 출력 파일 생성 실패: {}", output.display()))?;
    // Only the private temporary file is ever partially written. The caller
    // owns the enclosing staging directory and never publishes on failure.
    copy_region(
        &mut template_file,
        0,
        root.base,
        &mut out,
        cancel,
        &mut buffer,
    )?;

    let original_root_data_start = root.data_start;
    out.seek(SeekFrom::Start(root.base))?;
    let _root_size = hfs0::build_to_stream(
        &root_names,
        &mut out,
        0x200,
        0x200,
        cancel,
        |index, output| {
            let partition = &root.entries[index];
            if partition.name.eq_ignore_ascii_case("secure") {
                hfs0::build_to_stream(&inner_names, output, 0x200, 0x200, cancel, |i, target| {
                    let entry = &sources[i];
                    let mut source = File::open(&entry.path)?;
                    copy_region(
                        &mut source,
                        entry.offset,
                        entry.size,
                        target,
                        cancel,
                        &mut buffer,
                    )
                })?;
            } else {
                let start = original_root_data_start
                    .checked_add(partition.offset)
                    .context("XCI 파티션 오프셋 초과")?;
                copy_region(
                    &mut template_file,
                    start,
                    partition.size,
                    output,
                    cancel,
                    &mut buffer,
                )?;
            }
            Ok(())
        },
    )?;
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "XCI 병합이 취소되었습니다");

    let end = out.seek(SeekFrom::End(0))?;
    let padded = end
        .checked_add(0x1ff)
        .map(|value| value & !0x1ff)
        .context("XCI 출력 크기 초과")?;
    out.set_len(padded)?;
    let new_root = hfs0::Hfs0Index::parse_at(&mut out, root.base, padded)
        .context("생성된 XCI 루트 인덱스 검증 실패")?;
    let new_secure = new_root
        .entry("secure")
        .context("새 XCI secure 파티션 없음")?;
    let secure_start = new_root
        .data_start
        .checked_add(new_secure.offset)
        .context("XCI secure 오프셋 초과")?;
    let secure_index = hfs0::Hfs0Index::parse_at(&mut out, secure_start, padded)
        .context("생성된 XCI secure 인덱스 검증 실패")?;
    anyhow::ensure!(
        secure_index.entries.len() == sources.len(),
        "생성된 XCI secure의 콘텐츠 수가 일치하지 않습니다"
    );
    let header_len = new_root
        .data_start
        .checked_sub(root.base)
        .context("XCI 루트 헤더 길이 오류")?;
    anyhow::ensure!(
        header_len <= 16 * 1024 * 1024,
        "XCI 루트 헤더 크기가 비정상입니다"
    );
    let mut root_header = vec![0u8; usize::try_from(header_len)?];
    out.seek(SeekFrom::Start(root.base))?;
    out.read_exact(&mut root_header)?;
    let hash = Sha256::digest(&root_header);

    let mut head = [0u8; 0x200];
    out.seek(SeekFrom::Start(head_base))?;
    out.read_exact(&mut head)?;
    anyhow::ensure!(
        &head[0x100..0x104] == b"HEAD",
        "템플릿 XCI HEAD가 잘못되었습니다"
    );
    let root_pointer = u64::from_le_bytes(head[0x130..0x138].try_into()?);
    anyhow::ensure!(
        root_pointer == root.base,
        "XCI 루트 주소가 HEAD와 일치하지 않습니다"
    );
    let final_sector: u32 = ((padded / 0x200) - 1)
        .try_into()
        .context("XCI 유효 섹터 수가 32비트 범위를 초과했습니다")?;
    head[0x118..0x11c].copy_from_slice(&final_sector.to_le_bytes());
    head[0x138..0x140].copy_from_slice(&header_len.to_le_bytes());
    head[0x140..0x160].copy_from_slice(&hash);
    out.seek(SeekFrom::Start(head_base))?;
    out.write_all(&head)?;
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "XCI 병합이 취소되었습니다");
    out.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_xci_builder_rebuilds_secure_hashes_and_preserves_originals() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("original.xci");
        let merged = temp.path().join("merged.nsp");
        let result = temp.path().join("new.xci");
        let cancel = AtomicBool::new(false);
        let mut source_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source)
            .unwrap();
        let mut template_head = [0u8; 0x400];
        template_head[0x100..0x104].copy_from_slice(b"HEAD");
        template_head[0x130..0x138].copy_from_slice(&0x400u64.to_le_bytes());
        source_file.write_all(&template_head).unwrap();
        hfs0::build_to_stream(
            &["secure".into(), "normal".into()],
            &mut source_file,
            0x200,
            0x200,
            &cancel,
            |index, output| {
                if index == 0 {
                    hfs0::build_to_stream(
                        &["base.nca".into()],
                        output,
                        0x200,
                        0x200,
                        &cancel,
                        |_, f| {
                            f.write_all(&[1u8; 0xC00])?;
                            Ok(())
                        },
                    )?;
                } else {
                    output.write_all(&[0x5Au8; 1800])?;
                }
                Ok(())
            },
        )
        .unwrap();
        source_file.sync_all().unwrap();
        drop(source_file);
        // Minimal PFS0 with two dummy NCAs; no real keys or ROMs used.
        let names = ["base.nca", "patch.cnmt.nca"];
        let mut raw = vec![0u8; 16];
        raw[..4].copy_from_slice(b"PFS0");
        raw[4..8].copy_from_slice(&2u32.to_le_bytes());
        let str_bytes = names.iter().map(|s| s.len() + 1).sum::<usize>();
        raw[8..12].copy_from_slice(&(str_bytes as u32).to_le_bytes());
        for index in 0..names.len() {
            raw.extend_from_slice(&((index * 0xC00) as u64).to_le_bytes());
            raw.extend_from_slice(&(0xC00u64).to_le_bytes());
            raw.extend_from_slice(
                &(if index == 0 {
                    0u32
                } else {
                    (names[0].len() + 1) as u32
                })
                .to_le_bytes(),
            );
            raw.extend_from_slice(&0u32.to_le_bytes());
        }
        for name in names {
            raw.extend_from_slice(name.as_bytes());
            raw.push(0);
        }
        raw.extend_from_slice(&vec![3u8; 0x1800]);
        std::fs::write(&merged, &raw).unwrap();
        let original = std::fs::read(&source).unwrap();
        build_merged_xci_from_template(&source, &merged, &result, &cancel).unwrap();
        let root = hfs0::find_xci_root(&result).unwrap();
        let mut new_file = File::open(&result).unwrap();
        let root_head_len = root.data_start - root.base;
        let mut root_header = vec![0u8; root_head_len as usize];
        new_file.seek(SeekFrom::Start(root.base)).unwrap();
        new_file.read_exact(&mut root_header).unwrap();
        let mut head = [0u8; 0x200];
        new_file.seek(SeekFrom::Start(0)).unwrap();
        new_file.read_exact(&mut head).unwrap();
        assert_eq!(&head[0x140..0x160], &Sha256::digest(&root_header)[..]);
        let secure = root.entry("secure").unwrap();
        let new_file_size = new_file.metadata().unwrap().len();
        let sub = hfs0::Hfs0Index::parse_at(
            &mut new_file,
            root.data_start + secure.offset,
            new_file_size,
        )
        .unwrap();
        assert_eq!(sub.entries.len(), 2);
        assert_eq!(sub.entries[0].name, "base.nca");
        assert_eq!(sub.entries[1].name, "patch.cnmt.nca");
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read(&merged).unwrap(), raw);
        // Stage 10 must produce exactly the same XCI with selected files,
        // without building or reading the legacy giant merged NSP.
        let selected_base = temp.path().join("selected_base.nca");
        let selected_patch = temp.path().join("selected_patch.cnmt.nca");
        std::fs::write(&selected_base, [3u8; 0xC00]).unwrap();
        std::fs::write(&selected_patch, [3u8; 0xC00]).unwrap();
        let direct = temp.path().join("direct_selected.xci");
        build_merged_xci_from_selected_files(
            &source,
            &[
                ("base.nca".into(), selected_base),
                ("patch.cnmt.nca".into(), selected_patch),
            ],
            &direct,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(&direct).unwrap(),
            std::fs::read(&result).unwrap()
        );

        // Stage31: a retry must never overwrite a previously published XCI.
        // Keep the previously generated, structurally valid template and
        // materialized inputs untouched even when the destination exists.
        let existing = temp.path().join("existing-output.xci");
        let sentinel = b"user-owned output: do not overwrite";
        std::fs::write(&existing, sentinel).unwrap();
        assert!(build_merged_xci_from_template(&source, &merged, &existing, &cancel,).is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), sentinel);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read(&merged).unwrap(), raw);

        // Missing selected content must not create or truncate a destination.
        let missing_output = temp.path().join("missing-input.xci");
        assert!(build_merged_xci_from_selected_files(
            &source,
            &[("missing.nca".into(), temp.path().join("missing.nca"))],
            &missing_output,
            &cancel,
        )
        .is_err());
        assert!(!missing_output.exists());
    }

    #[test]
    fn bounded_large_region_copy_preserves_bytes_and_rejects_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("large_source.bin");
        let destination = temp.path().join("copied.bin");
        let bytes = (0..(7 * 1024 * 1024 + 137))
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&source, &bytes).unwrap();
        let mut input = File::open(&source).unwrap();
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .unwrap();
        let cancel = AtomicBool::new(false);
        let mut scratch = vec![0u8; 256 * 1024];
        let start = 173;
        let len = bytes.len() as u64 - 319;
        copy_region(&mut input, start, len, &mut output, &cancel, &mut scratch).unwrap();
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            bytes[start as usize..start as usize + len as usize]
        );
        let before = output.metadata().unwrap().len();
        cancel.store(true, Ordering::Relaxed);
        assert!(copy_region(&mut input, 0, 1024, &mut output, &cancel, &mut scratch).is_err());
        assert_eq!(output.metadata().unwrap().len(), before);

        cancel.store(false, Ordering::Relaxed);
        assert!(copy_region(
            &mut input,
            bytes.len() as u64 - 20,
            200,
            &mut output,
            &cancel,
            &mut scratch,
        )
        .is_err());
        assert_eq!(output.metadata().unwrap().len(), before);
    }

    #[test]
    fn cancelled_xci_builder_does_not_create_output() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("must_not_exist.xci");
        let cancel = AtomicBool::new(true);
        assert!(build_merged_xci_from_template(
            &temp.path().join("absent.xci"),
            &temp.path().join("absent.nsp"),
            &output,
            &cancel,
        )
        .is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}
