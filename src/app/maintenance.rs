//! Manage title databases, media collection, conversion, and cleanup tasks.

use crate::*;

impl NslApp {
    pub(crate) fn start_titledb_download(&mut self) {
        if self.titledb_result.is_some() {
            return;
        }
        if self.scan_result.is_some()
            || self.bulk_rename_result.is_some()
            || self.media_result.is_some()
        {
            self.status = "스캔 또는 파일명 변경이 끝난 뒤 타이틀 DB를 갱신하세요".into();
            return;
        }
        let dir = self.base_dir.join("titledb");
        let db = self.db.clone();
        // The repository has a single canonical source. Migrate legacy
        // "custom"/"tinfoil" choices so update checks cannot hit old hosts.
        let source = titledb::TITLE_DB_SOURCE.to_owned();
        self.config.settings.insert(
            "titledb_source".into(),
            serde_json::Value::String(source.clone()),
        );
        let (tx, rx) = mpsc::channel();
        self.titledb_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("타이틀 DB 다운로드 준비", None, false);
        self.titledb_progress = Some(progress.clone());
        self.status = "타이틀 DB 다운로드 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = titledb::download_and_apply(&dir, &source, &db, Some(&progress))
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(titledb::UpdateOutcome::AlreadyLatest) => {
                    "ℹ️ 타이틀 DB는 이미 최신입니다".into()
                }
                Ok(titledb::UpdateOutcome::Updated(count)) => {
                    format!("✅ 타이틀 DB 다운로드 및 반영 · {count}개 항목")
                }
                Err(error) => format!("❌ 타이틀 DB 다운로드 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    pub(crate) fn start_local_titledb_import(&mut self) {
        if self.titledb_result.is_some() {
            return;
        }
        if self.scan_result.is_some()
            || self.bulk_rename_result.is_some()
            || self.media_result.is_some()
        {
            self.status = "스캔 또는 파일명 변경이 끝난 뒤 타이틀 DB를 매칭하세요".into();
            return;
        }
        let dir = self.base_dir.join("titledb");
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.titledb_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("로컬 타이틀 DB 준비", None, false);
        self.titledb_progress = Some(progress.clone());
        self.status = "로컬 타이틀 DB 읽는 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = titledb::import_local_if_needed(&dir, &db, Some(&progress))
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(titledb::UpdateOutcome::Updated(count)) => {
                    format!("✅ 로컬 타이틀 DB 매칭 · {count}개 항목")
                }
                Ok(titledb::UpdateOutcome::AlreadyLatest) => {
                    "ℹ️ 타이틀 DB는 이미 최신입니다".into()
                }
                Err(error) => format!("❌ 로컬 타이틀 DB 매칭 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    pub(crate) fn start_media_collection(
        &mut self,
        mode: MediaSelection,
        selected: Option<&library::GameFile>,
    ) {
        if self.media_result.is_some() {
            return;
        }
        if self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.orphan_cleanup_result.is_some()
            || self.bulk_rename_result.is_some()
        {
            self.status = "스캔·DB 갱신·캐시 정리가 끝난 뒤 미디어를 수집하세요".into();
            return;
        }
        let plan = media_collection_plan(&self.rows, selected, mode);
        if plan.total() == 0 {
            self.status = "수집할 아이콘 또는 스크린샷이 없습니다".into();
            return;
        }
        // Manual collection supersedes pending automatic prefetch, avoiding a
        // redundant second pass over exactly the same game library.
        if selected.is_none() {
            self.pending_icon_prefetch = false;
            self.pending_screenshot_prefetch = false;
        }
        let icon_dir = self.icon_dir.clone();
        let screenshot_dir = self.screenshot_dir.clone();
        let total = media::estimated_total(&plan.icons, &plan.screenshots);
        let log = self.log_sender.clone();
        let (tx, rx) = mpsc::channel();
        self.media_result = Some(rx);
        let progress = Arc::new(media::MediaProgress::default());
        progress.begin(total);
        self.media_progress = Some(progress.clone());
        self.status = format!("미디어 수집 중… 0 / {total}");
        std::thread::spawn(move || {
            let result = media::collect_media(
                &icon_dir,
                &screenshot_dir,
                &plan.icons,
                &plan.screenshots,
                &progress,
            );
            let prefix = if result.cancelled {
                "⚠ 미디어 수집 중지"
            } else if result.failed > 0 {
                "⚠ 미디어 수집 일부 실패"
            } else {
                "✅ 미디어 수집 완료"
            };
            let _ = log.send(format!(
                "{prefix} · 아이콘 {}개 · 스크린샷 {}장 저장 · 캐시 재사용 {}개 · 실패 {}개",
                result.icons, result.screenshots, result.skipped, result.failed
            ));
            for message in progress.error_samples() {
                let _ = log.send(format!("⚠ {message}"));
            }
            let _ = tx.send(Ok(result));
        });
    }

    pub(crate) fn start_pending_media(&mut self) {
        // Scan completion reloads the DB asynchronously. Wait for fresh rows
        // instead of collecting stale/empty metadata during the same frame.
        if self.media_result.is_some()
            || self.reload_result.is_some()
            || self.reload_dirty
            || self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.orphan_cleanup_result.is_some()
            || self.bulk_rename_result.is_some()
        {
            return;
        }
        let mode = match (self.pending_icon_prefetch, self.pending_screenshot_prefetch) {
            (true, true) => Some(MediaSelection::Both),
            (true, false) => Some(MediaSelection::Icons),
            (false, true) => Some(MediaSelection::Screenshots),
            (false, false) => None,
        };
        if let Some(mode) = mode {
            self.pending_icon_prefetch = false;
            self.pending_screenshot_prefetch = false;
            self.start_media_collection(mode, None);
        }
    }
    pub(crate) fn start_decompress(&mut self, src: PathBuf) {
        if self.decompress_result.is_some() || self.workspace_cleanup_result.is_some() {
            self.status = "변환 작업이나 임시폴더 정리가 진행 중입니다".into();
            return;
        }
        let root = self.base_dir.join("workspaces");
        let log = self.log_sender.clone();
        let (tx, rx) = mpsc::channel();
        self.decompress_result = Some(rx);
        self.status = format!(
            "압축 해제 중: {}",
            src.file_name().unwrap_or_default().to_string_lossy()
        );
        std::thread::spawn(move || {
            let result = decompress::decompress(&src, &root).map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(path) => format!("✅ 압축 해제 완료 · {}", path.display()),
                Err(error) => format!("❌ 압축 해제 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    pub(crate) fn start_workspace_cleanup(&mut self) {
        if self.workspace_cleanup_result.is_some() || self.decompress_result.is_some() {
            self.status = "변환 작업이 끝난 뒤 임시폴더를 정리할 수 있습니다".into();
            return;
        }
        let root = self.base_dir.join("workspaces");
        let (tx, rx) = mpsc::channel();
        self.workspace_cleanup_result = Some(rx);
        self.status = "7일 이상 지난 변환 작업폴더 정리 중".into();
        std::thread::spawn(move || {
            let count =
                decompress::cleanup_workspaces(&root, std::time::Duration::from_secs(7 * 86400));
            let _ = tx.send(Ok(count));
        });
    }
}
