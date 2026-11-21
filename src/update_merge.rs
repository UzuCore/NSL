//! Safe NSRP-backed game patch merger, with native XCI assembly.
//!
//! The original title and downloaded update are *read-only*. NSRP operates in
//! private workspaces. NSP/NSZ uses CNMT-selected NSP assembly; XCI/XCZ
//! streams the selected materialized files directly into the gamecard without
//! a redundant merged NSP (and reuses prepared XCZ inputs). Nothing is
//! published beside the original before the result passes CNMT validation.

use crate::{progress::TaskProgress, scanner, xci_merge};
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
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_COMPRESSION_WORKERS: usize = 4;
const DEFAULT_ZSTD_LEVEL: i32 = 6;

mod fast_nsz;

/// Per-worker cancellation and measured per-stage progress. Unknown totals
/// remain indeterminate instead of displaying a fabricated overall percent.
#[derive(Default)]
pub(crate) struct MergeControl {
    cancelled: AtomicBool,
    pub(crate) progress: TaskProgress,
}

impl MergeControl {
    pub(crate) fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_cancel_requested(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub(crate) fn ensure_running(&self) -> Result<()> {
        anyhow::ensure!(
            !self.is_cancel_requested(),
            "사용자가 업데이트 병합을 중단했습니다"
        );
        Ok(())
    }

    pub(crate) fn stage(&self, label: &str) {
        self.progress.begin(label, None, false);
    }

    fn measured_stage(&self, label: &str) {
        self.progress.begin(label, Some(100), false);
    }

    fn fraction(&self, fraction: f32) {
        if fraction.is_finite() {
            self.progress.set((fraction.clamp(0.0, 1.0) * 100.0) as u64);
        }
    }
}

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

fn output_for_dlc(
    original: &Path,
    base_id: &str,
    dlc_id: &str,
    version: i64,
) -> Result<(PathBuf, OutputKind)> {
    let base = u64::from_str_radix(base_id, 16)?;
    let dlc = u64::from_str_radix(dlc_id, 16)?;
    anyhow::ensure!(
        base_id.len() == 16
            && dlc_id.len() == 16
            && base & 0x1fff == 0
            && dlc & !0x1fff == base
            && dlc & 0x1fff >= 0x1000,
        "기본 게임과 DLC Title ID가 일치하지 않습니다"
    );
    anyhow::ensure!(
        (0..=u32::MAX as i64).contains(&version),
        "DLC 버전이 올바르지 않습니다"
    );
    let kind = OutputKind::from_path(original)?;
    let parent = original.parent().context("원본 게임의 폴더가 없습니다")?;
    Ok((
        parent.join(format!(
            "[NSL DLC] [{base:016X}][{dlc:016X}][v{version}].{}",
            kind.extension()
        )),
        kind,
    ))
}

/// Exact output identity for a native DLC merge. Safe batch cleanup must
/// validate every candidate against this complete ID/version/extension path,
/// never a filename prefix alone.
pub(crate) fn expected_dlc_output(
    original: &Path,
    base_id: &str,
    dlc_id: &str,
    version: i64,
) -> Result<PathBuf> {
    Ok(output_for_dlc(original, base_id, dlc_id, version)?.0)
}

fn validate_dlc_cnmts(
    cnmts: &[CnmtInfo],
    base_id: &str,
    dlc_id: &str,
    required_version: i64,
) -> Result<()> {
    anyhow::ensure!(
        cnmts
            .iter()
            .any(|c| c.title_id.eq_ignore_ascii_case(base_id)
                && c.meta_type == ContentMetaType::Base),
        "병합 결과에 기본 게임 CNMT가 없습니다"
    );
    let dlcs = cnmts
        .iter()
        .filter(|c| c.title_id.eq_ignore_ascii_case(dlc_id) && c.meta_type == ContentMetaType::Dlc);
    let selected: Vec<_> = dlcs.collect();
    anyhow::ensure!(
        selected.len() == 1 && i64::from(selected[0].version) >= required_version,
        "병합 결과에서 요청한 DLC Title ID/버전이 확인되지 않습니다"
    );
    Ok(())
}

fn ensure_existing_content_preserved(prior: &[CnmtInfo], final_cnmts: &[CnmtInfo]) -> Result<()> {
    for entry in prior.iter().filter(|entry| {
        matches!(
            entry.meta_type,
            ContentMetaType::Update | ContentMetaType::Dlc
        )
    }) {
        anyhow::ensure!(
            final_cnmts.iter().any(|present| {
                present.title_id.eq_ignore_ascii_case(&entry.title_id)
                    && present.meta_type == entry.meta_type
                    && present.version >= entry.version
            }),
            "DLC 병합에서 기존 콘텐츠가 누락됨: {}",
            entry.title_id
        );
    }
    Ok(())
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

/// Fast NSZ assembly reuses the compressed base, and also reuses the patch
/// when it is already NSZ: no giant plaintext NSP is staged on LOCALAPPDATA.
fn check_fast_nsz_space(parent: &Path, original: &Path, patch: &Path) -> Result<()> {
    let base = fs::metadata(original)?.len();
    let update = fs::metadata(patch)?.len();
    // Two copies of the final output cover filesystems without hardlink
    // support. The extra patch allocation and fixed headroom cover temporary
    // compression, metadata and NCZ framing overhead.
    let patch_is_nsz = patch
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("nsz"));
    // Precompressed updates are copied as-is, with no intermediate patch NSZ.
    let patch_factor = if patch_is_nsz { 2 } else { 3 };
    let needed = base
        .saturating_mul(2)
        .saturating_add(update.saturating_mul(patch_factor))
        .saturating_add(512 * 1024 * 1024);
    let free = fs2::available_space(parent)?;
    anyhow::ensure!(
        free >= needed,
        "NSZ 고속 병합에 필요한 여유 공간이 부족합니다: 최소 {} GiB 필요, {} GiB 사용 가능",
        needed.div_ceil(1 << 30),
        free >> 30
    );
    Ok(())
}

fn is_fast_nsz_candidate(original: &Path, patch: &Path, format: OutputKind) -> bool {
    format == OutputKind::Nsz
        && original
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nsz"))
        && patch
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("nsp") || extension.eq_ignore_ascii_case("nsz")
            })
}

