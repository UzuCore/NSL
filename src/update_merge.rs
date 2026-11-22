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
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_COMPRESSION_WORKERS: usize = 4;
const DEFAULT_ZSTD_LEVEL: i32 = 6;
const MERGE_WORK_MARKER: &str = ".nsl-merge-owner-v1";

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

/// Verify both input containers from decoded CNMT before any large merge I/O.
/// Never infer identity or version from user-controlled filenames or paths.
fn validate_input_cnmts(
    original: &[CnmtInfo],
    incoming: &[CnmtInfo],
    base_id: &str,
    dlc_id: Option<&str>,
    required_version: i64,
) -> Result<()> {
    let base = cnmt::base_title_id(base_id)?;
    anyhow::ensure!(
        original.iter().any(|item| {
            item.meta_type == ContentMetaType::Base && item.title_id.eq_ignore_ascii_case(&base)
        }),
        "원본 파일에 요청한 게임의 Base CNMT가 없습니다"
    );
    let (wanted_id, wanted_type) = if let Some(dlc_id) = dlc_id {
        anyhow::ensure!(
            cnmt::base_title_id(dlc_id)? == base,
            "DLC가 다른 게임에 속합니다"
        );
        (dlc_id.to_ascii_uppercase(), ContentMetaType::Dlc)
    } else {
        let id = format!("{:016X}", u64::from_str_radix(&base, 16)? | 0x800);
        (id, ContentMetaType::Update)
    };
    let selected: Vec<_> = incoming
        .iter()
        .filter(|item| {
            item.title_id.eq_ignore_ascii_case(&wanted_id) && item.meta_type == wanted_type
        })
        .collect();
    anyhow::ensure!(
        selected.len() == 1 && i64::from(selected[0].version) >= required_version,
        "선택한 업데이트/DLC의 CNMT ID, 종류 또는 버전이 일치하지 않습니다"
    );
    anyhow::ensure!(
        incoming.iter().all(|item| {
            item.title_id.eq_ignore_ascii_case(&wanted_id) && item.meta_type == wanted_type
        }),
        "패치 파일에 다른 게임 또는 다른 종류의 CNMT가 포함되어 있습니다"
    );
    let prior = original
        .iter()
        .filter(|item| {
            item.title_id.eq_ignore_ascii_case(&wanted_id) && item.meta_type == wanted_type
        })
        .map(|item| item.version)
        .max();
    anyhow::ensure!(
        prior.is_none_or(|version| selected[0].version > version),
        "기존 콘텐츠보다 새로운 버전이 아닙니다"
    );
    Ok(())
}

