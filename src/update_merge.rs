//! Safe NSRP-backed game patch merger, with native XCI assembly.
//!
//! The original title and downloaded update are *read-only*. NSRP operates in
//! private workspaces, produces a CNMT-selected merged NSP, and optionally
//! recompresses it to NSZ or builds and compresses an XCI/XCZ. Nothing is
//! published beside the original before the result passes CNMT validation.

use crate::{scanner, xci_merge};
use anyhow::{Context, Result};
use nsrp_core::{
    keys::KeySet,
    ops::{
        cnmt::{self, CnmtInfo, ContentMetaType},
        compress::{self, CompressOptions},
        merge::{self, MergeFormat, MergeOptions},
        xcz::{self, XczOptions},
    },
};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const MAX_COMPRESSION_WORKERS: usize = 4;
const DEFAULT_ZSTD_LEVEL: i32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputKind {
    Nsp,
    Nsz,
    Xci,
    Xcz,
}

impl OutputKind {
    fn from_path(path: &Path) -> Result<Self> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "nsp" => Ok(Self::Nsp),
            "nsz" => Ok(Self::Nsz),
            "xci" => Ok(Self::Xci),
            "xcz" => Ok(Self::Xcz),
            extension => anyhow::bail!("병합을 지원하지 않는 원본 형식: {extension}"),
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Nsp => "nsp",
            Self::Nsz => "nsz",
            Self::Xci => "xci",
            Self::Xcz => "xcz",
        }
    }
}

fn output_for_game(original: &Path, base_id: &str, version: i64) -> Result<(PathBuf, OutputKind)> {
    anyhow::ensure!(
        base_id.len() == 16 && base_id.bytes().all(|b| b.is_ascii_hexdigit()),
        "게임 타이틀 ID 형식이 잘못되었습니다"
    );
    anyhow::ensure!(
        version > 0 && version <= u32::MAX as i64,
        "업데이트 버전이 잘못되었습니다"
    );
    let raw = u64::from_str_radix(base_id, 16)?;
    anyhow::ensure!(raw & 0x1fff == 0, "기본 게임 타이틀 ID가 아닙니다");
    let kind = OutputKind::from_path(original)?;
    let parent = original
        .parent()
        .context("원본 게임 폴더를 확인할 수 없습니다")?;
    Ok((
        parent.join(format!(
            "[NSL Merged] [{raw:016X}][v{version}].{}",
            kind.extension()
        )),
        kind,
    ))
}

fn validate_selected_cnmts(cnmts: &[CnmtInfo], base_id: &str, required_version: i64) -> Result<()> {
    let base = u64::from_str_radix(base_id, 16)? & !0x1fff;
    let base = format!("{base:016X}");
    let patch = format!("{:016X}", u64::from_str_radix(&base, 16)? | 0x800);
    anyhow::ensure!(
        cnmts.iter().any(|cnmt| {
            cnmt.title_id.eq_ignore_ascii_case(&base) && cnmt.meta_type == ContentMetaType::Base
        }),
        "병합 결과에 원본 게임 CNMT가 없습니다"
    );
    anyhow::ensure!(
        cnmts.iter().any(|cnmt| {
            cnmt.title_id.eq_ignore_ascii_case(&patch)
                && cnmt.meta_type == ContentMetaType::Update
                && i64::from(cnmt.version) >= required_version
        }),
        "병합 결과에 요청한 최신 업데이트 CNMT가 없습니다"
    );
    // Strictly reject a stale update for the same title. The selected
    // package must not contain multiple versions of one patch ID.
    anyhow::ensure!(
        cnmts
            .iter()
            .filter(|cnmt| {
                cnmt.title_id.eq_ignore_ascii_case(&patch)
                    && cnmt.meta_type == ContentMetaType::Update
            })
            .count()
            == 1,
        "병합 결과에 중복된 업데이트 CNMT가 있습니다"
    );
    Ok(())
}

fn compression_options() -> CompressOptions {
    CompressOptions {
        level: DEFAULT_ZSTD_LEVEL,
        threads: std::thread::available_parallelism()
            .map_or(2, |n| n.get().clamp(1, MAX_COMPRESSION_WORKERS)),
        block_mode: true,
        long_mode: false,
    }
}