fn publish_without_overwrite(staged: &Path, destination: &Path, cancel: &AtomicBool) -> Result<()> {
    anyhow::ensure!(
        !cancel.load(Ordering::Relaxed),
        "병합 작업이 중단되었습니다"
    );
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
    anyhow::ensure!(
        !cancel.load(Ordering::Relaxed),
        "병합 작업이 중단되었습니다"
    );
    if fs::hard_link(staged, destination).is_ok() {
        return Ok(());
    }
    // FAT/exFAT and some network shares disallow hardlinks. A same-directory
    // named temporary file is slower but retains atomic no-clobber semantics.
    let mut temp = tempfile::Builder::new()
        .prefix(".nsl-publish-")
        .tempfile_in(parent)?;
    let mut source = File::open(staged)?;
    let mut buffer = vec![0u8; 2 * 1024 * 1024];
    loop {
        anyhow::ensure!(
            !cancel.load(Ordering::Relaxed),
            "병합 작업이 중단되었습니다"
        );
        let count = std::io::Read::read(&mut source, &mut buffer)?;
        if count == 0 {
            break;
        }
        temp.as_file_mut().write_all(&buffer[..count])?;
    }
    temp.as_file_mut().flush()?;
    temp.as_file_mut().sync_all()?;
    anyhow::ensure!(
        !cancel.load(Ordering::Relaxed),
        "병합 작업이 중단되었습니다"
    );
    temp.persist_noclobber(destination).map_err(|error| {
        anyhow::anyhow!("기존 병합 파일을 덮어쓰지 않았습니다: {}", error.error)
    })?;
    Ok(())
}