fn preflight_input_cnmts(
    original: &Path,
    patch: &Path,
    base_id: &str,
    dlc_id: Option<&str>,
    required_version: i64,
    control: &MergeControl,
) -> Result<()> {
    control.ensure_running()?;
    let prod_keys = scanner::prod_keys().context("CNMT 검사에 필요한 키 파일이 없습니다")?;
    let workspace = tempfile::Builder::new()
        .prefix("nsl-cnmt-preflight-")
        .tempdir()?;
    let disabled_tool = workspace.path().join("_nsl_external_tool_disabled.exe");
    let (_, original_cnmts, _) = cnmt::discover_game_metadata(
        original,
        &workspace.path().join("original"),
        &disabled_tool,
        &prod_keys,
        &control.cancelled,
    )
    .context("원본 CNMT를 읽을 수 없어 병합을 중단합니다")?;
    control.ensure_running()?;
    let (_, patch_cnmts, _) = cnmt::discover_game_metadata(
        patch,
        &workspace.path().join("patch"),
        &disabled_tool,
        &prod_keys,
        &control.cancelled,
    )
    .context("업데이트 CNMT를 읽을 수 없어 병합을 중단합니다")?;
    control.ensure_running()?;
    validate_input_cnmts(
        &original_cnmts,
        &patch_cnmts,
        base_id,
        dlc_id,
        required_version,
    )
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

/// Normal TempDir cleanup cannot run after process termination. Tag each new
/// private work directory with a destination-specific ownership marker and
/// hold an OS lock for the entire merge. A later retry may remove only
/// unlocked, identically tagged workspaces in that same original-game folder.
fn work_marker_bytes(destination: &Path) -> Result<Vec<u8>> {
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .context("병합 출력 파일 이름 없음")?;
    anyhow::ensure!(name.is_ascii() && name.len() <= 200);
    Ok(format!("NSL-MERGE-WORK-V1\n{name}\n").into_bytes())
}

fn hold_merge_workspace_lock(work: &Path, destination: &Path) -> Result<Option<File>> {
    let path = work.join(MERGE_WORK_MARKER);
    let mut marker = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    if fs2::FileExt::try_lock_exclusive(&marker).is_err() {
        // Some network/FAT filesystems may not support locks. Preserve their
        // ability to merge, but do not mark such work as safely reclaimable.
        drop(marker);
        fs::remove_file(path)?;
        return Ok(None);
    }
    marker.write_all(&work_marker_bytes(destination)?)?;
    marker.sync_all()?;
    Ok(Some(marker))
}

fn reap_dead_merge_workspaces(parent: &Path, destination: &Path) -> Result<usize> {
    let expected = work_marker_bytes(destination)?;
    let mut removed = 0;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(".nsl-merge-") || name.len() > 100 {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            // A Windows junction is also a reparse point; never descend.
            if meta.file_attributes() & 0x400 != 0 {
                continue;
            }
        }
        let marker_path = path.join(MERGE_WORK_MARKER);
        let Ok(marker_meta) = fs::symlink_metadata(&marker_path) else {
            continue;
        };
        if !marker_meta.file_type().is_file()
            || marker_meta.file_type().is_symlink()
            || marker_meta.len() != expected.len() as u64
        {
            continue;
        }
        let Ok(mut marker) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&marker_path)
        else {
            continue; // An active worker may hold its marker.
        };
        let mut contents = Vec::with_capacity(expected.len());
        if marker.read_to_end(&mut contents).is_err() || contents != expected {
            continue;
        }
        if fs2::FileExt::try_lock_exclusive(&marker).is_err() {
            continue; // An active worker owns this exact workspace.
        }
        // A unique orphaned temporary directory is never resumed in place.
        // Release the marker handle before deleting the directory on Windows.
        drop(marker);
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => anyhow::bail!(
                "이전 병합 작업의 격리된 임시 폴더를 안전하게 정리하지 못했습니다: {}: {error}",
                path.display()
            ),
        }
    }
    Ok(removed)
}

fn fingerprint_publish_file(path: &Path, cancel: &AtomicBool) -> Result<(u64, [u8; 32])> {
    let mut input = File::open(path)?;
    let expected = input.metadata()?.len();
    let mut total = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        anyhow::ensure!(
            !cancel.load(Ordering::Relaxed),
            "publish fingerprint cancelled"
        );
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .context("publish fingerprint overflow")?;
        anyhow::ensure!(total <= expected, "publish source grew while hashing");
        digest.update(&buffer[..read]);
    }
    anyhow::ensure!(total == expected, "publish source shortened while hashing");
    Ok((total, digest.finalize().into()))
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
    destination.parent().context("병합 결과 저장 폴더 없음")?;
    // Windows FlushFileBuffers requires a handle opened with write access.
    fs::OpenOptions::new()
        .write(true)
        .open(staged)?
        .sync_all()?;
    anyhow::ensure!(
        !cancel.load(Ordering::Relaxed),
        "병합 작업이 중단되었습니다"
    );
    // Test-only: emulate a filesystem that does not support hardlinks.
    // The opt-in path check below confines this to a dedicated QA child.
    #[cfg(test)]
    let allow_hardlink = !stage29_force_owned_fallback(staged, destination)?;
    #[cfg(not(test))]
    let allow_hardlink = true;
    if allow_hardlink && fs::hard_link(staged, destination).is_ok() {
        return Ok(());
    }
    // FAT/exFAT and some network shares disallow hardlinks. Keep the copy
    // inside the *already owner-tagged* merge workspace, not in the user's
    // game folder. A hard crash while copying then leaves no unowned
    // `.nsl-publish-*` beside real games; the ordinary Stage27 retry reaper
    // can reclaim the entire dead, locked-marker-free work directory.
    // The work folder is on the destination's own volume (tempdir_in(parent)),
    // so persist_noclobber still performs an atomic no-overwrite publication.
    let fallback_parent = staged.parent().context("병합 임시 출력의 상위 폴더 없음")?;
    let (expected_bytes, expected_hash) = fingerprint_publish_file(staged, cancel)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".nsl-publish-")
        .tempfile_in(fallback_parent)?;
    let mut source = File::open(staged)?;
    let mut buffer = vec![0u8; 2 * 1024 * 1024];
    let mut copied_bytes = 0u64;
    let mut copied_digest = Sha256::new();
    #[cfg(test)]
    let mut written = 0u64;
    loop {
        anyhow::ensure!(
            !cancel.load(Ordering::Relaxed),
            "병합 작업이 중단되었습니다"
        );
        let count = std::io::Read::read(&mut source, &mut buffer)?;
        if count == 0 {
            break;
        }
        copied_bytes = copied_bytes
            .checked_add(count as u64)
            .context("publish copy overflow")?;
        anyhow::ensure!(
            copied_bytes <= expected_bytes,
            "publish source grew during copy"
        );
        copied_digest.update(&buffer[..count]);
        temp.as_file_mut().write_all(&buffer[..count])?;
        #[cfg(test)]
        {
            written += count as u64;
            stage29_hold_owned_fallback_after_write(
                staged,
                destination,
                written,
                temp.as_file_mut(),
            )?;
        }
    }
    temp.as_file_mut().flush()?;
    temp.as_file_mut().sync_all()?;
    anyhow::ensure!(
        copied_bytes == expected_bytes && copied_digest.finalize().as_slice() == expected_hash,
        "publish source changed while copying; refusing output"
    );
    let (staged_bytes, staged_hash) = fingerprint_publish_file(temp.path(), cancel)?;
    anyhow::ensure!(
        staged_bytes == expected_bytes && staged_hash == expected_hash,
        "publish staged copy SHA-256 mismatch; refusing output"
    );
    anyhow::ensure!(
        !cancel.load(Ordering::Relaxed),
        "병합 작업이 중단되었습니다"
    );
    temp.persist_noclobber(destination).map_err(|error| {
        anyhow::anyhow!("기존 병합 파일을 덮어쓰지 않았습니다: {}", error.error)
    })?;
    Ok(())
}

