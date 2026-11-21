//! Read-only catalog matching and selective update download planning.
//!
//! A catalog's title ID identifies a *game*, not the patch contents or its
//! version. Only a resolved torrent file with the expected patch title ID and
//! an explicit numeric version can be offered for a bulk queue. This is still
//! only a candidate; the downloaded package must be scanned before merging.

use crate::{catalog, library, scanner, torrent, update_merge};
use anyhow::{Context, Result};
use regex::Regex;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, OnceLock},
};

const MAX_RELEASES_PER_GAME: usize = 3;
const MAX_RELEASES_PER_DLC: usize = 8;
const MAX_CACHED_TORRENT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum BatchKind {
    #[default]
    Update,
    Dlc,
}

pub(crate) struct CachedPreview {
    files: Vec<torrent::TorrentFileEntry>,
    bytes: Arc<Vec<u8>>,
}

pub(crate) struct UpdateBatch {
    pub(crate) kind: BatchKind,
    pub(crate) rows: Vec<UpdateBatchRow>,
    pub(crate) running: bool,
    pub(crate) cursor: usize,
    pub(crate) active: Option<(usize, torrent::TorrentPreviewRequest)>,
    pub(crate) confirm_queue: bool,
    pub(crate) preview_cache: HashMap<usize, CachedPreview>,
}

pub(crate) struct UpdateBatchRow {
    pub(crate) library_id: i64,
    pub(crate) title_id: String,
    pub(crate) dlc_id: Option<String>,
    pub(crate) name: String,
    pub(crate) installed_version: i64,
    pub(crate) latest_version: i64,
    pub(crate) original_path: PathBuf,
    pub(crate) original_type: String,
    pub(crate) candidates: Vec<usize>,
    pub(crate) next_candidate: usize,
    pub(crate) found: Option<UpdateBatchMatch>,
    pub(crate) checked: bool,
    pub(crate) failure: Option<String>,
    pub(crate) job: Option<UpdateBatchJob>,
}

pub(crate) struct UpdateBatchMatch {
    pub(crate) release: catalog::CatalogItem,
    pub(crate) file: torrent::TorrentFileEntry,
    pub(crate) version: i64,
    pub(crate) torrent_bytes: Arc<Vec<u8>>,
}

pub(crate) struct UpdateBatchJob {
    pub(crate) key: String,
    /// Stable torrent output directory. Runtime torrent keys are not stable
    /// across a restored librqbit session.
    pub(crate) directory: PathBuf,
    pub(crate) output: PathBuf,
    pub(crate) expected_version: i64,
    pub(crate) expected_file_size: u64,
    pub(crate) original_size: u64,
    pub(crate) transfer_finished: bool,
    pub(crate) recovering_since: Option<std::time::Instant>,
    pub(crate) status: UpdateBatchJobStatus,
    pub(crate) verifying: Option<mpsc::Receiver<Result<Option<PathBuf>, String>>>,
    /// Runtime-only merge control. Never serialized into batch-updates.json.
    pub(crate) merge_control: Option<Arc<update_merge::MergeControl>>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum UpdateBatchJobStatus {
    Queued,
    Downloading,
    Verifying,
    Verified,
    Merged(PathBuf),
    Failed(String),
}

fn preferred_release_score(item: &catalog::CatalogItem, game: &str) -> (u8, u8, u64) {
    let title = item.title.to_lowercase();
    let modded = [
        "[mod",
        " romhack",
        "ctgp",
        "randomizer",
        " repack",
        "translation",
    ]
    .iter()
    .any(|needle| title.contains(needle));
    let search_terms: Vec<String> = game
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|part| part.chars().count() > 2)
        .collect();
    let matching_name =
        search_terms.is_empty() || search_terms.iter().any(|term| title.contains(term));
    let topic = item.topic_id.parse::<u64>().unwrap_or(0);
    (u8::from(matching_name), u8::from(!modded), topic)
}