fn check_space(parent: &Path, original: &Path, patch: &Path) -> Result<()> {
    let original_size = fs::metadata(original)?.len();
    let update_size = fs::metadata(patch)?.len();
    let input_size = original_size.saturating_add(update_size);
    // NSRP selects CNMT entries in a private workspace and decompresses
    // compressed inputs; the final sibling output has its own staging dir.
    // Leave generous headroom for intermediate files, especially NCZ data.
    let needed = input_size
        .saturating_mul(3)
        .saturating_add(512 * 1024 * 1024);
    let free = fs2::available_space(parent)?;
    anyhow::ensure!(
        free >= needed,
        "병합 출력 드라이브의 여유 공간이 부족합니다: 최소 {} GiB 필요, {} GiB 사용 가능",
        (needed + (1 << 30) - 1) >> 30,
        free >> 30
    );
    let work_root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("NSL");
    fs::create_dir_all(&work_root)?;
    let temp_needed = input_size
        .saturating_mul(2)
        .saturating_add(256 * 1024 * 1024);
    let free_temp = fs2::available_space(&work_root)?;
    anyhow::ensure!(
        free_temp >= temp_needed,
        "임시 병합 작업 드라이브의 여유 공간이 부족합니다: 최소 {} GiB 필요",
        (temp_needed + (1 << 30) - 1) >> 30
    );
    Ok(())
}

fn publish_without_overwrite(staged: &Path, destination: &Path) -> Result<()> {
    anyhow::ensure!(
        !destination.exists(),
        "이미 병합 결과물이 있습니다: {}",
        destination.display()
    );
    let parent = destination.parent().context("병합 결과 저장 폴더 없음")?;
    // Windows FlushFileBuffers requires a handle opened with write access.
    fs::OpenOptions::new()
        .write(true)
        .open(staged)?
        .sync_all()?;
    if fs::hard_link(staged, destination).is_ok() {
        return Ok(());
    }
    // FAT/exFAT and some network shares disallow hardlinks. A same-directory
    // named temporary file is slower but retains atomic no-clobber semantics.
    let mut temp = tempfile::Builder::new()
        .prefix(".nsl-publish-")
        .tempfile_in(parent)?;
    let mut source = File::open(staged)?;
    std::io::copy(&mut source, temp.as_file_mut())?;
    temp.as_file_mut().flush()?;
    temp.as_file_mut().sync_all()?;
    temp.persist_noclobber(destination).map_err(|error| {
        anyhow::anyhow!("기존 병합 파일을 덮어쓰지 않았습니다: {}", error.error)
    })?;
    Ok(())
}