/// Test-only hardlink-denied / mid-copy failpoint. A child may arm it only
/// for its exact, directly owned Stage29 scratch and marker. Production code
/// never observes these environment variables or pauses mid-publication.
#[cfg(test)]
fn stage29_qa_owned_fallback_root(staged: &Path, destination: &Path) -> Result<Option<PathBuf>> {
    let Some(root) = std::env::var_os("NSL_STAGE29_FALLBACK_QA_ROOT") else {
        return Ok(None);
    };
    let root = fs::canonicalize(root)?;
    let work = fs::canonicalize(
        staged
            .parent()
            .context("Stage29 테스트 임시 파일의 부모 폴더 없음")?,
    )?;
    anyhow::ensure!(
        root.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("stage29-fallback-"))
            && destination.parent() == Some(root.as_path())
            && work.parent() == Some(root.as_path())
            && work
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".nsl-merge-"))
            // The owner intentionally holds an exclusive whole-file lock;
            // opening and reading its marker from a *second handle* can
            // trigger Windows ERROR_LOCK_VIOLATION (33). The child just
            // created this marker and owns the lock, so check the exact
            // expected metadata length instead of bypassing that lock.
            && fs::metadata(work.join(MERGE_WORK_MARKER))?.len()
                == work_marker_bytes(destination)?.len() as u64,
        "Stage29 강제 예비 게시 테스트는 소유한 격리 폴더에서만 실행할 수 있습니다"
    );
    Ok(Some(root))
}

#[cfg(test)]
fn stage29_force_owned_fallback(staged: &Path, destination: &Path) -> Result<bool> {
    Ok(stage29_qa_owned_fallback_root(staged, destination)?.is_some())
}