pub(crate) fn new_update_batch(
    owned: &[library::GameFile],
    catalog_items: &[catalog::CatalogItem],
    excluded: &str,
) -> UpdateBatch {
    let mut by_id: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, item) in catalog_items.iter().enumerate() {
        if item.magnet.starts_with("magnet:?") {
            if let Some(base) = item.base_title_id() {
                by_id.entry(base).or_default().push(index);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for row in owned {
        if !row.has_base
            || row.latest_version <= row.version.max(row.update_version)
            || excluded
                .split(',')
                .any(|id| id.trim().eq_ignore_ascii_case(&row.title_id))
            || !seen.insert(row.title_id.to_ascii_uppercase())
        {
            continue;
        }
        let mut candidates = by_id
            .get(&row.title_id.to_ascii_uppercase())
            .cloned()
            .unwrap_or_default();
        candidates.sort_by(|&a, &b| {
            preferred_release_score(&catalog_items[b], &row.title_name)
                .cmp(&preferred_release_score(&catalog_items[a], &row.title_name))
        });
        candidates.truncate(MAX_RELEASES_PER_GAME);
        rows.push(UpdateBatchRow {
            library_id: row.id,
            title_id: row.title_id.to_ascii_uppercase(),
            dlc_id: None,
            name: row.title_name.clone(),
            installed_version: row.version.max(row.update_version),
            latest_version: row.latest_version,
            original_path: PathBuf::from(&row.path),
            original_type: row.file_type.to_ascii_lowercase(),
            candidates,
            next_candidate: 0,
            found: None,
            checked: true,
            failure: None,
            job: None,
        });
    }
    UpdateBatch {
        kind: BatchKind::Update,
        rows,
        running: true,
        cursor: 0,
        active: None,
        confirm_queue: false,
        preview_cache: HashMap::new(),
    }
}

pub(crate) fn new_dlc_batch(
    owned: &[library::GameFile],
    catalog_items: &[catalog::CatalogItem],
    excluded: &str,
) -> UpdateBatch {
    let mut by_base: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, item) in catalog_items.iter().enumerate() {
        if item.magnet.starts_with("magnet:?") {
            if let Some(base) = item.base_title_id() {
                by_base.entry(base).or_default().push(index);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for game in owned.iter().filter(|row| row.has_base) {
        let base = game.title_id.to_ascii_uppercase();
        let Ok(base_num) = u64::from_str_radix(&base, 16) else {
            continue;
        };
        if base.len() != 16 || base_num & 0x1fff != 0 {
            continue;
        }
        for (id, name) in &game.missing_dlcs {
            let id = id.to_ascii_uppercase();
            let valid = u64::from_str_radix(&id, 16)
                .ok()
                .is_some_and(|n| id.len() == 16 && n & !0x1fff == base_num && n & 0x1fff >= 0x1000);
            if !valid
                || excluded
                    .split([',', '\n', ';'])
                    .any(|part| part.trim().eq_ignore_ascii_case(&id))
                || !seen.insert((base.clone(), id.clone()))
            {
                continue;
            }
            let mut candidates = by_base.get(&base).cloned().unwrap_or_default();
            candidates.sort_by(|&a, &b| {
                let rank = |item: &catalog::CatalogItem| {
                    (
                        u8::from(
                            item.title_id
                                .as_deref()
                                .is_some_and(|raw| raw.eq_ignore_ascii_case(&id)),
                        ),
                        preferred_release_score(item, &game.title_name),
                    )
                };
                rank(&catalog_items[b]).cmp(&rank(&catalog_items[a]))
            });
            candidates.truncate(MAX_RELEASES_PER_DLC);
            rows.push(UpdateBatchRow {
                library_id: game.id,
                title_id: base.clone(),
                dlc_id: Some(id),
                name: format!("{} · {}", game.title_name, name),
                installed_version: -1,
                latest_version: 0,
                original_path: PathBuf::from(&game.path),
                original_type: game.file_type.to_ascii_lowercase(),
                candidates,
                next_candidate: 0,
                found: None,
                checked: true,
                failure: None,
                job: None,
            });
        }
    }
    UpdateBatch {
        kind: BatchKind::Dlc,
        rows,
        running: true,
        cursor: 0,
        active: None,
        confirm_queue: false,
        preview_cache: HashMap::new(),
    }
}

fn patch_file_version(file: &torrent::TorrentFileEntry, patch_id: &str) -> Option<i64> {
    if file.size == 0 {
        return None;
    }
    let ext = file.path.extension()?.to_str()?;
    if !["nsp", "nsz"].iter().any(|e| ext.eq_ignore_ascii_case(e)) {
        return None;
    }
    let name = file.path.file_name()?.to_str()?;
    static IDS: OnceLock<Regex> = OnceLock::new();
    static VERSIONS: OnceLock<Regex> = OnceLock::new();
    let ids = IDS.get_or_init(|| Regex::new(r"(?i)\[([0-9a-f]{16})\]").unwrap());
    let versions = VERSIONS.get_or_init(|| Regex::new(r"(?i)\[v(\d+)\]").unwrap());
    if !ids
        .captures_iter(name)
        .any(|cap| cap[1].eq_ignore_ascii_case(patch_id))
    {
        return None;
    }
    let number: i64 = versions.captures(name)?.get(1)?.as_str().parse().ok()?;
    (number > 0).then_some(number)
}

/// Requires a specific update TitleID and a version >= the game's current
/// title database version; packages without trustworthy filename metadata are
/// withheld from automatic download rather than guessed from release titles.
pub(crate) fn pick_current_patch<'a>(
    files: &'a [torrent::TorrentFileEntry],
    title_id: &str,
    installed: i64,
    latest: i64,
) -> Option<(&'a torrent::TorrentFileEntry, i64)> {
    let base = u64::from_str_radix(title_id, 16).ok()? & !0x1fff;
    let patch_id = format!("{:016X}", base | 0x800);
    files
        .iter()
        .filter_map(|file| {
            let version = patch_file_version(file, &patch_id)?;
            (version > installed && version >= latest).then_some((file, version))
        })
        .max_by_key(|(_, version)| *version)
}

fn dlc_file_version(file: &torrent::TorrentFileEntry, dlc_id: &str) -> Option<i64> {
    if file.size == 0
        || !file.path.extension()?.eq_ignore_ascii_case("nsp")
            && !file.path.extension()?.eq_ignore_ascii_case("nsz")
    {
        return None;
    }
    let filename = file.path.file_name()?.to_str()?;
    static IDS: OnceLock<Regex> = OnceLock::new();
    static VERSIONS: OnceLock<Regex> = OnceLock::new();
    let ids = IDS.get_or_init(|| Regex::new(r"(?i)\[([0-9a-f]{16})\]").unwrap());
    let versions = VERSIONS.get_or_init(|| Regex::new(r"(?i)\[v(\d+)\]").unwrap());
    if !ids
        .captures_iter(filename)
        .any(|cap| cap[1].eq_ignore_ascii_case(dlc_id))
    {
        return None;
    }
    match versions.captures(filename) {
        Some(caps) => caps[1].parse::<i64>().ok().filter(|v| *v >= 0),
        None => Some(0),
    }
}

pub(crate) fn pick_missing_dlc<'a>(
    files: &'a [torrent::TorrentFileEntry],
    dlc_id: &str,
) -> Option<(&'a torrent::TorrentFileEntry, i64)> {
    files
        .iter()
        .filter_map(|file| dlc_file_version(file, dlc_id).map(|v| (file, v)))
        .max_by_key(|(_, version)| *version)
}

impl UpdateBatch {
    fn apply_preview(
        &mut self,
        row_index: usize,
        catalog_index: usize,
        catalog_items: &[catalog::CatalogItem],
    ) {
        let Some(preview) = self.preview_cache.get(&catalog_index) else {
            return;
        };
        let row = &mut self.rows[row_index];
        let matched = match self.kind {
            BatchKind::Update => pick_current_patch(
                &preview.files,
                &row.title_id,
                row.installed_version,
                row.latest_version,
            ),
            BatchKind::Dlc => row
                .dlc_id
                .as_deref()
                .and_then(|id| pick_missing_dlc(&preview.files, id)),
        };
        if let Some((file, version)) = matched {
            row.found = Some(UpdateBatchMatch {
                release: catalog_items[catalog_index].clone(),
                file: file.clone(),
                version,
                torrent_bytes: Arc::clone(&preview.bytes),
            });
            row.failure = None;
        } else {
            row.failure = Some(match self.kind {
                BatchKind::Update => "최신 업데이트 ID·버전을 확인할 수 있는 파일이 없음".into(),
                BatchKind::Dlc => "해당 DLC Title ID가 명시된 NSP/NSZ 파일이 없음".into(),
            });
        }
    }

    pub(crate) fn eligible(&self) -> usize {
        self.rows.iter().filter(|row| row.found.is_some()).count()
    }
    pub(crate) fn chosen(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.checked && row.found.is_some() && row.job.is_none())
            .count()
    }
    pub(crate) fn has_match_for_game(&self, id: i64) -> bool {
        self.rows
            .iter()
            .any(|row| row.library_id == id && row.found.is_some())
    }
    pub(crate) fn progress(&self) -> (usize, usize) {
        (self.cursor.min(self.rows.len()), self.rows.len())
    }
    pub(crate) fn stop(&mut self) {
        self.active = None; // Drop cancels an outstanding magnet preview.
        self.running = false;
    }

    /// Poll exactly one metadata-only request per frame. A failed or
    /// uninformative release is followed by the next ranked candidate;
    /// downloads never start during this audit.
    pub(crate) fn tick(
        &mut self,
        catalog_items: &[catalog::CatalogItem],
        engine: Option<&torrent::TorrentEngine>,
    ) {
        if !self.running {
            return;
        }
        let completed = self
            .active
            .as_mut()
            .map(|(index, request)| (*index, request.receiver.try_recv()));
        if let Some((index, result)) = completed {
            match result {
                Ok(Ok(preview)) => {
                    let cached_bytes: usize =
                        self.preview_cache.values().map(|p| p.bytes.len()).sum();
                    let catalog_index =
                        self.rows[index].candidates[self.rows[index].next_candidate - 1];
                    if preview.torrent_bytes.len()
                        > MAX_CACHED_TORRENT_BYTES.saturating_sub(cached_bytes)
                    {
                        self.rows[index].failure =
                            Some("일괄 대조 메타데이터 메모리 한도(128MiB) 초과".into());
                    } else {
                        self.preview_cache.insert(
                            catalog_index,
                            CachedPreview {
                                files: preview.files,
                                bytes: Arc::new(preview.torrent_bytes),
                            },
                        );
                        self.apply_preview(index, catalog_index, catalog_items);
                    }
                    self.active = None;
                }
                Ok(Err(error)) => {
                    self.rows[index].failure = Some(format!("토렌트 조회 실패: {error}"));
                    self.active = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rows[index].failure = Some("토렌트 목록 조회 연결 끊김".into());
                    self.active = None;
                }
                Err(mpsc::TryRecvError::Empty) => return,
            }
        }
        while self.cursor < self.rows.len() {
            let row = &mut self.rows[self.cursor];
            if row.found.is_some() || row.next_candidate >= row.candidates.len() {
                if row.found.is_none() && row.failure.is_none() {
                    row.failure = Some("카탈로그 일치 항목 없음".into());
                }
                self.cursor += 1;
                continue;
            }
            let catalog_index = row.candidates[row.next_candidate];
            row.next_candidate += 1;
            if self.preview_cache.contains_key(&catalog_index) {
                self.apply_preview(self.cursor, catalog_index, catalog_items);
                continue;
            }
            let Some(engine) = engine else {
                self.stop();
                return;
            };
            match engine.preview_files(&catalog_items[catalog_index].magnet) {
                Ok(request) => {
                    self.active = Some((self.cursor, request));
                    return;
                }
                Err(error) => row.failure = Some(format!("토렌트 조회 시작 실패: {error}")),
            }
        }
        self.running = false;
    }
}

pub(crate) fn safe_update_output(
    base_dir: &Path,
    name: &str,
    base_id: &str,
    unique: &str,
) -> PathBuf {
    // All three components have a bounded validated alphabet and cannot
    // become absolute paths, traversal or NTFS alternate data streams.
    let safe = |s: &str, n: usize| {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .take(n)
            .collect::<String>()
    };
    base_dir.join("downloads").join(format!(
        "batch-{}-{}-{}",
        safe(base_id, 16),
        safe(name, 12),
        safe(unique, 16)
    ))
}

/// Do not trust a torrent filename, a displayed progress percentage, or a
/// catalog ID as proof that the payload is the requested patch. Only
/// independently scanned CNMT metadata can release it for a future merge.
pub(crate) fn verify_downloaded_patch(
    base_dir: &Path,
    output: &Path,
    base_id: &str,
    expected_version: i64,
    installed_version: i64,
) -> Result<()> {
    let root = base_dir
        .join("downloads")
        .canonicalize()
        .context("다운로드 저장소가 존재하지 않습니다")?;
    let file = output
        .canonicalize()
        .with_context(|| format!("다운로드된 업데이트 파일이 없음: {}", output.display()))?;
    anyhow::ensure!(
        file.starts_with(root),
        "다운로드 파일이 저장소 밖을 가리킵니다"
    );
    anyhow::ensure!(file.is_file(), "다운로드 결과가 일반 파일이 아닙니다");
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .context("다운로드 파일명이 올바르지 않습니다")?;
    let metadata = scanner::scan_file_hashed(&file, name, scanner::fast_hash(&file)?)?;
    let base = u64::from_str_radix(base_id, 16).context("잘못된 게임 타이틀 ID")? & !0x1fff;
    let patch_id = format!("{:016X}", base | 0x800);
    let found = metadata.contents.iter().find(|content| {
        content.title_id.as_deref() == Some(patch_id.as_str())
            && content.content_type == "update"
            && matches!(content.source.as_str(), "header_native" | "header_cnmt")
            && content.version > installed_version
            && content.version >= expected_version
    });
    anyhow::ensure!(
        found.is_some(),
        "패키지의 신뢰할 수 있는 CNMT에서 업데이트 {} v{} 이상을 확인하지 못했습니다",
        patch_id,
        expected_version
    );
    Ok(())
}

/// Require the exact missing DLC Title ID in trustworthy native/header CNMT.
/// A filename or catalog entry is never sufficient to approve the payload.
pub(crate) fn verify_downloaded_dlc(
    base_dir: &Path,
    output: &Path,
    base_id: &str,
    dlc_id: &str,
    expected_version: i64,
) -> Result<()> {
    let base = u64::from_str_radix(base_id, 16)?;
    let dlc = u64::from_str_radix(dlc_id, 16)?;
    anyhow::ensure!(
        base_id.len() == 16
            && dlc_id.len() == 16
            && base & 0x1fff == 0
            && dlc & !0x1fff == base
            && dlc & 0x1fff >= 0x1000,
        "DLC ID가 해당 기본 게임에 속하지 않습니다"
    );
    anyhow::ensure!(expected_version >= 0, "DLC 버전이 올바르지 않습니다");
    let download_root = base_dir.join("downloads").canonicalize()?;
    let file = output.canonicalize()?;
    anyhow::ensure!(
        file.starts_with(&download_root) && file.is_file(),
        "다운로드된 DLC 파일이 저장소 밖에 있습니다"
    );
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .context("DLC 파일명 오류")?;
    let metadata = scanner::scan_file_hashed(&file, name, scanner::fast_hash(&file)?)?;
    anyhow::ensure!(
        metadata.contents.iter().any(|content| {
            content
                .title_id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(dlc_id))
                && content.content_type == "dlc"
                && matches!(content.source.as_str(), "header_native" | "header_cnmt")
                && content.version >= expected_version
        }),
        "다운로드 파일의 신뢰할 수 있는 CNMT에서 DLC {} v{} 이상을 확인할 수 없습니다",
        dlc_id,
        expected_version
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: usize, name: &str, size: u64) -> torrent::TorrentFileEntry {
        torrent::TorrentFileEntry {
            index,
            path: name.into(),
            size,
        }
    }

    #[test]
    fn requires_matching_patch_title_id_and_known_newer_version() {
        let files = vec![
            entry(4, "Base [0100D2C0115B6000][v999999].nsz", 20),
            entry(5, "Wrong [0100D2C0115B6801][v999999].nsp", 20),
            entry(6, "Patch [0100D2C0115B6800][v65536].nsp", 20),
            entry(7, "Patch [0100D2C0115B6800][v196608].nsz", 20),
            entry(8, "Patch [0100D2C0115B6800].nsp", 20),
            entry(9, "Patch [0100D2C0115B6800][v999999].txt", 20),
        ];
        let found = pick_current_patch(&files, "0100D2C0115B6000", 65536, 196608).unwrap();
        assert_eq!((found.0.index, found.1), (7, 196608));
        assert!(pick_current_patch(&files, "0100D2C0115B6000", 196608, 262144).is_none());
        assert!(pick_current_patch(&files, "invalid", 0, 1).is_none());
    }

    #[test]
    fn safe_output_is_one_new_folder_below_downloads() {
        let root = Path::new("C:/NSL");
        let result = safe_update_output(root, "../trick", "0100AAAABBBB0000", "a:/bad");
        assert_eq!(result.parent().unwrap(), root.join("downloads"));
        assert!(!result.file_name().unwrap().to_string_lossy().contains('/'));
        assert!(!result.file_name().unwrap().to_string_lossy().contains(':'));
    }

    #[test]
    fn dlc_candidates_require_exact_title_id_and_allow_version_zero() {
        let files = vec![
            entry(0, "Game [0100D2C0115B6000].nsp", 200),
            entry(1, "Update [0100D2C0115B6800][v65536].nsz", 40),
            entry(2, "Other DLC [0100D2C0115B7002][v99999].nsp", 60),
            entry(3, "DLC [0100D2C0115B7001].nsp", 150),
            entry(4, "DLC [0100D2C0115B7001][v0].nsz", 180),
            entry(5, "DLC [0100D2C0115B7001][v65536].nsp", 180),
            entry(6, "DLC [0100D2C0115B7001][v999999].txt", 200),
            entry(7, "DLC [0100D2C0115B7001][v100000].nsp", 0),
        ];
        let selected = pick_missing_dlc(&files, "0100D2C0115B7001").unwrap();
        assert_eq!((selected.0.index, selected.1), (5, 65536));
        assert_eq!(
            pick_missing_dlc(&files[3..4], "0100D2C0115B7001")
                .unwrap()
                .1,
            0
        );
        assert!(pick_missing_dlc(&files, "0100D2C0115B7003").is_none());
    }

    #[test]
    #[ignore = "requires NSL_DLC_QA_FILE (a read-only DLC NSP or NSZ below 16 MiB)"]
    fn isolated_real_dlc_cnmt_validation_rejects_other_title_ids() -> Result<()> {
        let source = std::fs::canonicalize(std::env::var("NSL_DLC_QA_FILE")?)?;
        anyhow::ensure!(std::fs::metadata(&source)?.len() < 16 * 1024 * 1024);
        let original = std::fs::read(&source)?;
        let filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .context("DLC 파일명 오류")?;
        let id = Regex::new(r"(?i)\[([0-9a-f]{16})\]")?
            .captures(filename)
            .context("파일명에 DLC Title ID 없음")?[1]
            .to_ascii_uppercase();
        let numeric = u64::from_str_radix(&id, 16)?;
        let base = format!("{:016X}", numeric & !0x1fff);
        let wrong = format!("{:016X}", (numeric & !0x1fff) | 0x1fff);
        let sandbox = tempfile::tempdir()?;
        let folder = sandbox.path().join("downloads").join("qa");
        std::fs::create_dir_all(&folder)?;
        let output = folder.join("dlc.nsp");
        std::fs::copy(&source, &output)?;
        verify_downloaded_dlc(sandbox.path(), &output, &base, &id, 0)?;
        anyhow::ensure!(verify_downloaded_dlc(sandbox.path(), &output, &base, &wrong, 0).is_err());
        anyhow::ensure!(std::fs::read(&source)? == original);
        println!("STAGE12_REAL_DLC_CNMT_PASS id={id} size={}", original.len());
        Ok(())
    }
}
