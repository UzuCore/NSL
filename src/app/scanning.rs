//! Reload and scan the library using isolated background workers.

use crate::*;

/// Keep the fast metadata-only cache path, but never treat a file rewritten
/// within the same second as unchanged. SQLite preserves the f64 timestamp
/// bits, so comparing the precise mtime we originally stored is reliable.
/// A modified timestamp then falls through to the bounded fast-hash check.
fn unchanged_scan_metadata(
    stored_size: i64,
    stored_mtime: Option<f64>,
    current_size: i64,
    current_mtime: Option<f64>,
) -> bool {
    stored_size == current_size
        && stored_mtime
            .zip(current_mtime)
            .is_some_and(|(old, current)| old.is_finite() && old == current)
}

/// Legacy scanner sources remain display-only until the current scanner
/// independently reads a Base CNMT from the file itself.
fn needs_legacy_cnmt_revalidation(record: &database::FileRecord) -> bool {
    record
        .contents
        .iter()
        .any(|item| item.content_type == "base")
        && !record.contents.iter().any(|item| {
            item.content_type == "base"
                && matches!(
                    item.source.as_str(),
                    "header_cnmt" | "header_native" | "header_native_card"
                )
                && item.title_id.as_deref().is_some_and(|id| {
                    id.len() == 16
                        && u64::from_str_radix(id, 16)
                            .ok()
                            .is_some_and(|value| value & 0x1fff == 0)
                })
        })
}

/// Preserve old displayed metadata on unsupported crypto/layout; only decoded
/// CNMT of the same Base ID may promote previously unverified records.
fn validated_legacy_replacement(
    old: &database::FileRecord,
    scanned: scanner::ScanResult,
) -> Option<database::FileRecord> {
    let original_bases = old
        .contents
        .iter()
        .filter(|item| item.content_type == "base")
        .filter_map(|item| item.title_id.as_deref())
        .collect::<Vec<_>>();
    if original_bases.is_empty()
        || !scanned.contents.iter().any(|item| {
            item.content_type == "base"
                && matches!(
                    item.source.as_str(),
                    "header_cnmt" | "header_native" | "header_native_card"
                )
                && item.title_id.as_deref().is_some_and(|id| {
                    original_bases
                        .iter()
                        .any(|old_id| old_id.eq_ignore_ascii_case(id))
                })
        })
    {
        return None;
    }
    let mut upgraded = old.clone();
    upgraded.contents = scanned.contents;
    if !scanned.nacp_ko.is_empty() {
        upgraded.nacp_ko = scanned.nacp_ko;
    }
    if !scanned.nacp_en.is_empty() {
        upgraded.nacp_en = scanned.nacp_en;
    }
    if !scanned.nacp_ja.is_empty() {
        upgraded.nacp_ja = scanned.nacp_ja;
    }
    Some(upgraded)
}