/// User-visible merge: preserve the original's format, including native
/// NSZ/XCZ recompression of the merged package (not a renamed NSP/XCI).
/// Unsupported NCAs and missing keys fail without publishing any output.
pub(crate) fn merge_verified_package(
    original: &Path,
    patch: &Path,
    base_id: &str,
    wanted_version: i64,
) -> Result<PathBuf> {
    let (destination, format) = output_for_game(original, base_id, wanted_version)?;
    let ext = patch.extension().and_then(|e| e.to_str()).unwrap_or("");
    anyhow::ensure!(
        ext.eq_ignore_ascii_case("nsp") || ext.eq_ignore_ascii_case("nsz"),
        "업데이트 입력은 NSP 또는 NSZ여야 합니다"
    );
    anyhow::ensure!(
        original.is_file() && patch.is_file(),
        "병합 입력 파일이 없습니다"
    );
    anyhow::ensure!(
        original.canonicalize()? != patch.canonicalize()?,
        "원본과 업데이트 파일이 같습니다"
    );
    anyhow::ensure!(
        !destination.exists(),
        "이미 같은 업데이트 버전의 병합 파일이 있습니다"
    );
    let parent = destination.parent().context("원본 게임 폴더 없음")?;
    check_space(parent, original, patch)?;

    let prod_keys = scanner::prod_keys().context("NSZ/XCZ 병합에 필요한 prod.keys가 없습니다")?;
    let mut keys = KeySet::load_with_local_title_keys(&prod_keys)
        .context("프로그램 키 파일을 읽을 수 없습니다")?;
    let work = tempfile::Builder::new()
        .prefix(".nsl-merge-")
        .tempdir_in(parent)?;
    let disabled_tool = work.path().join("_nsl_external_tool_disabled.exe");
    let cancelled = AtomicBool::new(false);
    let options = MergeOptions {
        output_format: MergeFormat::Nsp,
        compress_after: false,
        use_game_title: false,
        decompression_threads: 2,
        compression: compression_options(),
    };
    let sources = [original.to_path_buf(), patch.to_path_buf()];
    let mut logs = Vec::new();
    let merged_outputs = merge::merge_packages(
        &sources,
        work.path(),
        true,
        work.path(),
        &disabled_tool,
        &prod_keys,
        &mut keys,
        &options,
        &cancelled,
        &mut |_| {},
        &mut |line| {
            if logs.len() < 100 {
                logs.push(line);
            }
        },
    )
    .with_context(|| format!("NSRP CNMT 병합 실패: {}", logs.join(" | ")))?;
    anyhow::ensure!(
        merged_outputs.len() == 1,
        "병합 출력이 게임별 단일 결과물이 아닙니다"
    );
    let merged_nsp = &merged_outputs[0];
    let produced = match format {
        OutputKind::Nsp => merged_nsp.clone(),
        OutputKind::Nsz => {
            let target = work.path().join("compressed.nsz");
            let stats = compress::compress_nsp(
                merged_nsp,
                &target,
                &mut keys,
                &compression_options(),
                &cancelled,
                &mut |_| {},
                &mut |_| {},
            )?;
            anyhow::ensure!(
                stats.ncz_count > 0,
                "압축된 NCA가 없어 NSZ 출력을 게시할 수 없습니다"
            );
            target
        }
        OutputKind::Xci | OutputKind::Xcz => {
            let template = if format == OutputKind::Xcz {
                let template = work.path().join("original.xci");
                xcz::decompress_xcz(original, &template, &cancelled, &mut |_| {}, &mut |_| {})?;
                template
            } else {
                original.to_path_buf()
            };
            let xci_output = work.path().join("merged.xci");
            xci_merge::build_merged_xci_from_template(
                &template,
                merged_nsp,
                &xci_output,
                &cancelled,
            )?;
            if format == OutputKind::Xci {
                xci_output
            } else {
                let target = work.path().join("compressed.xcz");
                let opts = compression_options();
                let stats = xcz::compress_xci(
                    &xci_output,
                    &target,
                    &mut keys,
                    &XczOptions {
                        level: opts.level,
                        threads: opts.threads,
                        block_mode: opts.block_mode,
                        long_mode: opts.long_mode,
                    },
                    &cancelled,
                    &mut |_| {},
                    &mut |_| {},
                )?;
                anyhow::ensure!(
                    stats.ncz_count > 0,
                    "압축된 NCA가 없어 XCZ 출력을 게시할 수 없습니다"
                );
                target
            }
        }
    };
    anyhow::ensure!(
        produced.is_file() && fs::metadata(&produced)?.len() > 0,
        "병합 출력 파일이 비어 있습니다"
    );
    // The output must contain a verified base *and* one CNMT update at least
    // as new as the requested version. Unsupported crypto/rights layouts
    // fail closed, never fall back to filename inference.
    let (_, verified, _) = cnmt::discover_game_metadata(
        &produced,
        &work.path().join("verify"),
        &disabled_tool,
        &prod_keys,
        &cancelled,
    )
    .context("병합 출력 CNMT 검증 실패")?;
    validate_selected_cnmts(&verified, base_id, wanted_version)?;

    publish_without_overwrite(&produced, &destination)?;
    Ok(destination)
}