/// User-visible merge: preserve the original's format, including native
/// NSZ/XCZ recompression of the merged package (not a renamed NSP/XCI).
/// Unsupported NCAs and missing keys fail without publishing any output.
#[cfg(test)]
pub(crate) fn merge_verified_package(
    original: &Path,
    patch: &Path,
    base_id: &str,
    wanted_version: i64,
) -> Result<PathBuf> {
    merge_verified_package_controlled(
        original,
        patch,
        base_id,
        wanted_version,
        &MergeControl::default(),
    )
}

#[cfg(test)]
fn merge_verified_package_controlled(
    original: &Path,
    patch: &Path,
    base_id: &str,
    wanted_version: i64,
    control: &MergeControl,
) -> Result<PathBuf> {
    merge_verified_package_target_controlled(
        original,
        patch,
        base_id,
        None,
        wanted_version,
        control,
    )
}

fn merge_verified_package_target_controlled(
    original: &Path,
    patch: &Path,
    base_id: &str,
    dlc_id: Option<&str>,
    wanted_version: i64,
    control: &MergeControl,
) -> Result<PathBuf> {
    control.ensure_running()?;
    control.stage("콘텐츠 병합 사전 검증");
    let (destination, format) = if let Some(id) = dlc_id {
        output_for_dlc(original, base_id, id, wanted_version)?
    } else {
        output_for_game(original, base_id, wanted_version)?
    };
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
    // The native NSZ passthrough preserves the original NCZ payloads for
    // both updates and additional DLCs; avoid inflating and recompressing
    // every already-installed DLC on each cumulative merge.
    let fast_candidate = is_fast_nsz_candidate(original, patch, format);
    if fast_candidate {
        check_fast_nsz_space(parent, original, patch)?;
    } else {
        check_space(parent, original, patch)?;
    }

    let prod_keys = scanner::prod_keys().context("NSZ/XCZ 병합에 필요한 prod.keys가 없습니다")?;
    let mut keys = KeySet::load_with_local_title_keys(&prod_keys)
        .context("프로그램 키 파일을 읽을 수 없습니다")?;
    let work = tempfile::Builder::new()
        .prefix(".nsl-merge-")
        .tempdir_in(parent)?;
    let disabled_tool = work.path().join("_nsl_external_tool_disabled.exe");
    let cancelled = &control.cancelled;
    control.ensure_running()?;
    let optimized = if fast_candidate {
        control.stage("NSZ 고속 병합");
        fast_nsz::try_merge_nsz_fast(
            fast_nsz::FastNszRequest {
                original,
                patch,
                base_id,
                version: wanted_version,
                dlc_id,
                work: work.path(),
                disabled_tool: &disabled_tool,
                prod_keys: &prod_keys,
            },
            &mut keys,
            cancelled,
        )?
    } else {
        None
    };
    let produced = if let Some(optimized) = optimized {
        optimized
    } else {
        control.ensure_running()?;
        if fast_candidate {
            // An uncommon compressed-CNMT layout may need the legacy path.
            // Apply its larger disk-space preflight before any fallback I/O.
            check_space(parent, original, patch)?;
        }
        let options = MergeOptions {
            output_format: MergeFormat::Nsp,
            compress_after: false,
            use_game_title: false,
            decompression_threads: 2,
            compression: compression_options(),
        };
        let sources = [original.to_path_buf(), patch.to_path_buf()];
        let mut logs = Vec::new();
        let merged_outputs = if matches!(format, OutputKind::Xci | OutputKind::Xcz) {
            // The CNMT selector already materializes only the required
            // NCAs/tickets. Feed them straight into the native gamecard
            // writer while its workspace is alive: do not also assemble a
            // full-size NSP and do not decode the XCZ template a second time.
            let xci_output = work.path().join("merged.xci");
            control.measured_stage("XCI 콘텐츠 선택·병합");
            merge::merge_packages_selected(
                &sources,
                work.path(),
                true,
                work.path(),
                &disabled_tool,
                &prod_keys,
                &mut keys,
                &options,
                cancelled,
                &mut |fraction| control.fraction(fraction),
                &mut |line| {
                    if logs.len() < 100 {
                        logs.push(line);
                    }
                },
                &mut |group| {
                    xci_merge::build_merged_xci_from_selected_files(
                        group.prepared_base,
                        group.files,
                        &xci_output,
                        group.cancel,
                    )?;
                    Ok(xci_output.clone())
                },
            )
        } else {
            control.measured_stage("NSP 콘텐츠 선택·병합");
            merge::merge_packages(
                &sources,
                work.path(),
                true,
                work.path(),
                &disabled_tool,
                &prod_keys,
                &mut keys,
                &options,
                cancelled,
                &mut |fraction| control.fraction(fraction),
                &mut |line| {
                    if logs.len() < 100 {
                        logs.push(line);
                    }
                },
            )
        }
        .with_context(|| format!("NSRP CNMT 병합 실패: {}", logs.join(" | ")))?;
        anyhow::ensure!(
            merged_outputs.len() == 1,
            "병합 출력이 게임별 단일 결과물이 아닙니다"
        );
        let merged_nsp = &merged_outputs[0];
        let produced = match format {
            OutputKind::Nsp => merged_nsp.clone(),
            OutputKind::Nsz => {
                control.measured_stage("NSZ 압축");
                let target = work.path().join("compressed.nsz");
                let stats = compress::compress_nsp(
                    merged_nsp,
                    &target,
                    &mut keys,
                    &compression_options(),
                    cancelled,
                    &mut |fraction| control.fraction(fraction),
                    &mut |_| {},
                )?;
                anyhow::ensure!(
                    stats.ncz_count > 0,
                    "압축된 NCA가 없어 NSZ 출력을 게시할 수 없습니다"
                );
                target
            }
            OutputKind::Xci | OutputKind::Xcz => {
                // `merge_packages_selected` already assembled this XCI
                // directly from the selected raw content files.
                let xci_output = merged_outputs[0].clone();
                if format == OutputKind::Xci {
                    xci_output
                } else {
                    control.measured_stage("XCZ 압축");
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
                        cancelled,
                        &mut |fraction| control.fraction(fraction),
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
        produced
    };
    control.ensure_running()?;
    anyhow::ensure!(
        produced.is_file() && fs::metadata(&produced)?.len() > 0,
        "병합 출력 파일이 비어 있습니다"
    );
    // The output must contain a verified base *and* one CNMT update at least
    // as new as the requested version. Unsupported crypto/rights layouts
    // fail closed, never fall back to filename inference.
    control.stage("완성된 병합 파일 CNMT 검증");
    let (_, verified, _) = cnmt::discover_game_metadata(
        &produced,
        &work.path().join("verify"),
        &disabled_tool,
        &prod_keys,
        cancelled,
    )
    .context("병합 출력 CNMT 검증 실패")?;
    if let Some(dlc_id) = dlc_id {
        validate_dlc_cnmts(&verified, base_id, dlc_id, wanted_version)?;
        // A second DLC must not replace the first one or discard an installed
        // update. Verify every prior update/DLC CNMT is still represented.
        let (_, prior, _) = cnmt::discover_game_metadata(
            original,
            &work.path().join("verify_original"),
            &disabled_tool,
            &prod_keys,
            cancelled,
        )?;
        ensure_existing_content_preserved(&prior, &verified)?;
    } else {
        validate_selected_cnmts(&verified, base_id, wanted_version)?;
    }
    control.ensure_running()?;
    control.stage("병합 파일 안전 게시");
    publish_without_overwrite(&produced, &destination, cancelled)?;
    Ok(destination)
}

/// Recover an already published, independently verified package after a
/// crash between final file creation and journal update. Never overwrite it.
#[cfg(test)]
pub(crate) fn merge_or_recover_existing(
    original: &Path,
    patch: &Path,
    base_id: &str,
    version: i64,
) -> Result<PathBuf> {
    merge_or_recover_existing_controlled(
        original,
        patch,
        base_id,
        version,
        &MergeControl::default(),
    )
}

pub(crate) fn merge_or_recover_existing_controlled(
    original: &Path,
    patch: &Path,
    base_id: &str,
    version: i64,
    control: &MergeControl,
) -> Result<PathBuf> {
    merge_or_recover_target_controlled(original, patch, base_id, None, version, control)
}

pub(crate) fn merge_or_recover_dlc_controlled(
    original: &Path,
    patch: &Path,
    base_id: &str,
    dlc_id: &str,
    version: i64,
    control: &MergeControl,
) -> Result<PathBuf> {
    merge_or_recover_target_controlled(original, patch, base_id, Some(dlc_id), version, control)
}

fn merge_or_recover_target_controlled(
    original: &Path,
    patch: &Path,
    base_id: &str,
    dlc_id: Option<&str>,
    version: i64,
    control: &MergeControl,
) -> Result<PathBuf> {
    control.ensure_running()?;
    let (destination, format) = if let Some(id) = dlc_id {
        output_for_dlc(original, base_id, id, version)?
    } else {
        output_for_game(original, base_id, version)?
    };
    if destination.is_file() {
        control.stage("기존 병합 결과 CNMT 재검증");
        let keys = scanner::prod_keys().context("병합 결과 검증을 위한 prod.keys 없음")?;
        let work = tempfile::tempdir()?;
        let disabled_tool = work.path().join("_nsl_external_tool_disabled.exe");
        let (_, cnmts, _) = cnmt::discover_game_metadata(
            &destination,
            &work.path().join("verify"),
            &disabled_tool,
            &keys,
            &control.cancelled,
        )?;
        control.ensure_running()?;
        if let Some(id) = dlc_id {
            validate_dlc_cnmts(&cnmts, base_id, id, version)
                .context("동일 이름의 기존 파일이 요청한 DLC와 다릅니다")?;
            let (_, prior, _) = cnmt::discover_game_metadata(
                original,
                &work.path().join("verify_original"),
                &disabled_tool,
                &keys,
                &control.cancelled,
            )?;
            ensure_existing_content_preserved(&prior, &cnmts)?;
        } else {
            validate_selected_cnmts(&cnmts, base_id, version)
                .context("동일 이름의 기존 파일이 요청한 병합 결과와 일치하지 않습니다")?;
        }
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
        control.ensure_running()?;
        return Ok(destination);
    }
    merge_verified_package_target_controlled(original, patch, base_id, dlc_id, version, control)
}

#[cfg(test)]
#[path = "update_merge/real_qa.rs"]
mod real_qa;

#[cfg(test)]
#[path = "update_merge/stage10_real_qa.rs"]
mod stage10_real_qa;

#[cfg(test)]
#[path = "update_merge/stage10_wrapper_qa.rs"]
mod stage10_wrapper_qa;

#[cfg(test)]
#[path = "update_merge/stage13_real_dlc_qa.rs"]
mod stage13_real_dlc_qa;

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
    fn recognizes_fast_nsz_inputs_including_precompressed_updates() {
        for extension in ["nsp", "nsz", "NSZ"] {
            assert!(is_fast_nsz_candidate(
                Path::new("C:/Games/original.NSZ"),
                Path::new(&format!("C:/Games/patch.{extension}")),
                OutputKind::Nsz,
            ));
        }
        for (original, patch, output) in [
            ("original.nsp", "patch.nsz", OutputKind::Nsp),
            ("original.xci", "patch.nsz", OutputKind::Xci),
            ("original.nsz", "patch.xci", OutputKind::Nsz),
        ] {
            assert!(!is_fast_nsz_candidate(
                Path::new(original),
                Path::new(patch),
                output,
            ));
        }
    }

    #[test]
    fn publishing_never_overwrites_existing_output() {
        let folder = tempfile::tempdir().unwrap();
        let from = folder.path().join("staged.nsz");
        let dest = folder.path().join("merged.nsz");
        fs::write(&from, b"new result").unwrap();
        fs::write(&dest, b"previous user result").unwrap();
        let cancel = AtomicBool::new(false);
        assert!(publish_without_overwrite(&from, &dest, &cancel).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"previous user result");
        fs::remove_file(&dest).unwrap();
        publish_without_overwrite(&from, &dest, &cancel).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new result");
        assert_eq!(fs::read(&from).unwrap(), b"new result");
    }

    #[test]
    fn cancelled_publication_keeps_source_and_never_creates_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("staged.nsp");
        let output = dir.path().join("merged.nsp");
        fs::write(&source, b"complete staging").unwrap();
        let control = MergeControl::default();
        control.stage("입력 검증");
        assert_eq!(control.progress.snapshot().total, None);
        control.measured_stage("재압축");
        control.fraction(0.46);
        assert_eq!(control.progress.snapshot().completed, 46);
        assert_eq!(control.progress.snapshot().total, Some(100));
        control.request_cancel();
        assert!(control.ensure_running().is_err());
        assert!(publish_without_overwrite(&source, &output, &control.cancelled).is_err());
        assert!(!output.exists());
        assert_eq!(fs::read(source).unwrap(), b"complete staging");
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

    #[test]
    fn dlc_outputs_have_distinct_names_and_require_exact_title_ids() {
        let input = Path::new("C:/Games/Base.NSZ");
        let (one, kind) = output_for_dlc(input, "0100D2C0115B6000", "0100D2C0115B7001", 0).unwrap();
        let (two, _) = output_for_dlc(input, "0100D2C0115B6000", "0100D2C0115B7002", 0).unwrap();
        assert_eq!(kind, OutputKind::Nsz);
        assert_ne!(one, two);
        assert_eq!(one.parent(), input.parent());
        assert!(output_for_dlc(input, "0100D2C0115B6000", "0100D2C0115B6800", 0).is_err());
        assert!(output_for_dlc(input, "0100D2C0115B6000", "0100D2C0115B7001", -1).is_err());
        let entry = |id: &str, kind| CnmtInfo {
            cnmt_nca_name: format!("{id}.cnmt.nca"),
            title_id: id.into(),
            version: 0,
            meta_type: kind,
            required_system_version: 0,
            contents: vec![],
            raw: vec![],
        };
        let base = entry("0100D2C0115B6000", ContentMetaType::Base);
        let dlc = entry("0100D2C0115B7001", ContentMetaType::Dlc);
        assert!(validate_dlc_cnmts(
            &[base.clone(), dlc.clone()],
            &base.title_id,
            &dlc.title_id,
            0
        )
        .is_ok());
        assert!(validate_dlc_cnmts(
            &[base.clone(), dlc.clone()],
            &base.title_id,
            "0100D2C0115B7002",
            0
        )
        .is_err());
        assert!(validate_dlc_cnmts(
            &[base.clone(), dlc.clone()],
            &base.title_id,
            &dlc.title_id,
            1
        )
        .is_err());
        assert!(validate_dlc_cnmts(
            &[base, dlc.clone(), dlc.clone()],
            "0100D2C0115B6000",
            &dlc.title_id,
            0
        )
        .is_err());
        let prior = vec![
            entry("0100D2C0115B6800", ContentMetaType::Update),
            entry("0100D2C0115B7002", ContentMetaType::Dlc),
        ];
        let only_new_dlc = vec![dlc.clone()];
        assert!(ensure_existing_content_preserved(&prior, &only_new_dlc).is_err());
        let mut cumulative = prior.clone();
        cumulative.push(dlc);
        assert!(ensure_existing_content_preserved(&prior, &cumulative).is_ok());
    }
}
