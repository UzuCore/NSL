//! Filter, navigate, inspect, and operate on local library entries.

use crate::*;

impl NslApp {
    pub(crate) fn launch_path(&mut self, path: &PathBuf) -> Result<()> {
        anyhow::ensure!(path.is_file(), "실행할 파일을 찾지 못했습니다");
        let enabled = self
            .config
            .settings
            .get("launcher_enabled")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            == "1";
        let launcher = self
            .config
            .settings
            .get("launcher_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        if enabled && !launcher.is_empty() {
            let raw = self
                .config
                .settings
                .get("launcher_args")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let has_path = raw.contains("{path}");
            let expanded = raw.replace("{path}", &path.to_string_lossy());
            let args = shlex::split(&expanded)
                .ok_or_else(|| anyhow::anyhow!("실행 인자를 해석할 수 없습니다"))?;
            let mut cmd = std::process::Command::new(&launcher);
            cmd.args(args);
            if !has_path {
                cmd.arg(path);
            }
            cmd.spawn()?;
        } else {
            #[cfg(windows)]
            {
                std::process::Command::new("explorer").arg(path).spawn()?;
            }
            #[cfg(target_os = "macos")]
            {
                std::process::Command::new("open").arg(path).spawn()?;
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                std::process::Command::new("xdg-open").arg(path).spawn()?;
            }
        }
        self.status = format!(
            "실행: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        Ok(())
    }
    pub(crate) fn run_file(&mut self, row: &library::GameFile) {
        let ext = row.file_type.to_ascii_lowercase();
        let auto = self
            .config
            .settings
            .get("decompress_auto")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            == "1";
        if auto && (ext == "nsz" || ext == "xcz") {
            self.launch_after_decompress = true;
            self.start_decompress(PathBuf::from(&row.path));
            return;
        }
        if let Err(e) = self.launch_path(&PathBuf::from(&row.path)) {
            self.status = format!("실행 실패: {e}")
        }
    }
    pub(crate) fn row_context_menu(&mut self, ui: &mut egui::Ui, row: &library::GameFile) {
        self.selected = Some(row.id);
        if ui.button("🎮  실행").clicked() {
            self.run_file(row);
            ui.close_menu();
        }
        if ui.button("📂  폴더에서 열기").clicked() {
            let _ = std::process::Command::new("explorer")
                .arg("/select,")
                .arg(&row.path)
                .spawn();
            ui.close_menu();
        }
        if ui.button("📄  경로 복사").clicked() {
            ui.ctx().copy_text(row.path.clone());
            self.status = "경로를 클립보드에 복사했습니다".into();
            ui.close_menu();
        }
        ui.separator();
        if ui.button("✏️  파일명 정규화").clicked() {
            match self.rename_target(row) {
                Ok(target) => {
                    self.rename_preview =
                        Some((row.path.clone(), target.to_string_lossy().into_owned()));
                }
                Err(e) => self.status = e.to_string(),
            }
            ui.close_menu();
        }
        ui.separator();
        if ui.button("📸  스크린샷 다운로드").clicked() {
            self.start_media_collection(MediaSelection::Screenshots, Some(row));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("🖼  아이콘 스캔").clicked() {
            self.start_media_collection(MediaSelection::Icons, Some(row));
            ui.close_menu();
        }
    }
    pub(crate) fn start_translation(&mut self, row: &library::GameFile) {
        if self.translate_result.is_some() {
            return;
        }
        let id = row.id;
        let desc = row.description.clone();
        if desc.is_empty() {
            self.status = "번역할 설명이 없습니다".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.translate_result = Some(rx);
        self.translation_error = None;
        self.status = "게임 설명 번역 중…".into();
        std::thread::spawn(move || {
            let _ = tx.send((id, translate::to_korean(&desc).map_err(|e| e.to_string())));
        });
    }
    pub(crate) fn rename_target(&self, row: &library::GameFile) -> Result<PathBuf> {
        let old = PathBuf::from(&row.path);
        if !old.is_file() {
            anyhow::bail!("게임 파일을 찾을 수 없습니다")
        }
        let ext = old
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !["nsp", "nsz", "xci", "xcz"].contains(&ext.as_str()) {
            anyhow::bail!("지원하지 않는 게임 파일 형식입니다")
        }
        let contents = self.db.file_contents(&row.path)?;
        let base = contents
            .iter()
            .find(|c| c.content_type == "base")
            .ok_or_else(|| anyhow::anyhow!("베이스 게임 정보를 찾지 못했습니다"))?;
        let tid = base.title_id.clone().unwrap_or_default();
        if tid.len() != 16 || !tid.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("베이스 타이틀 ID를 찾지 못했습니다")
        }
        let updates: Vec<i64> = contents
            .iter()
            .filter(|c| c.content_type == "update")
            .map(|c| c.version)
            .collect();
        let dlcs: std::collections::HashSet<String> = contents
            .iter()
            .filter(|c| c.content_type == "dlc")
            .filter_map(|c| c.title_id.clone())
            .collect();
        let mut title = row.title_name.clone();
        for bad in ['\\', '/', '*', '?', ':', '"', '<', '>', '|'] {
            title = title.replace(bad, "");
        }
        title = title
            .trim()
            .trim_end_matches([' ', '.'])
            .chars()
            .take(100)
            .collect();
        if title.is_empty() {
            anyhow::bail!("게임 이름을 결정할 수 없습니다")
        }
        let name = if updates.is_empty() && dlcs.is_empty() {
            format!(
                "{title} [{}][v{}][Base].{ext}",
                tid.to_ascii_uppercase(),
                base.version
            )
        } else {
            let mut tags = vec!["B".to_owned()];
            if let Some(v) = updates.iter().max() {
                tags.push(format!("U{v}"));
            }
            if !dlcs.is_empty() {
                tags.push(format!("{}DLC", dlcs.len()));
            }
            format!(
                "{title} [{}][{}].{ext}",
                tid.to_ascii_uppercase(),
                tags.join("+")
            )
        };
        Ok(old.with_file_name(name))
    }
    /// Folder tab labels, rebuilt only when the row set or folders change.
    pub(crate) fn library_tabs_cached(&mut self) -> &[(String, String)] {
        if self.cached_tabs_generation == self.rows_generation && !self.cached_tabs.is_empty() {
            return &self.cached_tabs;
        }
        let base_count = self.rows.iter().filter(|row| row.has_base).count();
        let mut tabs = vec![(String::new(), format!("전체 ({base_count})"))];
        for folder in &self.folders {
            let count = self
                .rows
                .iter()
                .filter(|row| row.has_base && path_is_within(&row.path, folder))
                .count();
            let name = PathBuf::from(folder)
                .file_name()
                .map(|v| v.to_string_lossy().into_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| folder.clone());
            tabs.push((folder.clone(), format!("{name} ({count})")));
        }
        tabs.push((
            "MISSING_UPDATES".into(),
            format!("미보유 업데이트 ({})", self.cached_missing_updates),
        ));
        tabs.push((
            "MISSING_DLCS".into(),
            format!("미보유 DLC ({})", self.cached_missing_dlcs),
        ));
        tabs.push(("CATALOG".into(), "스토어".into()));
        tabs.push((
            "DOWNLOADS".into(),
            if self.torrent_progress.is_empty() {
                "다운로드".into()
            } else {
                format!("다운로드 ({})", self.torrent_progress.len())
            },
        ));
        self.cached_tabs = tabs;
        self.cached_tabs_generation = self.rows_generation;
        &self.cached_tabs
    }

    /// Missing-update / missing-DLC counts and duplicate groups, computed once
    /// per row generation instead of every frame.
    pub(crate) fn refresh_library_stats(&mut self) {
        let exceptions = (
            self.config
                .settings
                .get("exception_update")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            self.config
                .settings
                .get("exception_dlc")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
        );
        if self.cached_stats_generation == self.rows_generation
            && self.cached_stats_exceptions == exceptions
        {
            return;
        }
        self.cached_stats_exceptions = exceptions;
        self.cached_tabs_generation = u64::MAX;
        self.cached_missing_updates = self
            .rows
            .iter()
            .filter(|row| row.has_base && self.row_is_missing_update(row))
            .count();
        self.cached_missing_dlcs = self
            .rows
            .iter()
            .filter(|row| row.has_base && self.row_has_missing_dlc(row))
            .count();
        self.cached_duplicates = self.duplicate_title_groups();
        self.cached_stats_generation = self.rows_generation;
    }

    /// Screenshot files for the selected game, resolved only when the
    /// selection changes.
    pub(crate) fn selected_screenshot_paths_cached(&mut self) -> Vec<PathBuf> {
        if self.cached_screenshot_selection == self.selected {
            return self.cached_screenshot_paths.clone();
        }
        self.cached_screenshot_selection = self.selected;
        let paths: Vec<PathBuf> = self
            .selected
            .and_then(|id| self.rows.iter().find(|r| r.id == id))
            .map(|row| {
                let dir = self.screenshot_dir.join(&row.title_id);
                let mut files = std::fs::read_dir(dir)
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| {
                                ["jpg", "jpeg", "png", "webp"]
                                    .contains(&e.to_ascii_lowercase().as_str())
                            })
                            .unwrap_or(false)
                    })
                    .collect::<Vec<_>>();
                files.sort();
                files.into_iter().take(6).collect()
            })
            .unwrap_or_default();
        self.cached_screenshot_paths = paths.clone();
        paths
    }

    /// Filtered + sorted library view, rebuilt only when an input changes.
    ///
    /// The naive version cloned and sorted all 800+ `GameFile` rows on every
    /// frame, which allocated ~16k strings per frame and made the library view
    /// stutter. This keeps the computation off the hot path.
    pub(crate) fn library_rows_cached(&mut self) -> Arc<Vec<library::GameFile>> {
        let key = FilterKey {
            search: self.search_committed.clone(),
            kind: self.kind_filter.clone(),
            folder: self.folder_filter.clone(),
            sort_column: self.sort_column.clone(),
            sort_desc: self.sort_desc,
            exception_update: self
                .config
                .settings
                .get("exception_update")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            exception_dlc: self
                .config
                .settings
                .get("exception_dlc")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            rows_len: self.rows.len(),
            rows_generation: self.rows_generation,
        };
        if self.filtered_key.as_ref() == Some(&key) {
            return self.filtered_rows.clone();
        }
        let mut matching = library::visible(&self.rows, &self.search_committed)
            .filter(|row| self.row_matches_current_filter(row))
            .cloned()
            .collect::<Vec<_>>();
        matching.sort_by(|a, b| {
            let ordering = match self.sort_column.as_str() {
                "id" => a.title_id.cmp(&b.title_id),
                "type" => a.content_type.cmp(&b.content_type),
                "version" => a.version.cmp(&b.version),
                "latest_ver" => a.latest_version.cmp(&b.latest_version),
                "dlc" => a.dlc_count.cmp(&b.dlc_count),
                "size" => a.size.cmp(&b.size),
                "path" => a.path.to_lowercase().cmp(&b.path.to_lowercase()),
                "status" => {
                    let status = |row: &library::GameFile| {
                        if row.latest_version == 0 {
                            0
                        } else if row.version.max(row.update_version) < row.latest_version {
                            1
                        } else {
                            2
                        }
                    };
                    status(a).cmp(&status(b))
                }
                _ => a
                    .title_name
                    .to_lowercase()
                    .cmp(&b.title_name.to_lowercase()),
            };
            if self.sort_desc {
                ordering.reverse()
            } else {
                ordering
            }
        });
        self.filtered_rows = Arc::new(matching);
        self.filtered_key = Some(key);
        self.filtered_rows.clone()
    }

    pub(crate) fn row_matches_current_filter(&self, row: &library::GameFile) -> bool {
        let exception_update = self
            .config
            .settings
            .get("exception_update")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let exception_dlc = self
            .config
            .settings
            .get("exception_dlc")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if self.folder_filter == "MISSING_UPDATES" {
            return row.has_base
                && row.latest_version > 0
                && row.version.max(row.update_version) < row.latest_version
                && !exception_update
                    .split(',')
                    .any(|id| id.trim().eq_ignore_ascii_case(&row.title_id));
        }
        if self.folder_filter == "MISSING_DLCS" {
            return row.has_base
                && row.missing_dlcs.iter().any(|(id, _)| {
                    !exception_dlc
                        .split(',')
                        .any(|excluded| excluded.trim().eq_ignore_ascii_case(id))
                });
        }
        if !self.folder_filter.is_empty() {
            return row.has_base && path_is_within(&row.path, &self.folder_filter);
        }
        match self.kind_filter.as_str() {
            "base" => row.has_base,
            "update" => row.has_update,
            "dlc" => row.dlc_count > 0,
            "unknown" => !row.has_base && !row.has_update && row.dlc_count == 0,
            "missing_updates" => {
                row.has_base
                    && row.latest_version > 0
                    && row.version.max(row.update_version) < row.latest_version
                    && !exception_update
                        .split(',')
                        .any(|id| id.trim().eq_ignore_ascii_case(&row.title_id))
            }
            "missing_dlcs" => {
                row.has_base
                    && row.missing_dlcs.iter().any(|(id, _)| {
                        !exception_dlc
                            .split(',')
                            .any(|excluded| excluded.trim().eq_ignore_ascii_case(id))
                    })
            }
            _ => true,
        }
    }
    pub(crate) fn row_is_missing_update(&self, row: &library::GameFile) -> bool {
        let excluded = self
            .config
            .settings
            .get("exception_update")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        row.latest_version > 0
            && row.version.max(row.update_version) < row.latest_version
            && !excluded
                .split(',')
                .any(|id| id.trim().eq_ignore_ascii_case(&row.title_id))
    }
    pub(crate) fn row_has_missing_dlc(&self, row: &library::GameFile) -> bool {
        let excluded = self
            .config
            .settings
            .get("exception_dlc")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        row.missing_dlcs.iter().any(|(id, _)| {
            !excluded
                .split(',')
                .any(|value| value.trim().eq_ignore_ascii_case(id))
        })
    }
    pub(crate) fn duplicate_title_groups(&self) -> Vec<(String, String, Vec<String>)> {
        let mut grouped: HashMap<String, Vec<&library::GameFile>> = HashMap::new();
        for row in self.rows.iter().filter(|row| row.has_base) {
            if row.title_id.len() == 16 {
                grouped
                    .entry(row.title_id.to_ascii_uppercase())
                    .or_default()
                    .push(row);
            }
        }
        let mut duplicates = grouped
            .into_iter()
            .filter_map(|(title_id, rows)| {
                let mut unique = HashMap::<String, String>::new();
                for row in rows {
                    unique
                        .entry(row.path.to_ascii_lowercase())
                        .or_insert_with(|| row.path.clone());
                }
                if unique.len() < 2 {
                    return None;
                }
                let mut paths = unique.into_values().collect::<Vec<_>>();
                paths.sort_by_key(|path| path.to_ascii_lowercase());
                let name = self
                    .rows
                    .iter()
                    .find(|row| row.title_id.eq_ignore_ascii_case(&title_id))
                    .map(|row| row.title_name.clone())
                    .unwrap_or_default();
                Some((title_id, name, paths))
            })
            .collect::<Vec<_>>();
        duplicates.sort_by_key(|(_, name, id_paths)| {
            name.to_lowercase() + id_paths.first().map(String::as_str).unwrap_or("")
        });
        duplicates
    }
    pub(crate) fn preview_bulk_rename(&mut self) {
        if self.bulk_rename_result.is_some() || self.bulk_rename_preview.is_some() {
            return;
        }
        let mut changes = Vec::new();
        let mut unprocessable = 0usize;
        for row in library::visible(&self.rows, &self.search_committed) {
            if !self.row_matches_current_filter(row) {
                continue;
            }
            match self.rename_target(row) {
                Ok(path) if path.to_string_lossy() != row.path => {
                    changes.push((row.path.clone(), path.to_string_lossy().into_owned()));
                }
                Ok(_) => {}
                Err(_) => unprocessable += 1,
            }
        }
        if changes.is_empty() {
            self.status = format!("정규화할 파일이 없습니다 (분석 불가 {unprocessable}개)");
        } else {
            self.status = format!(
                "변경 예정 {}개, 분석 불가 {}개",
                changes.len(),
                unprocessable
            );
            self.bulk_rename_preview = Some(changes);
        }
    }
    pub(crate) fn start_bulk_rename(&mut self, changes: Vec<(String, String)>) {
        if self.bulk_rename_result.is_some() {
            return;
        }
        if self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.decompress_result.is_some()
            || self.media_result.is_some()
            || self.orphan_cleanup_result.is_some()
        {
            self.status = "스캔·타이틀 DB·압축 해제 작업 중에는 파일명을 변경할 수 없습니다".into();
            return;
        }
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.bulk_rename_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("파일명 일괄 정규화", Some(changes.len() as u64), false);
        self.rename_progress = Some(progress.clone());
        self.status = format!("파일명 일괄 정규화 중: {}개", changes.len());
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let mut renamed = 0usize;
            let mut failed = 0usize;
            for (index, (from, to)) in changes.into_iter().enumerate() {
                if PathBuf::from(&to).exists() || std::fs::rename(&from, &to).is_err() {
                    failed += 1;
                    progress.set(index as u64 + 1);
                    continue;
                }
                if db.rename_file_path(&from, &to).is_ok() {
                    renamed += 1;
                } else {
                    let _ = std::fs::rename(&to, &from);
                    failed += 1;
                }
                progress.set(index as u64 + 1);
            }
            let _ = log.send(format!(
                "✏️ 파일명 일괄 정규화 완료 · 변경 {renamed}개 · 실패 {failed}개"
            ));
            let _ = tx.send(Ok((renamed, failed)));
        });
    }
    pub(crate) fn start_orphan_cleanup(&mut self) {
        if self.orphan_cleanup_result.is_some() {
            return;
        }
        if self.media_result.is_some()
            || self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.reload_result.is_some()
            || self.bulk_rename_result.is_some()
        {
            self.status = "미디어 수집·스캔·DB 갱신이 끝난 뒤 캐시를 정리하세요".into();
            return;
        }
        let db = self.db.clone();
        let icon_dir = self.icon_dir.clone();
        let screenshot_dir = self.screenshot_dir.clone();
        let (tx, rx) = mpsc::channel();
        self.orphan_cleanup_result = Some(rx);
        self.status = "사용하지 않는 아이콘/스크린샷 정리 중".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<(usize, usize)> {
                let active = db
                    .content_title_ids()?
                    .into_iter()
                    .map(|id| id.to_ascii_uppercase())
                    .collect::<std::collections::HashSet<_>>();
                let mut removed_icons = 0;
                let mut removed_screenshots = 0;
                if let Ok(root) = icon_dir.canonicalize() {
                    if let Ok(entries) = std::fs::read_dir(&root) {
                        for entry in entries.filter_map(Result::ok) {
                            let path = entry.path();
                            if path.extension().and_then(|v| v.to_str()) != Some("png") {
                                continue;
                            }
                            let id = path
                                .file_stem()
                                .and_then(|v| v.to_str())
                                .unwrap_or("")
                                .to_ascii_uppercase();
                            if !active.contains(&id)
                                && path
                                    .canonicalize()
                                    .ok()
                                    .and_then(|p| p.parent().map(PathBuf::from))
                                    == Some(root.clone())
                                && std::fs::remove_file(&path).is_ok()
                            {
                                removed_icons += 1;
                            }
                        }
                    }
                }
                if let Ok(root) = screenshot_dir.canonicalize() {
                    if let Ok(entries) = std::fs::read_dir(&root) {
                        for entry in entries.filter_map(Result::ok) {
                            let path = entry.path();
                            if !path.is_dir() {
                                continue;
                            }
                            let id = path
                                .file_name()
                                .and_then(|v| v.to_str())
                                .unwrap_or("")
                                .to_ascii_uppercase();
                            let safe_child = path
                                .canonicalize()
                                .ok()
                                .map(|target| target.parent() == Some(root.as_path()))
                                .unwrap_or(false);
                            if !active.contains(&id)
                                && safe_child
                                && std::fs::remove_dir_all(&path).is_ok()
                            {
                                removed_screenshots += 1;
                            }
                        }
                    }
                }
                Ok((removed_icons, removed_screenshots))
            })()
            .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok((icons, screenshots)) => {
                    format!(
                        "🧹 미디어 캐시 정리 완료 · 아이콘 {icons}개 · 스크린샷 {screenshots}개"
                    )
                }
                Err(error) => format!("❌ 미디어 캐시 정리 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    pub(crate) fn export_csv(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("nsl-library.csv")
            .save_file()
        else {
            return;
        };
        let result = (|| -> Result<()> {
            let mut file = std::fs::File::create(path)?;
            std::io::Write::write_all(&mut file, &[0xEF, 0xBB, 0xBF])?;
            let mut w = csv::WriterBuilder::new()
                .has_headers(false)
                .from_writer(file);
            w.write_record([
                "게임 이름",
                "타이틀 (KO)",
                "타이틀 (EN)",
                "타이틀 (JA)",
                "타이틀 ID",
                "종류",
                "보유 버전",
                "업데이트 버전",
                "최신 버전",
                "상태",
                "크기",
                "형식",
                "DLC 보유",
                "DB DLC 수",
                "언어",
                "지역",
                "파일명",
                "파일 경로",
            ])?;
            for r in &self.rows {
                let ver = if r.update_version != 0 {
                    r.update_version
                } else {
                    r.version
                };
                let status = if r.latest_version == 0 {
                    "미확인"
                } else if ver >= r.latest_version {
                    "최신"
                } else {
                    "업데이트 가능"
                };
                w.write_record([
                    r.title_name.as_str(),
                    r.nacp_ko.as_str(),
                    r.nacp_en.as_str(),
                    r.nacp_ja.as_str(),
                    r.title_id.as_str(),
                    r.content_type.as_str(),
                    &common::version_to_str(Some(r.version)),
                    &if r.update_version == 0 {
                        String::new()
                    } else {
                        common::version_to_str(Some(r.update_version))
                    },
                    &if r.latest_version == 0 {
                        String::new()
                    } else {
                        common::version_to_str(Some(r.latest_version))
                    },
                    status,
                    &common::format_size(r.size.max(0) as u64),
                    &r.file_type.to_ascii_uppercase(),
                    &r.dlc_count.to_string(),
                    &r.db_dlc_count.to_string(),
                    r.languages.as_str(),
                    r.region.as_str(),
                    r.filename.as_str(),
                    r.path.as_str(),
                ])?;
            }
            w.flush()?;
            Ok(())
        })();
        self.status = match result {
            Ok(()) => "CSV 내보내기 완료".into(),
            Err(e) => e.to_string(),
        };
    }

    pub(crate) fn selected_detail_ui(
        &mut self,
        ui: &mut egui::Ui,
        row: &library::GameFile,
        screenshot_paths: &[PathBuf],
    ) {
        let panel_content_width = ui.available_width();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // A long title or absolute path must wrap inside the user's
                // chosen sash width, never expand the SidePanel's layout.
                ui.set_max_width(panel_content_width);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                ui.horizontal(|ui| {
                    if let Some(icon) = self.icon_textures.get(&row.title_id) {
                        let icon_size = if panel_content_width < 365.0 {
                            96.0
                        } else {
                            120.0
                        };
                        ui.image((icon.id(), egui::vec2(icon_size, icon_size)));
                    }
                    ui.vertical(|ui| {
                        ui.set_max_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            if ui.small_button("×").clicked() {
                                self.selected = None;
                            }
                            ui.label(egui::RichText::new(&row.title_name).size(14.0).strong());
                        });
                        ui.label(
                            egui::RichText::new(&row.title_id)
                                .size(12.0)
                                .color(egui::Color32::GRAY),
                        );
                        let mut type_label = match row.content_type.as_str() {
                            "base" => "베이스".to_owned(),
                            "update" => "업데이트".to_owned(),
                            "dlc" => "DLC".to_owned(),
                            "delta" => "Delta".to_owned(),
                            _ => "Unknown".to_owned(),
                        };
                        if row.has_update {
                            type_label.push_str("+U");
                        }
                        if row.dlc_count > 0 {
                            type_label.push_str(&format!("+{}DLC", row.dlc_count));
                        }
                        type_label.push_str(&format!("  ({})", row.file_type.to_ascii_uppercase()));
                        ui.colored_label(ui.visuals().hyperlink_color, type_label);
                        let installed = if row.update_version != 0 {
                            row.update_version
                        } else {
                            row.version
                        };
                        ui.label(format!("보유: {}", common::version_to_str(Some(installed))));
                        ui.colored_label(
                            if row.latest_version > installed {
                                egui::Color32::from_rgb(224, 108, 117)
                            } else {
                                egui::Color32::GRAY
                            },
                            format!("최신: {}", common::version_to_str(Some(row.latest_version))),
                        );
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    let width = ((ui.available_width() - 3.0) / 4.0).max(48.0);
                    for (index, label) in ["파일정보", "게임정보", "스크린샷", "DLC"]
                        .into_iter()
                        .enumerate()
                    {
                        if draw_tab_button(ui, label, self.detail_tab == index, width, index, 4)
                            .clicked()
                        {
                            self.detail_tab = index;
                            self.config.settings.insert(
                                "last_selected_tab".into(),
                                serde_json::Value::String(index.to_string()),
                            );
                        }
                    }
                });
                ui.separator();
                match self.detail_tab {
                    0 => {
                        let installed = if row.update_version != 0 {
                            row.update_version
                        } else {
                            row.version
                        };
                        let localized_names = [
                            ("게임 이름 (한국어)", row.nacp_ko.trim()),
                            ("게임 이름 (영어)", row.nacp_en.trim()),
                            ("게임 이름 (일본어)", row.nacp_ja.trim()),
                        ];
                        let names_present =
                            localized_names.iter().any(|(_, name)| !name.is_empty());
                        let mut fields = Vec::new();
                        if names_present {
                            fields.extend(
                                localized_names
                                    .iter()
                                    .filter(|(_, name)| !name.is_empty())
                                    .map(|(label, name)| ((*label).to_owned(), (*name).to_owned())),
                            );
                        } else {
                            fields.push((
                                "게임 이름".to_owned(),
                                if row.title_name.is_empty() {
                                    row.filename.clone()
                                } else {
                                    row.title_name.clone()
                                },
                            ));
                        }
                        let mut content_type = match row.content_type.as_str() {
                            "base" => "베이스".to_owned(),
                            "update" => "업데이트".to_owned(),
                            "dlc" => "DLC".to_owned(),
                            "" | "unknown" => "—".to_owned(),
                            other => other.to_owned(),
                        };
                        if row.has_update {
                            content_type.push_str("+U");
                        }
                        if row.dlc_count > 0 {
                            content_type.push_str(&format!("+{}DLC", row.dlc_count));
                        }
                        if !row.file_type.is_empty() {
                            content_type
                                .push_str(&format!("  ({})", row.file_type.to_ascii_uppercase()));
                        }
                        fields.extend([
                            ("타이틀 ID".to_owned(), row.title_id.clone()),
                            (
                                "보유 버전".to_owned(),
                                common::version_to_str(Some(installed)),
                            ),
                            (
                                "최신 버전".to_owned(),
                                common::version_to_str(Some(row.latest_version)),
                            ),
                            ("타입".to_owned(), content_type),
                            (
                                "크기".to_owned(),
                                common::format_size(row.size.max(0) as u64),
                            ),
                            ("파일 경로".to_owned(), row.path.clone()),
                            ("파일명".to_owned(), row.filename.clone()),
                        ]);
                        for (label, value) in fields {
                            ui.label(egui::RichText::new(label).color(egui::Color32::GRAY));
                            ui.label(value);
                            ui.add_space(6.0);
                        }
                    }
                    1 => {
                        for (label, value) in [
                            ("한국어", &row.nacp_ko),
                            ("영어", &row.nacp_en),
                            ("일본어", &row.nacp_ja),
                            ("지역", &row.region),
                            ("언어", &row.languages),
                        ] {
                            if !value.is_empty() {
                                ui.label(format!("{label}: {value}"));
                            }
                        }
                        if !row.description.is_empty() {
                            ui.heading("게임 설명");
                            ui.label(&row.description);
                            let translated = self
                                .translated_description
                                .as_ref()
                                .filter(|(id, _)| *id == row.id)
                                .map(|(_, text)| text.clone());
                            if translated.is_none() {
                                if self.translate_result.is_some() {
                                    ui.add_enabled(false, egui::Button::new("한국어 번역 중…"));
                                } else if ui.button("설명 한국어 번역").clicked() {
                                    self.start_translation(row);
                                }
                            }
                            if let Some(error) = self
                                .translation_error
                                .as_ref()
                                .filter(|(id, _)| *id == row.id)
                                .map(|(_, error)| error.clone())
                            {
                                ui.colored_label(
                                    egui::Color32::LIGHT_RED,
                                    format!("번역 실패: {error}"),
                                );
                            }
                            if let Some(translated) = translated {
                                ui.add_space(8.0);
                                ui.heading("한국어 번역");
                                ui.label(translated);
                            }
                        }
                        if row.latest_version > row.version.max(row.update_version) {
                            ui.colored_label(
                                egui::Color32::LIGHT_RED,
                                format!(
                                    "업데이트 필요: {} → {}",
                                    common::version_to_str(Some(
                                        row.version.max(row.update_version)
                                    )),
                                    common::version_to_str(Some(row.latest_version))
                                ),
                            );
                        }
                    }
                    2 => {
                        if let Ok(urls) = serde_json::from_str::<Vec<String>>(&row.screenshots) {
                            if !urls.is_empty() {
                                ui.collapsing(
                                    format!("원본 스크린샷 링크 ({})", urls.len()),
                                    |ui| {
                                        for (index, url) in urls.iter().enumerate() {
                                            ui.hyperlink_to(format!("스크린샷 {}", index + 1), url);
                                        }
                                    },
                                );
                            }
                        }
                        if !screenshot_paths.is_empty() {
                            ui.label("저장된 스크린샷 (선택하면 열기)");
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                for path in screenshot_paths {
                                    let key = path.to_string_lossy().into_owned();
                                    if let Some(tex) = self.screenshot_textures.get(&key) {
                                        let texture_size = tex.size_vec2();
                                        let width = ui.available_width().clamp(1.0, 900.0);
                                        let height =
                                            width * texture_size.y / texture_size.x.max(1.0);
                                        let response = ui.add(
                                            egui::Image::new((tex.id(), egui::vec2(width, height)))
                                                .sense(egui::Sense::click()),
                                        );
                                        if response.clicked() {
                                            let _ = std::process::Command::new("explorer")
                                                .arg(&key)
                                                .spawn();
                                        }
                                        ui.add_space(8.0);
                                    }
                                }
                            });
                        } else {
                            ui.label("저장된 스크린샷이 없습니다.");
                        }
                    }
                    _ => {
                        if !row.missing_dlcs.is_empty() {
                            ui.colored_label(
                                egui::Color32::from_rgb(229, 192, 123),
                                format!("미보유 DLC 목록 (총 {}개)", row.missing_dlcs.len()),
                            );
                            for (id, name) in &row.missing_dlcs {
                                ui.label(format!("  - [{id}] {name}"));
                            }
                            ui.add_space(10.0);
                        }
                        if !row.owned_dlcs.is_empty() {
                            ui.colored_label(
                                egui::Color32::from_rgb(152, 195, 121),
                                format!("설치된 DLC 목록 (총 {}개)", row.owned_dlcs.len()),
                            );
                            for (id, name) in &row.owned_dlcs {
                                ui.label(format!("  + [{id}] {name}"));
                            }
                        }
                        if row.owned_dlcs.is_empty() && row.missing_dlcs.is_empty() {
                            ui.label(format!(
                                "DLC 정보가 없습니다. 보유 DLC: {} / DB 등록 DLC: {}",
                                row.dlc_count, row.db_dlc_count
                            ));
                        }
                    }
                }
            });
    }
}