/// Recover an already published, independently verified package after a
/// crash between final file creation and journal update. Never overwrite it.
pub(crate) fn merge_or_recover_existing(
    original: &Path,
    patch: &Path,
    base_id: &str,
    version: i64,
) -> Result<PathBuf> {
    let (destination, format) = output_for_game(original, base_id, version)?;
    if destination.is_file() {
        let keys = scanner::prod_keys().context("병합 결과 검증을 위한 prod.keys 없음")?;
        let cancelled = AtomicBool::new(false);
        let work = tempfile::tempdir()?;
        let disabled_tool = work.path().join("_nsl_external_tool_disabled.exe");
        let (_, cnmts, _) = cnmt::discover_game_metadata(
            &destination,
            &work.path().join("verify"),
            &disabled_tool,
            &keys,
            &cancelled,
        )?;
        validate_selected_cnmts(&cnmts, base_id, version)
            .context("동일 이름의 기존 파일이 요청한 병합 결과와 일치하지 않습니다")?;
        match format {
            OutputKind::Nsz => {
                let index = nsrp_core::formats::pfs0::Pfs0Index::parse(&destination)?;
                anyhow::ensure!(
                    index
                        .entries
                        .iter()
                        .any(|entry| { entry.name.to_ascii_lowercase().ends_with(".ncz") }),
                    "기존 NSZ에는 압축된 NCA가 없습니다"
                );
            }
            OutputKind::Xcz => {
                let files = nsrp_core::formats::hfs0::collect_leaf_entries(&destination, false)?;
                anyhow::ensure!(
                    files
                        .iter()
                        .any(|(name, _, _)| { name.to_ascii_lowercase().ends_with(".ncz") }),
                    "기존 XCZ에는 압축된 NCA가 없습니다"
                );
            }
            OutputKind::Nsp | OutputKind::Xci => {}
        }
        return Ok(destination);
    }
    merge_verified_package(original, patch, base_id, version)
}

#[cfg(test)]
#[path = "update_merge/real_qa.rs"]
mod real_qa;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_preserves_the_original_package_kind_and_rejects_path_injection() {
        for format in ["nsp", "nsz", "xci", "xcz"] {
            let path = PathBuf::from(format!("C:/Library/Game.{format}"));
            let (target, kind) = output_for_game(&path, "0100D2C0115B6000", 196608).unwrap();
            assert_eq!(
                target.extension().and_then(|value| value.to_str()),
                Some(format)
            );
            assert_eq!(target.parent(), path.parent());
            assert_eq!(kind.extension(), format);
        }
        assert!(output_for_game(Path::new("C:/game.nsp"), "..\\..", 1).is_err());
        assert!(output_for_game(Path::new("C:/game.nsp"), "0100D2C0115B6800", 1).is_err());
        assert!(output_for_game(Path::new("C:/game.nsp"), "0100D2C0115B6000", 0).is_err());
    }

    #[test]
    fn publishing_never_overwrites_existing_output() {
        let folder = tempfile::tempdir().unwrap();
        let from = folder.path().join("staged.nsz");
        let dest = folder.path().join("merged.nsz");
        fs::write(&from, b"new result").unwrap();
        fs::write(&dest, b"previous user result").unwrap();
        assert!(publish_without_overwrite(&from, &dest).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"previous user result");
        fs::remove_file(&dest).unwrap();
        publish_without_overwrite(&from, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new result");
        assert_eq!(fs::read(&from).unwrap(), b"new result");
    }

    #[test]
    fn cnmt_acceptance_requires_base_and_new_update_and_rejects_duplicate_patches() {
        let entry = |title: &str, kind: ContentMetaType, version: u32| CnmtInfo {
            cnmt_nca_name: format!("meta-{title}.cnmt.nca"),
            title_id: title.into(),
            version,
            meta_type: kind,
            required_system_version: 0,
            contents: vec![],
            raw: vec![],
        };
        let base = entry("0100D2C0115B6000", ContentMetaType::Base, 0);
        let patch = entry("0100D2C0115B6800", ContentMetaType::Update, 196608);
        assert!(
            validate_selected_cnmts(&[base.clone(), patch.clone()], &base.title_id, 196608).is_ok()
        );
        assert!(
            validate_selected_cnmts(std::slice::from_ref(&patch), &base.title_id, 196608).is_err()
        );
        assert!(
            validate_selected_cnmts(&[base.clone(), patch.clone()], &base.title_id, 262144)
                .is_err()
        );
        assert!(
            validate_selected_cnmts(&[base, patch.clone(), patch], "0100D2C0115B6000", 196608)
                .is_err()
        );
    }
}