#[cfg(test)]
fn stage29_hold_owned_fallback_after_write(
    staged: &Path,
    destination: &Path,
    written: u64,
    temp: &mut File,
) -> Result<()> {
    if written < 8 * 1024 * 1024
        || std::env::var("NSL_STAGE29_ABORT_AFTER_8M").ok().as_deref() != Some("1")
    {
        return Ok(());
    }
    let root = stage29_qa_owned_fallback_root(staged, destination)?
        .context("Stage29 복사 중단 시험은 소유한 작업 폴더에서만 가능합니다")?;
    // Windows FILE_ATTRIBUTE_TEMPORARY may defer data/size visibility until
    // a flush; force the test's actual 8MiB write to durable storage before
    // signalling the parent. Production does not pay this per-chunk cost.
    temp.flush()?;
    temp.sync_all()?;
    fs::write(root.join("stage29-fallback-8m.signal"), b"written-in-work")?;
    loop {
        std::thread::park();
    }
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
    // Fail closed on mismatched, unreadable, stale or mixed CNMT inputs
    // before allocating merge output, cleaning workspaces or copying NCAs.
    control.stage("입력 파일 CNMT 호환성 검증");
    preflight_input_cnmts(original, patch, base_id, dlc_id, wanted_version, control)?;
    let parent = destination.parent().context("원본 게임 폴더 없음")?;
    // Only a dead, locked-marker-free workspace for this exact destination
    // may be reclaimed. Do this before the free-space preflight: a crashed
    // merge may otherwise strand several GiB and block its own safe retry.
    reap_dead_merge_workspaces(parent, &destination)?;
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
    let _merge_work_lock = hold_merge_workspace_lock(work.path(), &destination)?;
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
    // A crash after no-clobber publication but before the owning batch
    // journal records Merged must be recoverable without publishing twice.
    // This opt-in hook exists only in the test binary; production never
    // waits here. It lets the QA parent terminate precisely in that window.
    #[cfg(test)]
    stage28_hold_owned_child_after_publish(work.path(), &destination)?;
    Ok(destination)
}

#[cfg(test)]
fn stage28_hold_owned_child_after_publish(work: &Path, destination: &Path) -> Result<()> {
    let Some(qa_root) = std::env::var_os("NSL_STAGE28_AFTER_PUBLISH_CHILD_ROOT") else {
        return Ok(());
    };
    let qa_root = fs::canonicalize(qa_root)?;
    anyhow::ensure!(
        qa_root
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("stage28-published-crash-"))
            && destination.parent() == Some(qa_root.as_path())
            && work.parent() == Some(qa_root.as_path())
            && work.join(MERGE_WORK_MARKER).is_file()
            && destination.is_file(),
        "Stage28 hook may only pause a private, published NSZ QA result"
    );
    fs::write(
        qa_root.join("stage28-published-and-locked.signal"),
        b"published-qa-only",
    )?;
    // Kill only the spawned child from the owning parent test. This simulates
    // an abrupt exit *after* the destination link is durable but *before*
    // TempDir/owner locks drop or the parent's batch journal is updated.
    loop {
        std::thread::park();
    }
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
        // A crash after successful atomic publication but before the durable
        // batch journal update leaves a completed result plus an abandoned
        // private workspace. Only after the result passes full CNMT and
        // compressed-package verification may its dead workspace be reaped.
        reap_dead_merge_workspaces(
            destination.parent().context("병합 출력 상위 폴더 없음")?,
            &destination,
        )?;
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
#[path = "update_merge/stage27_real_crash_qa.rs"]
mod stage27_real_crash_qa;