impl NslApp {
    pub(crate) fn start_reload(&mut self) {
        if self.reload_result.is_some() {
            // Keep a follow-up request if this query predates a DB write.
            self.reload_dirty = true;
            return;
        }
        self.reload_dirty = false;
        let mode = self
            .config
            .settings
            .get("name_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("db")
            .to_owned();
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.reload_result = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(db.files(&mode).map_err(|e| e.to_string()));
        });
    }
    pub(crate) fn start_scan(&mut self, incremental: bool) {
        if self.scan_result.is_some() {
            return;
        }
        if self.titledb_result.is_some()
            || self.bulk_rename_result.is_some()
            || self.orphan_cleanup_result.is_some()
            || self.media_result.is_some()
        {
            self.status = "타이틀 DB·파일명 변경·캐시 정리가 끝난 뒤 스캔할 수 있습니다".into();
            return;
        }
        if self.folders.is_empty() {
            self.settings_open = true;
            self.status = "먼저 설정에서 스캔 폴더를 추가하세요".into();
            let _ = self
                .log_sender
                .send("⚠ 스캔을 시작하지 못했습니다 · 스캔 폴더 없음".into());
            return;
        }
        if !scanner::has_prod_keys() {
            self.status =
                "prod.keys를 프로그램 폴더 또는 %USERPROFILE%\\.switch 폴더에 넣어주세요".into();
            let _ = self
                .log_sender
                .send("❌ 스캔을 시작하지 못했습니다 · prod.keys 없음".into());
            return;
        }
        let folders = self.folders.clone();
        self.pending_icon_prefetch = false;
        self.pending_screenshot_prefetch = false;
        let db = self.db.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(ScanProgress {
            completed: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
        });
        self.scan_cancel = Some(cancel.clone());
        self.scan_progress = Some(progress.clone());
        let default_threads = std::thread::available_parallelism()
            .map(|n| n.get().min(16))
            .unwrap_or(8);
        let scan_threads = self
            .config
            .settings
            .get("max_scan_threads")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n <= 20)
            .unwrap_or(default_threads);
        let (tx, rx) = mpsc::channel();
        self.scan_result = Some(rx);
        self.status = "파일 검색 및 분석 중…".into();
        let log = self.log_sender.clone();
        // A one-time full re-analysis after the scan schema changes; otherwise
        // cached records are reused normally.
        let stored_schema = self
            .config
            .settings
            .get("scan_schema")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let force_reanalyze = stored_schema != SCAN_SCHEMA_VERSION;
        std::thread::spawn(move || {
            let _ = log.send(format!(
                "🚀 {} 시작 · 폴더 {}개",
                if incremental {
                    "빠른 스캔"
                } else {
                    "전체 스캔"
                },
                folders.len()
            ));
            let result = (|| -> Result<ScanOutcome> {
                // One cache read serves metadata comparison AND content reuse.
                // Building two large HashMaps duplicated path strings and DB IO.
                let cached_records = db
                    .cached_file_records()?
                    .into_iter()
                    .map(|(path, mut record)| {
                        let path = normalize_scan_path_text(&path);
                        record.path = path.clone();
                        (path, record)
                    })
                    .collect::<HashMap<_, _>>();
                let mut discovered = Vec::new();
                let mut existing = Vec::new();
                let mut skipped = 0usize;
                let mut candidates = Vec::new();
                let mut scanned_roots: Vec<String> = Vec::new();
                let mut scan_errors = 0usize;
                'folders: for folder in &folders {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let root = PathBuf::from(folder);
                    if !root.is_dir() {
                        let _ = log.send(format!(
                            "⚠ 폴더에 접근할 수 없습니다 · {folder} (기록은 유지됩니다)"
                        ));
                        continue;
                    }
                    // An unreadable child must not be treated as a deleted game.
                    let mut complete = true;
                    let ignored = [
                        "saves",
                        "save",
                        "mods",
                        "mod",
                        "atmosphere",
                        "sxos",
                        "hekate",
                        "bootloader",
                        ".git",
                        "node_modules",
                        "__pycache__",
                        "temp",
                        "tmp",
                    ];
                    for item in walkdir::WalkDir::new(&root)
                        .follow_links(false)
                        .into_iter()
                        .filter_entry(|e| {
                            e.depth() == 0
                                || !e.file_type().is_dir()
                                || !ignored.contains(
                                    &e.file_name()
                                        .to_string_lossy()
                                        .to_ascii_lowercase()
                                        .as_str(),
                                )
                        })
                    {
                        let entry = match item {
                            Ok(entry) => entry,
                            Err(error) => {
                                complete = false;
                                scan_errors += 1;
                                let _ =
                                    log.send(format!("스캔 접근 오류 (기존 기록 보존): {error}"));
                                continue;
                            }
                        };
                        if cancel.load(Ordering::Relaxed) {
                            break 'folders;
                        }
                        if !entry.file_type().is_file() {
                            continue;
                        }
                        let ext = entry
                            .path()
                            .extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if !["nsp", "nsz", "xci", "xcz"].contains(&ext.as_str()) {
                            continue;
                        }
                        let meta = match entry.metadata() {
                            Ok(meta) => meta,
                            Err(error) => {
                                complete = false;
                                scan_errors += 1;
                                let _ = log.send(format!(
                                    "스캔 메타데이터 오류 (기존 기록 보존): {error}"
                                ));
                                continue;
                            }
                        };
                        let path = normalize_scan_path(entry.path());
                        let mtime = meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64());
                        existing.push(path.clone());
                        if incremental && !force_reanalyze {
                            if let Some(old) = cached_records.get(&path) {
                                if unchanged_scan_metadata(
                                    old.size,
                                    old.mtime,
                                    meta.len() as i64,
                                    mtime,
                                ) && !needs_legacy_cnmt_revalidation(old)
                                {
                                    skipped += 1;
                                    continue;
                                }
                            }
                        }
                        let filename = entry.file_name().to_string_lossy().into_owned();
                        candidates.push((
                            PathBuf::from(&path),
                            path,
                            filename,
                            meta.len() as i64,
                            mtime,
                            ext,
                            normalize_scan_path(&root),
                        ));
                    }
                    if complete {
                        scanned_roots.push(normalize_scan_path(&root));
                    } else {
                        let _ = log.send(format!(
                            "일부 폴더를 읽지 못해 이전 기록 삭제를 생략: {}",
                            root.display()
                        ));
                    }
                }
                progress
                    .total
                    .store(candidates.len() + skipped, Ordering::Relaxed);
                progress.completed.store(skipped, Ordering::Relaxed);
                let _ = log.send(format!(
                    "📋 대상 {}개 · 캐시 재사용 {}개",
                    candidates.len() + skipped,
                    skipped
                ));
                if !candidates.is_empty() {
                    let analysis_errors = AtomicUsize::new(0);
                    let threads = if scan_threads == 0 {
                        default_threads.max(1)
                    } else {
                        scan_threads.max(1)
                    };
                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .build()?;
                    let results = pool.install(|| {
                        candidates
                            .par_iter()
                            .map(|(source, path, filename, size, mtime, ext, folder)| {
                                let cached_record = cached_record_for_path(&cached_records, path);
                                let same_metadata = cached_records.get(path).is_some_and(|old| {
                                    unchanged_scan_metadata(old.size, old.mtime, *size, *mtime)
                                });
                                // Re-decode legacy content even if size/mtime is unchanged.
                                // Reuse the prior hash; the metadata reader does not need
                                // a full-file copy or a new hash for unchanged inputs.
                                let legacy_candidate = incremental
                                    && !force_reanalyze
                                    && same_metadata
                                    && cached_record.is_some_and(needs_legacy_cnmt_revalidation);
                                if legacy_candidate {
                                    let old = cached_record.unwrap();
                                    let result = if cancel.load(Ordering::Relaxed) {
                                        (None, false)
                                    } else {
                                        scanner::scan_file_hashed(
                                            source,
                                            filename,
                                            old.fast_hash.clone(),
                                        )
                                        .ok()
                                        .and_then(|scanned| {
                                            validated_legacy_replacement(old, scanned)
                                        })
                                        .map_or((None, true), |record| (Some(record), false))
                                    };
                                    progress.completed.fetch_add(1, Ordering::Relaxed);
                                    if result.0.is_some() {
                                        let _ = log.send(format!("CNMT 재검증 성공 · {filename}"));
                                    } else if !cancel.load(Ordering::Relaxed) {
                                        let _ = log.send(format!(
                                            "CNMT 재검증 보류 (기존 기록 유지) · {filename}"
                                        ));
                                    }
                                    return result;
                                }
                                // A record written before the NACP version column
                                // existed has an empty version that a plain cache
                                // hit would keep forever, so re-read those once.
                                let cache_complete = cached_record.is_some() && !force_reanalyze;
                                let result = if cancel.load(Ordering::Relaxed) {
                                    (None, false)
                                } else if incremental
                                    && cached_record.is_some()
                                    && same_metadata
                                    && cache_complete
                                {
                                    (None, true)
                                } else if let Ok(hash) = scanner::fast_hash(source) {
                                    if let Some(old) = cached_record {
                                        if incremental && old.fast_hash == hash && cache_complete {
                                            let mut record = old.clone();
                                            record.path = path.clone();
                                            record.filename = filename.clone();
                                            record.size = *size;
                                            record.mtime = *mtime;
                                            record.file_type = ext.clone();
                                            record.scan_folder = folder.clone();
                                            record.fast_hash = hash;
                                            (Some(record), true)
                                        } else if let Ok(scanned) =
                                            scanner::scan_file_hashed(source, filename, hash)
                                        {
                                            (
                                                Some(database::FileRecord {
                                                    path: path.clone(),
                                                    filename: filename.clone(),
                                                    size: *size,
                                                    mtime: *mtime,
                                                    file_type: ext.clone(),
                                                    scan_folder: folder.clone(),
                                                    fast_hash: scanned.fast_hash,
                                                    nacp_ko: scanned.nacp_ko,
                                                    nacp_en: scanned.nacp_en,
                                                    nacp_ja: scanned.nacp_ja,
                                                    contents: scanned.contents,
                                                }),
                                                false,
                                            )
                                        } else {
                                            (None, false)
                                        }
                                    } else if let Ok(scanned) =
                                        scanner::scan_file_hashed(source, filename, hash)
                                    {
                                        (
                                            Some(database::FileRecord {
                                                path: path.clone(),
                                                filename: filename.clone(),
                                                size: *size,
                                                mtime: *mtime,
                                                file_type: ext.clone(),
                                                scan_folder: folder.clone(),
                                                fast_hash: scanned.fast_hash,
                                                nacp_ko: scanned.nacp_ko,
                                                nacp_en: scanned.nacp_en,
                                                nacp_ja: scanned.nacp_ja,
                                                contents: scanned.contents,
                                            }),
                                            false,
                                        )
                                    } else {
                                        (None, false)
                                    }
                                } else {
                                    (None, false)
                                };
                                match &result {
                                    (Some(record), was_cached) => {
                                        let content_ids = record
                                            .contents
                                            .iter()
                                            .filter_map(|content| content.title_id.as_deref())
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        let _ = log.send(format!(
                                            "{} {}{}",
                                            if *was_cached { "⚡" } else { "✅" },
                                            filename,
                                            if content_ids.is_empty() {
                                                String::new()
                                            } else {
                                                format!(" · {content_ids}")
                                            }
                                        ));
                                    }
                                    (None, true) => {
                                        let _ = log.send(format!("⚡ 캐시 사용 · {filename}"));
                                    }
                                    (None, false) if !cancel.load(Ordering::Relaxed) => {
                                        analysis_errors.fetch_add(1, Ordering::Relaxed);
                                        let _ = log.send(format!("⚠ 분석 실패 · {filename}"));
                                    }
                                    _ => {}
                                }
                                progress.completed.fetch_add(1, Ordering::Relaxed);
                                result
                            })
                            .collect::<Vec<_>>()
                    });
                    for (record, was_skipped) in results {
                        skipped += usize::from(was_skipped);
                        if let Some(record) = record {
                            discovered.push(record);
                        }
                    }
                    scan_errors += analysis_errors.load(Ordering::Relaxed);
                }
                let count = discovered.len();
                if folders.is_empty() {
                    return Ok((0, 0, false, scan_errors));
                }
                db.upsert_files(&discovered)?;
                let was_cancelled = cancel.load(Ordering::Relaxed);
                let removed = if was_cancelled {
                    0
                } else {
                    db.delete_missing_files_within(&existing, &scanned_roots)?
                };
                let _ = log.send(format!(
                    "💾 DB 반영 완료 · 새 기록 {}개 · 누락 제거 {}개{}",
                    count,
                    removed,
                    if was_cancelled {
                        " · 취소된 스캔"
                    } else {
                        ""
                    }
                ));
                Ok((count + skipped, removed, was_cancelled, scan_errors))
            })()
            .map_err(|e| e.to_string());
            if let Err(error) = &result {
                let _ = log.send(format!("❌ 스캔 오류 · {error}"));
            }
            let _ = tx.send(result);
        });
    }
}

#[cfg(test)]
mod metadata_cache_tests {
    use super::{
        needs_legacy_cnmt_revalidation, unchanged_scan_metadata, validated_legacy_replacement,
    };
    use crate::{database, scanner};

    #[test]
    fn stage42_legacy_cnmt_revalidation_requires_matching_decoded_base() {
        let make_content = |id: &str, source: &str, kind: &str| scanner::Content {
            title_id: Some(id.to_string()),
            content_type: kind.to_string(),
            source: source.to_string(),
            version: 0,
        };
        let base_id = "0100D2C0115B6000";
        let other_id = "0100AABBCCDDE000";
        let old = database::FileRecord {
            path: "C:/Games/renamed_without_title_id.nsp".into(),
            filename: "renamed_without_title_id.nsp".into(),
            size: 1234,
            mtime: Some(123.5),
            file_type: "nsp".into(),
            scan_folder: "C:/Games".into(),
            fast_hash: "original-hash".into(),
            nacp_ko: "old-name".into(),
            nacp_en: String::new(),
            nacp_ja: String::new(),
            contents: vec![make_content(base_id, "header_hactool", "base")],
        };
        assert!(needs_legacy_cnmt_revalidation(&old));
        let scan = |content: scanner::Content| scanner::ScanResult {
            fast_hash: "ignored-scan-hash".into(),
            contents: vec![content],
            nacp_ko: String::new(),
            nacp_en: "verified".into(),
            nacp_ja: String::new(),
        };
        assert!(validated_legacy_replacement(
            &old,
            scan(make_content(base_id, "filename", "base"))
        )
        .is_none());
        assert!(validated_legacy_replacement(
            &old,
            scan(make_content(other_id, "header_native", "base"))
        )
        .is_none());
        assert!(validated_legacy_replacement(
            &old,
            scan(make_content(base_id, "header_ticket", "base"))
        )
        .is_none());
        assert!(validated_legacy_replacement(
            &old,
            scan(make_content(base_id, "header_native", "update"))
        )
        .is_none());
        let upgraded = validated_legacy_replacement(
            &old,
            scan(make_content(base_id, "header_native", "base")),
        )
        .unwrap();
        assert_eq!(upgraded.path, old.path);
        assert_eq!(upgraded.fast_hash, old.fast_hash);
        assert_eq!(upgraded.mtime, old.mtime);
        assert_eq!(upgraded.size, old.size);
        assert_eq!(upgraded.nacp_ko, "old-name");
        assert_eq!(upgraded.nacp_en, "verified");
        assert!(!needs_legacy_cnmt_revalidation(&upgraded));
        let mut no_base = old;
        no_base.contents.clear();
        assert!(!needs_legacy_cnmt_revalidation(&no_base));
    }

    #[test]
    fn subsecond_timestamp_changes_require_fast_hash_verification() {
        let original = 1_800_000_000.125;
        assert!(unchanged_scan_metadata(
            4096,
            Some(original),
            4096,
            Some(original)
        ));
        assert!(!unchanged_scan_metadata(
            4096,
            Some(original),
            4096,
            Some(original + 0.125)
        ));
        assert!(!unchanged_scan_metadata(
            4096,
            Some(original),
            8192,
            Some(original)
        ));
        assert!(!unchanged_scan_metadata(4096, None, 4096, None));
        assert!(!unchanged_scan_metadata(
            4096,
            Some(f64::NAN),
            4096,
            Some(f64::NAN)
        ));
    }
}