#[cfg(test)]
#[path = "update_merge/stage29_fallback_qa.rs"]
mod stage29_fallback_qa;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage27_orphan_merge_reaper_only_removes_same_destination_and_dead_ownership() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let target = temp
            .path()
            .join("[NSL DLC] [0100111122220000][0100111122221001][v0].nsz");
        let another = temp
            .path()
            .join("[NSL DLC] [0100111122220000][0100111122221002][v0].nsz");
        let original = temp.path().join("original.nsz");
        let unrelated = temp.path().join("[NSL DLC] valuable.nsz");
        fs::write(&original, b"original must remain unchanged")?;
        fs::write(&unrelated, b"unrelated must remain unchanged")?;

        let dead = tempfile::Builder::new()
            .prefix(".nsl-merge-")
            .tempdir_in(temp.path())?;
        let dead_path = dead.path().to_path_buf();
        let dead_marker = hold_merge_workspace_lock(dead.path(), &target)?;
        anyhow::ensure!(
            dead_marker.is_some(),
            "local filesystem unexpectedly has no file locks"
        );
        fs::write(dead.path().join("orphaned.partial"), vec![1u8; 1024])?;
        // Keep this directory after its owning worker's last handle closes.
        drop(dead_marker);
        let _dead_path = dead.keep();

        let live = tempfile::Builder::new()
            .prefix(".nsl-merge-")
            .tempdir_in(temp.path())?;
        let live_marker = hold_merge_workspace_lock(live.path(), &target)?;
        anyhow::ensure!(live_marker.is_some());
        fs::write(live.path().join("live.partial"), b"unfinished data")?;

        let other = tempfile::Builder::new()
            .prefix(".nsl-merge-")
            .tempdir_in(temp.path())?;
        let other_path = other.path().to_path_buf();
        let other_marker = hold_merge_workspace_lock(other.path(), &another)?;
        anyhow::ensure!(other_marker.is_some());
        drop(other_marker);
        let _other_path = other.keep();

        let user_dir = temp.path().join(".nsl-merge-user-files");
        fs::create_dir(&user_dir)?;
        fs::write(user_dir.join("important.txt"), b"not an NSL workspace")?;
        assert_eq!(reap_dead_merge_workspaces(temp.path(), &target)?, 1);
        assert!(!dead_path.exists());
        assert!(live.path().join("live.partial").is_file());
        assert!(other_path.is_dir());
        assert!(user_dir.join("important.txt").is_file());
        assert_eq!(fs::read(&original)?, b"original must remain unchanged");
        assert_eq!(fs::read(&unrelated)?, b"unrelated must remain unchanged");
        assert_eq!(reap_dead_merge_workspaces(temp.path(), &target)?, 0);
        drop(live_marker);
        drop(live);
        assert_eq!(reap_dead_merge_workspaces(temp.path(), &another)?, 1);
        assert!(!other_path.exists());
        Ok(())
    }

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
    fn stage41_input_cnmt_preflight_fails_closed_on_wrong_or_stale_content() {
        let item = |title: &str, kind: ContentMetaType, version: u32| CnmtInfo {
            cnmt_nca_name: format!("{title}.cnmt.nca"),
            title_id: title.to_string(),
            version,
            meta_type: kind,
            required_system_version: 0,
            contents: vec![],
            raw: vec![],
        };
        let base_id = "0100D2C0115B6000";
        let patch_id = "0100D2C0115B6800";
        let base = item(base_id, ContentMetaType::Base, 0);
        let patch = item(patch_id, ContentMetaType::Update, 196608);
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            std::slice::from_ref(&patch),
            base_id,
            None,
            196608
        )
        .is_ok());
        assert!(
            validate_input_cnmts(&[], std::slice::from_ref(&patch), base_id, None, 196608).is_err()
        );
        assert!(
            validate_input_cnmts(std::slice::from_ref(&base), &[], base_id, None, 196608).is_err()
        );
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            &[patch.clone(), patch.clone()],
            base_id,
            None,
            196608
        )
        .is_err());
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            &[
                patch.clone(),
                item("0100D2C0115B7001", ContentMetaType::Dlc, 0)
            ],
            base_id,
            None,
            196608
        )
        .is_err());
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            &[item("0100D2C0115B8800", ContentMetaType::Update, 196608)],
            base_id,
            None,
            196608
        )
        .is_err());
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            &[item(patch_id, ContentMetaType::Base, 196608)],
            base_id,
            None,
            196608
        )
        .is_err());
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            std::slice::from_ref(&patch),
            base_id,
            None,
            262144
        )
        .is_err());
        assert!(validate_input_cnmts(
            &[base.clone(), patch.clone()],
            std::slice::from_ref(&patch),
            base_id,
            None,
            196608
        )
        .is_err());
        assert!(validate_input_cnmts(
            &[
                base.clone(),
                item(patch_id, ContentMetaType::Update, 262144)
            ],
            std::slice::from_ref(&patch),
            base_id,
            None,
            196608
        )
        .is_err());
        let dlc_id = "0100D2C0115B7001";
        let dlc = item(dlc_id, ContentMetaType::Dlc, 0);
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            std::slice::from_ref(&dlc),
            base_id,
            Some(dlc_id),
            0
        )
        .is_ok());
        assert!(validate_input_cnmts(
            std::slice::from_ref(&base),
            std::slice::from_ref(&dlc),
            base_id,
            Some("0100D2C0115BA001"),
            0
        )
        .is_err());
        assert!(validate_input_cnmts(
            &[base, dlc.clone()],
            std::slice::from_ref(&dlc),
            base_id,
            Some(dlc_id),
            0
        )
        .is_err());
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
