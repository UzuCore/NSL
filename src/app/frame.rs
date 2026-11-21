//! Drive the UI frame, task progress, popup dialogs, and panel layout.

use crate::*;

impl NslApp {
    /// Leave list shortcuts available while a row or paging button has focus,
    /// but never intercept arrows from the search text edits or any popup.
    pub(crate) fn management_navigation_enabled(&self, ctx: &egui::Context) -> bool {
        let dialog_open = self.rename_preview.is_some()
            || self.catalog_torrent_dialog.is_some()
            || self.download_delete_confirm.is_some()
            || self.bulk_rename_preview.is_some()
            || self.titledb_info.is_some()
            || self.clear_titledb_confirm
            || self.clear_library_confirm
            || self.remove_folder_confirm.is_some()
            || self.folder_notice.is_some()
            || self.orphan_cleanup_confirm
            || self.workspace_cleanup_confirm
            || self.update_info.is_some()
            || self.update_ready.is_some()
            || self.cache_info.is_some()
            || self.shortcuts_open
            || self.about_open
            || self.exit_confirm;
        !dialog_open
            && !self.settings_open
            && !ctx.memory(|memory| memory.any_popup_open())
            && !ctx.memory(|memory| {
                memory.has_focus(egui::Id::new("catalog_search"))
                    || memory.has_focus(egui::Id::new("download_search"))
            })
    }

    /// Close just one visible overlay per Esc press. Popup drawing order in
    /// `ui_frame` determines which dialog is on top (last drawn wins). Never
    /// confirm destructive actions, launch updates or cancel running jobs.
    fn dismiss_topmost_overlay(&mut self) -> bool {
        // Popup stack, in the reverse order of show_centered_popup calls.
        if self.exit_confirm {
            self.exit_confirm = false;
        } else if self.about_open {
            self.about_open = false;
        } else if self.shortcuts_open {
            self.shortcuts_open = false;
        } else if self.cache_info.is_some() {
            self.cache_info = None;
        } else if self.update_ready.is_some() {
            self.update_ready = None;
        } else if self.update_info.is_some() {
            self.update_info = None;
        } else if self.workspace_cleanup_confirm {
            self.workspace_cleanup_confirm = false;
        } else if self.orphan_cleanup_confirm {
            self.orphan_cleanup_confirm = false;
        } else if self.folder_notice.is_some() {
            self.folder_notice = None;
        } else if self.remove_folder_confirm.is_some() {
            self.remove_folder_confirm = None;
        } else if self.clear_library_confirm {
            self.clear_library_confirm = false;
        } else if self.clear_titledb_confirm {
            self.clear_titledb_confirm = false;
        } else if self.titledb_info.is_some() {
            self.titledb_info = None;
        } else if self.bulk_rename_preview.is_some() {
            self.bulk_rename_preview = None;
        } else if self.download_delete_confirm.is_some() {
            self.download_delete_confirm = None;
        } else if self.catalog_torrent_dialog.is_some() {
            // Dropping an outstanding preview also cancels its metadata request.
            self.catalog_torrent_dialog = None;
            self.status = "토렌트 파일 선택 취소됨".into();
        } else if self.rename_preview.is_some() {
            self.rename_preview = None;
        } else if self.settings_open {
            self.settings_open = false;
        } else if self.log_open {
            // The log is a docked panel, not a popup: close it only once no
            // other dialog or settings screen is awaiting dismissal.
            self.log_open = false;
        } else {
            return false;
        }
        true
    }

    /// Display every running operation in the same footer, with a measured
    /// fraction where available. The first foreground task wins the visible
    /// slot; simultaneous tasks remain discoverable in its hover tooltip.
    pub(crate) fn active_progress_views(&self) -> Vec<ProgressView> {
        let mut views = Vec::new();
        if self.scan_result.is_some() {
            views.push(if let Some(progress) = &self.scan_progress {
                ProgressView::counted(
                    "게임 파일 스캔",
                    progress.completed.load(Ordering::Relaxed) as u64,
                    progress.total.load(Ordering::Relaxed) as u64,
                )
            } else {
                ProgressView::busy("게임 파일 스캔")
            });
        }
        if self.titledb_result.is_some() {
            views.push(self.titledb_progress.as_ref().map_or_else(
                || ProgressView::busy("타이틀 DB 다운로드/매칭"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        if self.media_result.is_some() {
            views.push(if let Some(progress) = &self.media_progress {
                let (done, total, saved) = progress.snapshot();
                let mut view = ProgressView::counted(
                    &format!("미디어 수집 · {}", progress.stage_label()),
                    done as u64,
                    total as u64,
                );
                view.detail = format!(
                    "{} / {total} · 저장 {saved} · 실패 {}",
                    done.min(total),
                    progress.failures()
                );
                view
            } else {
                ProgressView::busy("미디어 수집")
            });
        }
        if self.catalog_result.is_some() {
            views.push(self.catalog_progress.as_ref().map_or_else(
                || ProgressView::busy("게임 카탈로그 다운로드"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        if let Some(batch) = &self.update_batch {
            if batch.running {
                let (done, total) = batch.progress();
                views.push(ProgressView::counted(
                    if batch.kind == update_batch::BatchKind::Dlc {
                        "미보유 DLC 카탈로그 대조"
                    } else {
                        "미보유 업데이트 카탈로그 대조"
                    },
                    done as u64,
                    total as u64,
                ));
            }
            if let Some(job) = batch
                .rows
                .iter()
                .filter_map(|row| row.job.as_ref())
                .find(|job| job.verifying.is_some())
            {
                views.push(job.merge_control.as_ref().map_or_else(
                    || {
                        ProgressView::busy(if batch.kind == update_batch::BatchKind::Dlc {
                            "DLC 병합·재압축"
                        } else {
                            "업데이트 병합·재압축"
                        })
                    },
                    |control| {
                        if control.is_cancel_requested() {
                            ProgressView::busy("업데이트 병합 중단 처리 중")
                        } else {
                            ProgressView::from_task(control.progress.snapshot())
                        }
                    },
                ));
            }
        }
        if self
            .catalog_torrent_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.request.is_some())
        {
            // Treat magnet metadata resolution as progress only while the
            // actual request is alive, never as a sticky status string.
            views.push(ProgressView::busy("토렌트 파일 목록 확인"));
        }
        if self.bulk_rename_result.is_some() {
            views.push(self.rename_progress.as_ref().map_or_else(
                || ProgressView::busy("파일명 일괄 정규화"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        if self.update_prepare_result.is_some() {
            views.push(self.update_progress.as_ref().map_or_else(
                || ProgressView::busy("프로그램 업데이트 다운로드/검증"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        for (active, label) in [
            (self.decompress_result.is_some(), "NSZ/XCZ 압축 해제"),
            (self.orphan_cleanup_result.is_some(), "미디어 캐시 정리"),
            (
                self.workspace_cleanup_result.is_some(),
                "오래된 변환 폴더 정리",
            ),
            (self.update_check_result.is_some(), "프로그램 업데이트 확인"),
            (self.translate_result.is_some(), "게임 설명 번역"),
            (self.catalog_cover_queue.is_some(), "스토어 커버 다운로드"),
        ] {
            if active {
                views.push(ProgressView::busy(label));
            }
        }
        let transfers: Vec<_> = self
            .torrent_progress
            .iter()
            .map(|(_, p)| p)
            .filter(|p| !p.finished && p.error.is_none() && !p.paused)
            .collect();
        if !transfers.is_empty() {
            let completed = transfers
                .iter()
                .fold(0_u64, |acc, p| acc.saturating_add(p.progress_bytes));
            let total = transfers
                .iter()
                .fold(0_u64, |acc, p| acc.saturating_add(p.total_bytes));
            let label = format!("토렌트 다운로드 · {}개", transfers.len());
            views.push(if transfers.iter().all(|p| p.total_bytes > 0) {
                let mut view = ProgressView::counted(&label, completed, total);
                let speed = transfers
                    .iter()
                    .fold(0_u64, |acc, p| acc.saturating_add(p.download_speed_bps));
                view.detail = format!(
                    "{} / {} · {}/s",
                    common::format_size(completed),
                    common::format_size(total),
                    common::format_size(speed)
                );
                view
            } else {
                ProgressView::busy(&label)
            });
        }
        if self.reload_result.is_some() {
            views.push(ProgressView::busy("게임 목록 갱신"));
        }
        views
    }

    pub(crate) fn torrent_poll_due(&mut self) -> bool {
        let now = std::time::Instant::now();
        if self.torrent_polled_at.is_some_and(|previous| {
            now.saturating_duration_since(previous) < std::time::Duration::from_millis(300)
        }) {
            return false;
        }
        self.torrent_polled_at = Some(now);
        true
    }

    pub(crate) fn ui_frame(&mut self, ctx: &egui::Context) {
        self.refresh_resource_sample(ctx);
        let theme_id = self
            .config
            .settings
            .get("theme")
            .or_else(|| self.config.settings.get("ui_theme"))
            .and_then(|v| v.as_str())
            .unwrap_or("github_dark")
            .to_owned();
        if self.active_theme != theme_id {
            theme::apply(ctx, &theme_id);
            self.active_theme = theme_id;
        }
        while let Ok(line) = self.log_receiver.try_recv() {
            self.log_lines.push(line);
        }
        if self.log_lines.len() > 2000 {
            self.log_lines.drain(..self.log_lines.len() - 2000);
        }
        let (ctrl_f, ctrl_o, f5, escape, enter) = ctx.input(|input| {
            (
                input.modifiers.ctrl && input.key_pressed(egui::Key::F),
                input.modifiers.ctrl && input.key_pressed(egui::Key::O),
                input.key_pressed(egui::Key::F5),
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::Enter),
            )
        });
        if ctrl_f {
            if !self.settings_open && self.folder_filter == "CATALOG" {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("catalog_search")));
            } else if !self.settings_open && self.folder_filter == "DOWNLOADS" {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("download_search")));
            } else {
                self.search_focus = true;
            }
        }
        if ctrl_o {
            self.settings_open = !self.settings_open;
        }
        if f5 && self.scan_result.is_none() {
            self.start_scan(true);
        }
        if escape {
            // egui menus and combo-boxes are foreground popups; close them
            // before our centered windows or the docked log panel.
            let native_popup_open = ctx.memory(|memory| memory.any_popup_open());
            let dismissed = if native_popup_open {
                ctx.memory_mut(|memory| memory.close_popup());
                true
            } else {
                self.dismiss_topmost_overlay()
            };
            if dismissed {
                // Prevent any widget underneath the dismissed dialog from
                // processing this same Escape keystroke a second time.
                ctx.input_mut(|input| {
                    let modifiers = input.modifiers;
                    input.consume_key(modifiers, egui::Key::Escape);
                });
            } else {
                // TextEdit changes its buffer during this frame, so clearing
                // only `search` can leave the committed query filtering the
                // list indefinitely until the user types again.
                self.search.clear();
                self.search_committed.clear();
                self.search_pending = None;
                self.search_focus = false;
                self.current_page = 0;
                self.filtered_key = None;
                if self.folder_filter == "CATALOG" {
                    self.catalog_search.clear();
                    self.catalog_search_committed.clear();
                    self.catalog_search_pending = None;
                    self.catalog_search_cache = None;
                    self.catalog_page = 0;
                    self.catalog_scroll_reset = true;
                } else if self.folder_filter == "DOWNLOADS" {
                    self.download_search.clear();
                    self.download_page = 0;
                    self.download_scroll_reset = true;
                }
            }
        }
        if enter
            && !ctx.wants_keyboard_input()
            && !self.settings_open
            && !matches!(self.folder_filter.as_str(), "CATALOG" | "DOWNLOADS")
            && !ctx.memory(|memory| memory.any_popup_open())
        {
            if let Some(row) = self
                .selected
                .and_then(|id| self.rows.iter().find(|row| row.id == id))
                .cloned()
            {
                self.run_file(&row);
            }
        }
        let active_tasks = [
            (self.scan_result.is_some(), "게임 파일 스캔"),
            (self.titledb_result.is_some(), "타이틀 DB 작업"),
            (self.media_result.is_some(), "미디어 수집"),
            (self.decompress_result.is_some(), "NSZ/XCZ 압축 해제"),
            (self.bulk_rename_result.is_some(), "파일명 일괄 정규화"),
            (self.update_check_result.is_some(), "업데이트 확인"),
            (self.update_prepare_result.is_some(), "업데이트 다운로드"),
            (self.orphan_cleanup_result.is_some(), "미디어 캐시 정리"),
            (
                self.workspace_cleanup_result.is_some(),
                "오래된 변환 폴더 정리",
            ),
            (self.translate_result.is_some(), "설명 번역"),
            (self.catalog_result.is_some(), "카탈로그 다운로드"),
            (
                self.torrent_progress
                    .iter()
                    .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused),
                "토렌트 다운로드",
            ),
        ]
        .into_iter()
        .filter_map(|(active, label)| active.then_some(label))
        .collect::<Vec<_>>();
        // Background scans/downloads must not steal focus by opening the log
        // window. The toolbar has a dedicated log toggle for users who want it.
        if self
            .torrent_progress
            .iter()
            .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested && !self.exit_confirmed && !active_tasks.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.exit_confirm = true;
        }
        let scan_done = poll_background(&self.scan_result, "scan");
        if let Some(done) = scan_done {
            self.scan_result = None;
            self.scan_cancel = None;
            self.scan_progress = None;
            match done {
                Ok((n, removed, cancelled, scan_errors)) => {
                    self.last_scan_errors = scan_errors;
                    self.status = if cancelled {
                        format!("스캔 중지됨 · {n}개 파일 반영 · 기존 파일 기록은 유지")
                    } else {
                        format!("{n}개 파일 검색 완료 · {removed}개 누락 기록 제거")
                    };
                    // Remember the scan schema that produced these records so the
                    // forced re-analysis happens only once.
                    if !cancelled && scan_errors == 0 {
                        self.config.settings.insert(
                            "scan_schema".into(),
                            serde_json::Value::String(SCAN_SCHEMA_VERSION.into()),
                        );
                        let _ =
                            database::save_config(&self.base_dir.join("config.json"), &self.config);
                    }
                    self.pending_icon_prefetch = !cancelled
                        && scan_errors == 0
                        && !self.headless_mode
                        && self
                            .config
                            .settings
                            .get("scan_icon")
                            .and_then(|v| v.as_str())
                            .unwrap_or("1")
                            == "1";
                    self.pending_screenshot_prefetch = !cancelled
                        && scan_errors == 0
                        && !self.headless_mode
                        && self
                            .config
                            .settings
                            .get("scan_screenshot")
                            .and_then(|v| v.as_str())
                            .unwrap_or("1")
                            == "1";
                    if scan_errors > 0 {
                        self.status =
                            format!("스캔 완료, 읽기/분석 오류 {scan_errors}개 (기존 기록 보존)");
                    }
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.scan_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let titledb_done = poll_background(&self.titledb_result, "titledb");
        if let Some(done) = titledb_done {
            self.titledb_result = None;
            self.titledb_progress = None;
            match done {
                Ok(titledb::UpdateOutcome::AlreadyLatest) => {
                    self.status = "타이틀 DB는 이미 최신입니다".into();
                }
                Ok(titledb::UpdateOutcome::Updated(n)) => {
                    self.status = format!("타이틀 DB {n}개 항목 다운로드 및 반영 완료");
                    self.invalidate_catalog_title_details();
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.titledb_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let reload_done = poll_background(&self.reload_result, "reload");
        if let Some(done) = reload_done {
            self.reload_result = None;
            match done {
                Ok(rows) => {
                    self.rows = rows;
                    self.catalog_owned = catalog_owned_summaries(&self.rows);
                    self.catalog_search_cache = None;
                    self.cached_screenshot_selection = None;
                    self.rows_generation = self.rows_generation.wrapping_add(1);
                    match self.db.stats() {
                        Ok(stats) => self.stats = stats,
                        Err(error) => self.status = format!("DB 통계를 읽지 못했습니다: {error}"),
                    }
                    if self.view_mode == "grid" && self.selected.is_none() {
                        self.selected = library::visible(&self.rows, &self.search_committed)
                            .filter(|row| self.row_matches_current_filter(row))
                            .min_by(|a, b| {
                                a.title_name
                                    .to_lowercase()
                                    .cmp(&b.title_name.to_lowercase())
                            })
                            .map(|row| row.id);
                    }
                }
                Err(e) => {
                    self.status = format!("목록 갱신 실패: {e}");
                    self.pending_icon_prefetch = false;
                    self.pending_screenshot_prefetch = false;
                }
            }
            if self.reload_dirty {
                self.start_reload();
            }
        } else if self.reload_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let bulk_rename_done = poll_background(&self.bulk_rename_result, "bulk rename");
        if let Some(done) = bulk_rename_done {
            self.bulk_rename_result = None;
            self.rename_progress = None;
            match done {
                Ok((renamed, failed)) => {
                    self.status = format!("일괄 정규화 완료: 변경 {renamed}개, 실패 {failed}개");
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.bulk_rename_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let catalog_loaded = match self.catalog_load_result.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => {
                self.catalog_load_result = None;
                result
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.catalog_load_result = None;
                None
            }
            _ => None,
        };
        if let Some(state) = catalog_loaded {
            if self.catalog_state.items.is_empty() && self.catalog_result.is_none() {
                self.catalog_state = state;
                self.invalidate_catalog_title_details();
                self.catalog_search_cache = None;
                self.catalog_page = 0;
            }
        } else if self.catalog_load_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let catalog_done = poll_background(&self.catalog_result, "catalog");
        if let Some(done) = catalog_done {
            self.catalog_result = None;
            self.catalog_progress = None;
            match done {
                Ok(state) => {
                    self.status = if state.not_modified {
                        format!("카탈로그 이미 최신 · {}개 항목", state.items.len())
                    } else {
                        format!("카탈로그 {}개 항목 준비 완료", state.items.len())
                    };
                    // The background worker already parsed and validated the
                    // state. Move it into the UI without a second JSON read
                    // or parse, which could block the frame on large catalogs.
                    self.catalog_state = state;
                    self.invalidate_catalog_title_details();
                    self.catalog_page = 0;
                    self.catalog_search_cache = None;
                    self.catalog_matching_indices.clear();
                    self.catalog_cover_retry_after.clear();
                    self.catalog_cover_missing.clear();
                }
                Err(e) => self.status = e,
            }
        } else if self.catalog_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(done) =
            poll_background(&self.catalog_title_load_result, "catalog title metadata")
        {
            self.catalog_title_load_result = None;
            self.catalog_title_metadata_loaded = true;
            match done {
                Ok(details) => {
                    self.catalog_title_details = details;
                    self.catalog_search_cache = None;
                    ctx.request_repaint();
                }
                Err(error) => {
                    let _ = self.log_sender.send(format!(
                        "스토어 타이틀 DB 정보 조회 실패: {error} (기본 카탈로그 이름 사용)"
                    ));
                }
            }
        } else if self.catalog_title_load_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let cover_done = match self.catalog_cover_queue.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(done)) => Some(done),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.catalog_cover_queue = None;
                self.status = "스토어 커버 수집 작업이 비정상 종료되었습니다".into();
                None
            }
            _ => None,
        };
        if let Some(done) = cover_done {
            self.catalog_cover_queue = None;
            let (fetched, failed) = done;
            let retry_at = std::time::Instant::now() + STORE_COVER_RETRY_DELAY;
            for topic_id in failed {
                self.catalog_cover_retry_after.insert(topic_id, retry_at);
            }
            if fetched > 0 {
                self.catalog_cover_missing.clear();
                self.status = format!("스토어 커버 {fetched}개 받음");
                ctx.request_repaint();
            }
        } else if self.catalog_cover_queue.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if self.torrent_engine.is_some() && self.torrent_poll_due() {
            if let Some(engine) = self.torrent_engine.clone() {
                let was_active = self
                    .torrent_progress
                    .iter()
                    .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused);
                let (snapshot, completions) = engine.poll();
                let now_active = snapshot
                    .iter()
                    .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused);
                if was_active && !now_active {
                    self.start_reload();
                }
                for notice in completions {
                    let (key, result) = match notice {
                        torrent::EngineNotice::Download { key, result } => (key, result),
                        torrent::EngineNotice::Command {
                            key,
                            action,
                            result,
                        } => {
                            if let Err(error) = result {
                                self.status = format!("다운로드 {action} 명령 실패: {error}");
                                let _ = self
                                    .log_sender
                                    .send(format!("다운로드 명령 실패 ({key}): {error}"));
                            }
                            continue;
                        }
                    };
                    self.handle_missing_update_completion(&key, &result);
                    let label = snapshot
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, p)| p.name.clone())
                        .unwrap_or_else(|| key.clone());
                    match result {
                        Ok(()) => {
                            let _ = self
                                .log_sender
                                .send(format!("✅ 다운로드 완료 · {}", truncate_label(&label, 60)));
                            self.status = format!("다운로드 완료: {}", truncate_label(&label, 60));
                        }
                        Err(error) => {
                            let _ = self.log_sender.send(format!(
                                "❌ 다운로드 실패 · {} · {error}",
                                truncate_label(&label, 60)
                            ));
                        }
                    }
                }
                if self.torrent_progress.len() != snapshot.len() {
                    self.cached_tabs_generation = u64::MAX;
                }
                if self
                    .download_selected
                    .as_deref()
                    .is_some_and(|key| !snapshot.iter().any(|(current, _)| current == key))
                {
                    self.download_selected = None;
                }
                self.torrent_progress = snapshot;
            }
        }
        self.poll_missing_update_audit(ctx);
        if self.batch_rescan_pending && self.scan_result.is_none() {
            self.batch_rescan_pending = false;
            self.start_scan(true);
        }
        let update_check_done = poll_background(&self.update_check_result, "update check");
        if let Some(done) = update_check_done {
            self.update_check_result = None;
            let automatic = std::mem::take(&mut self.update_check_automatic);
            match done {
                Ok(info) if info.is_newer && info.download_url.is_some() => {
                    self.update_info = Some(info);
                }
                Ok(info) if info.is_newer => {
                    self.status = format!(
                        "커밋 {}가 있지만 호환되는 ZIP이 릴리스에 없습니다",
                        info.latest_version
                    );
                }
                Ok(info) => {
                    if !automatic {
                        self.status = format!("현재 커밋 {}가 최신입니다", info.current_version);
                    }
                }
                Err(e) => {
                    if !automatic {
                        self.status = format!("업데이트 확인 실패: {e}");
                    }
                }
            }
        } else if self.update_check_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let update_prepare_done = poll_background(&self.update_prepare_result, "update prepare");
        if let Some(done) = update_prepare_done {
            self.update_prepare_result = None;
            self.update_progress = None;
            match done {
                Ok(path) => {
                    self.status = "업데이트 패키지를 검증했습니다".into();
                    self.update_ready = Some(path);
                }
                Err(e) => self.status = format!("업데이트 준비 실패: {e}"),
            }
        } else if self.update_prepare_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let orphan_cleanup_done = poll_background(&self.orphan_cleanup_result, "orphan cleanup");
        if let Some(done) = orphan_cleanup_done {
            self.orphan_cleanup_result = None;
            match done {
                Ok((icons, screenshots)) => {
                    self.status = format!(
                        "캐시 정리 완료 · 아이콘 {icons}개, 스크린샷 폴더 {screenshots}개 제거"
                    );
                }
                Err(e) => self.status = format!("캐시 정리 실패: {e}"),
            }
        } else if self.orphan_cleanup_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let workspace_cleanup_done =
            poll_background(&self.workspace_cleanup_result, "workspace cleanup");
        if let Some(done) = workspace_cleanup_done {
            self.workspace_cleanup_result = None;
            match done {
                Ok(count) => {
                    self.status = format!("오래된 변환 폴더 {count}개 정리 완료");
                    let _ = self.log_sender.send(self.status.clone());
                }
                Err(error) => self.status = format!("변환 폴더 정리 실패: {error}"),
            }
        } else if self.workspace_cleanup_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.start_pending_media();
        let media_done = poll_background(&self.media_result, "media");
        if let Some(done) = media_done {
            self.media_result = None;
            self.media_progress = None;
            match done {
                Ok(outcome) => {
                    self.status = if outcome.cancelled {
                        format!("미디어 수집 중지됨 · {}개 저장", outcome.saved_total())
                    } else if outcome.failed > 0 {
                        format!(
                            "미디어 수집 종료 · {}개 저장 · {}개 실패 (로그 확인)",
                            outcome.saved_total(),
                            outcome.failed
                        )
                    } else {
                        format!(
                            "미디어 수집 완료 · 아이콘 {}개 · 스크린샷 {}장 · 캐시 {}개 재사용",
                            outcome.icons, outcome.screenshots, outcome.skipped
                        )
                    };
                    self.invalidate_icons();
                    self.screenshot_textures.clear();
                    self.screenshot_missing.clear();
                    self.cached_screenshot_selection = None;
                    self.start_pending_media();
                }
                Err(e) => self.status = e,
            }
        } else if self.media_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let decompress_done = poll_background(&self.decompress_result, "decompress");
        if let Some(done) = decompress_done {
            self.decompress_result = None;
            match done {
                Ok(path) => {
                    self.status = format!(
                        "압축 해제 완료: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    self.converted_file = Some(path);
                    if self.launch_after_decompress {
                        self.launch_after_decompress = false;
                        if let Some(path) = self.converted_file.clone() {
                            if let Err(e) = self.launch_path(&path) {
                                self.status = format!("실행 실패: {e}")
                            }
                        }
                    }
                }
                Err(e) => {
                    self.launch_after_decompress = false;
                    self.status = e
                }
            }
        } else if self.decompress_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let translate_done = match self.translate_result.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(done)) => Some(done),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.translate_result = None;
                self.status = "설명 번역 작업이 비정상 종료되었습니다".into();
                None
            }
            _ => None,
        };
        if let Some(done) = translate_done {
            self.translate_result = None;
            match done {
                (id, Ok(translated)) => {
                    self.status = "설명 번역 완료".into();
                    self.translation_error = None;
                    self.translated_description = Some((id, translated));
                }
                (id, Err(e)) => {
                    self.status = format!("번역 실패: {e}");
                    self.translation_error = Some((id, e));
                }
            }
        } else if self.translate_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        if self.texture_selection != self.selected {
            self.image_loader.cancel_stale_screenshots(self.selected);
            self.screenshot_textures.clear();
            self.screenshot_missing.clear();
            self.texture_selection = self.selected;
        }
        self.poll_loaded_images(ctx);
        if let Some(row) = self
            .selected
            .and_then(|id| self.rows.iter().find(|r| r.id == id))
            .cloned()
        {
            let key = row.title_id.clone();
            let mut budget = ICON_LOAD_BUDGET;
            self.ensure_icon_texture(ctx, &key, &mut budget);
            self.touch_icon(&key);
        }
        self.refresh_library_stats();
        let selected_screenshot_paths = self.selected_screenshot_paths_cached();
        // Only decode screenshots when that detail tab is visible. Selecting
        // a row on the text tabs must not synchronously load up to six large
        // images or allocate their full-resolution intermediate buffers.
        if self.detail_tab == 2 && self.selected.is_some() {
            for path in &selected_screenshot_paths {
                let key = path.to_string_lossy().into_owned();
                if !self.screenshot_textures.contains_key(&key)
                    && !self.screenshot_missing.contains(&key)
                {
                    self.image_loader.request(
                        ImageKind::Screenshot,
                        &key,
                        path.clone(),
                        900,
                        self.selected,
                    );
                }
            }
            if self.image_loader.has_pending() {
                ctx.request_repaint_after(std::time::Duration::from_millis(45));
            }
        }
        let search_before = self.search.clone();
        let filter_before = self.kind_filter.clone();
        let folder_filter_before = self.folder_filter.clone();
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            top_bar(self, ui, ctx, &active_tasks);
        });
        if self.search != search_before {
            self.search_pending =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
        }
        if let Some(deadline) = self.search_pending {
            if std::time::Instant::now() >= deadline {
                self.search_pending = None;
                if self.search_committed != self.search {
                    self.search_committed = self.search.clone();
                }
            }
        }
        if self.search_committed != search_before || self.kind_filter != filter_before {
            self.current_page = 0;
        }
        if self.kind_filter != filter_before && !self.folder_filter.is_empty() {
            self.folder_filter.clear();
            self.config.settings.insert(
                "last_tab_key".into(),
                serde_json::Value::String(String::new()),
            );
            let _ = database::save_config(&self.base_dir.join("config.json"), &self.config);
        }
        if self.folder_filter != folder_filter_before {
            self.current_page = 0;
        }
        if self.search_committed != search_before
            || self.kind_filter != filter_before
            || self.folder_filter != folder_filter_before
        {
            self.selected = None;
        }
        if self.search_pending.is_some() || self.catalog_search_pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Pin the status bar FIRST, so the optional log panel is always ABOVE it.
        // Match the Python version: status | 180px progress | RAM/CPU.
        let activities = self.active_progress_views();
        let primary = activities.first();
        let status_message = if let Some(task) = primary {
            if activities.len() > 1 {
                format!(
                    "{} · {} · 외 {}개 작업",
                    task.label,
                    task.detail,
                    activities.len() - 1
                )
            } else {
                format!("{} · {}", task.label, task.detail)
            }
        } else {
            self.status.clone()
        };
        let progress_help = if activities.is_empty() {
            "진행 중인 작업 없음".to_owned()
        } else {
            activities
                .iter()
                .map(|a| format!("{} · {}", a.label, a.detail))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let status_panel = egui::TopBottomPanel::bottom("status")
            .exact_height(31.0)
            .frame(
                egui::Frame::none()
                    .fill(ctx.style().visuals.widgets.inactive.bg_fill)
                    .inner_margin(egui::Margin::symmetric(9.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let width = ui.available_width();
                    let show_resource = width >= 580.0;
                    let resource_text = self.resource_usage_text();
                    let resource_width = if show_resource {
                        ui.painter()
                            .layout_no_wrap(
                                resource_text.clone(),
                                egui::TextStyle::Monospace.resolve(ui.style()),
                                ui.visuals().weak_text_color(),
                            )
                            .size()
                            .x
                            + 6.0
                    } else {
                        0.0
                    };
                    let bar_width = if width >= 360.0 { 180.0 } else { 100.0 };
                    let label_width = (width - resource_width - bar_width - 26.0).max(5.0);
                    // Keep the text anchored at the *left edge* of its slot.
                    // egui::Label inside add_sized can center the galley in
                    // this horizontal layout, even with Label::halign(Min).
                    let (message_rect, message_response) =
                        ui.allocate_exact_size(egui::vec2(label_width, 19.0), egui::Sense::hover());
                    ui.painter().with_clip_rect(message_rect).text(
                        message_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        &status_message,
                        egui::FontId::proportional(12.0),
                        ui.visuals().text_color(),
                    );
                    message_response.on_hover_text(&status_message);
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_status_message"),
                            status_message.clone(),
                        );
                    });
                    let progress_response =
                        if primary.is_some_and(|task| task.fraction.is_none()) {
                            // An animated segment, not a fabricated 0-100% value,
                            // for network waits, parsing, unpacking, and other work
                            // without a measurable total.
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(bar_width, 15.0),
                                egui::Sense::hover(),
                            );
                            let painter = ui.painter();
                            painter.rect_filled(rect, 7.5, ui.visuals().extreme_bg_color);
                            let segment_width = (bar_width * 0.30).max(26.0);
                            let cycle = (ctx.input(|i| i.time) as f32 * 0.65).fract();
                            let x =
                                rect.left() + (bar_width + segment_width) * cycle - segment_width;
                            let segment = egui::Rect::from_min_size(
                                egui::pos2(x, rect.top()),
                                egui::vec2(segment_width, rect.height()),
                            )
                            .intersect(rect);
                            if segment.is_positive() {
                                painter.rect_filled(segment, 6.0, ui.visuals().hyperlink_color);
                            }
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "작업 중",
                                egui::TextStyle::Small.resolve(ui.style()),
                                ui.visuals().text_color(),
                            );
                            ctx.request_repaint_after(std::time::Duration::from_millis(80));
                            response
                        } else if let Some(task) = primary {
                            ui.add(
                                egui::ProgressBar::new(task.fraction.unwrap_or(0.0))
                                    .desired_width(bar_width)
                                    .desired_height(15.0)
                                    .show_percentage(),
                            )
                        } else {
                            // Idle: keep the Python-style empty progress rail
                            // visible without egui's misleading 0%-fill dot.
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(bar_width, 15.0),
                                egui::Sense::hover(),
                            );
                            ui.painter()
                                .rect_filled(rect, 7.5, ui.visuals().extreme_bg_color);
                            response
                        }
                        .on_hover_text(&progress_help);
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_status_progress_rect"),
                            progress_response.rect,
                        );
                    });
                    #[cfg(not(test))]
                    let _ = progress_response;
                    if show_resource {
                        ui.separator();
                        ui.label(
                            egui::RichText::new(resource_text)
                                .monospace()
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
            });
        #[cfg(test)]
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_status_rect"),
                status_panel.response.rect,
            );
        });
        #[cfg(not(test))]
        let _ = status_panel;

        let mut close_log = false;
        if self.log_open {
            let log_height = (ctx.available_rect().height() * 0.30).clamp(68.0, 174.0);
            let log_panel = egui::TopBottomPanel::bottom("operation_log_panel")
                .exact_height(log_height)
                .frame(
                    egui::Frame::none()
                        .fill(ctx.style().visuals.panel_fill)
                        .stroke(ctx.style().visuals.widgets.noninteractive.bg_stroke)
                        .inner_margin(egui::Margin::symmetric(10.0, 5.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "작업 로그  ·  {}개 메시지",
                                self.log_lines.len()
                            ))
                            .strong()
                            .size(12.0),
                        );
                        if ui.small_button("로그 지우기").clicked() {
                            self.log_lines.clear();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close_log = ui.small_button("×").on_hover_text("로그 닫기").clicked();
                        });
                    });
                    ui.separator();
                    let log_row_height = ui.text_style_height(&egui::TextStyle::Monospace)
                        + ui.spacing().item_spacing.y;
                    // ScrollArea::show_rows can request the *full* initial
                    // panel height despite a header above it. Reserve header,
                    // dividers and margins explicitly, or the frame can grow
                    // over the fixed status bar during window resizing.
                    let scroll_height = (log_height - 55.0).max(10.0);
                    let scroll_output = egui::ScrollArea::vertical()
                        .id_salt("operation_log")
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .min_scrolled_height(0.0)
                        .max_height(scroll_height)
                        .show_rows(ui, log_row_height, self.log_lines.len(), |ui, row_range| {
                            // Virtualized rows: thousands of messages do not cause
                            // full log layout on every repaint.
                            for line in &self.log_lines[row_range] {
                                let color = if line.contains("❌") || line.contains("⚠") {
                                    if ui.visuals().dark_mode {
                                        egui::Color32::LIGHT_RED
                                    } else {
                                        egui::Color32::from_rgb(207, 34, 46)
                                    }
                                } else if line.contains("✅") {
                                    if ui.visuals().dark_mode {
                                        egui::Color32::LIGHT_GREEN
                                    } else {
                                        egui::Color32::from_rgb(26, 127, 55)
                                    }
                                } else {
                                    ui.visuals().text_color()
                                };
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(line).monospace().color(color),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(line);
                            }
                        });
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_log_scroll_rect"),
                            scroll_output.inner_rect,
                        );
                    });
                    #[cfg(not(test))]
                    let _ = scroll_output;
                });
            #[cfg(test)]
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new("nsl_test_log_rect"), log_panel.response.rect);
            });
            #[cfg(not(test))]
            let _ = log_panel;
        } else {
            #[cfg(test)]
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new("nsl_test_log_rect"), egui::Rect::NOTHING);
            });
        }
        if close_log {
            self.log_open = false;
        }
        let settings_before = self.config.settings.clone();
        let folders_before = self.folders.clone();
        let name_mode_before = self
            .config
            .settings
            .get("name_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("db")
            .to_owned();
        let remember_sash = self
            .config
            .settings
            .get("sash_remember")
            .and_then(|v| v.as_str())
            .unwrap_or("1")
            == "1";
        let mut saved_detail_width = self
            .config
            .settings
            .get("sash_pos")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(440.0)
            .clamp(320.0, 900.0);
        if self.reset_panel_width {
            ctx.data_mut(|data| {
                data.remove::<egui::containers::panel::PanelState>(egui::Id::new("detail"));
            });
            saved_detail_width = 440.0;
            self.reset_panel_width = false;
        }
        // Management views get their own right-hand controls; keep the
        // library detail panel and its saved sash width completely separate.
        if !self.settings_open
            && self.folder_filter != "CATALOG"
            && self.folder_filter != "DOWNLOADS"
        {
            if let Some(row) = self
                .selected
                .and_then(|id| self.rows.iter().find(|row| row.id == id))
                .cloned()
            {
                let panel_id = egui::Id::new("detail");
                let resize_id = panel_id.with("__resize");
                // PanelState is egui's layout cache, not the user's preferred
                // sash width. A temporarily smaller window can clamp that
                // cache, so restore the saved preference once there is room.
                // Do not fight a real in-progress splitter drag.
                let dragging_before_layout = ctx
                    .read_response(resize_id)
                    .is_some_and(|response| response.dragged());
                if remember_sash && !dragging_before_layout {
                    if let Some(mut state) =
                        egui::containers::panel::PanelState::load(ctx, panel_id)
                    {
                        if (state.size().x - saved_detail_width).abs() > 1.0 {
                            state.rect.min.x = state.rect.max.x - saved_detail_width;
                            ctx.data_mut(|data| data.insert_persisted(panel_id, state));
                        }
                    }
                }
                let detail_panel = egui::SidePanel::right("detail")
                    .default_width(saved_detail_width)
                    .min_width(320.0)
                    .max_width(900.0)
                    .resizable(true)
                    .show(ctx, |ui| {
                        self.selected_detail_ui(ui, &row, &selected_screenshot_paths);
                    });
                // egui can change PanelState when child widgets need more
                // space or the window becomes narrower. Only a *real splitter
                // drag* should overwrite the user's saved width; otherwise a
                // long game title or a temporary window resize is persisted.
                let user_resizing = ctx
                    .read_response(resize_id)
                    .is_some_and(|response| response.dragged());
                if remember_sash && user_resizing {
                    let width = egui::containers::panel::PanelState::load(ctx, panel_id)
                        .map(|state| state.size().x)
                        .unwrap_or_else(|| detail_panel.response.rect.width());
                    if (width - saved_detail_width).abs() > 1.0 {
                        self.config.settings.insert(
                            "sash_pos".into(),
                            serde_json::Value::String(format!("{width:.0}")),
                        );
                    }
                }
            }
        }
        // Store and Downloads are one management workspace: use the SAME
        // resize handle, persisted width and egui panel cache on both tabs.
        // The library's detail panel ("detail"/"sash_pos") remains separate.
        if !self.settings_open && matches!(self.folder_filter.as_str(), "CATALOG" | "DOWNLOADS") {
            let panel_id = egui::Id::new("management_controls");
            let resize_id = panel_id.with("__resize");
            let saved_width = self
                .config
                .settings
                .get("management_sidebar_width")
                .and_then(|value| value.as_str())
                .and_then(|width| width.parse::<f32>().ok())
                .filter(|width| width.is_finite())
                .unwrap_or(282.0)
                .clamp(235.0, 365.0);
            // A temporarily narrow window or a longer control label may
            // clamp egui's layout cache. Never save that as a user resize.
            let was_dragging = ctx
                .read_response(resize_id)
                .is_some_and(|response| response.dragged());
            if !was_dragging {
                if let Some(mut state) = egui::containers::panel::PanelState::load(ctx, panel_id) {
                    if (state.size().x - saved_width).abs() > 1.0 {
                        state.rect.min.x = state.rect.max.x - saved_width;
                        ctx.data_mut(|data| data.insert_persisted(panel_id, state));
                    }
                }
            }
            let sidebar = egui::SidePanel::right(panel_id)
                .default_width(saved_width)
                .min_width(235.0)
                .max_width(365.0)
                .resizable(true)
                .frame(management_sidebar_frame(&ctx.style()))
                .show(ctx, |ui| {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        if self.folder_filter == "CATALOG" {
                            self.catalog_sidebar_ui(ui, ctx);
                        } else {
                            self.downloads_sidebar_ui(ui);
                        }
                    });
                });
            if ctx
                .read_response(resize_id)
                .is_some_and(|response| response.dragged())
            {
                let width = egui::containers::panel::PanelState::load(ctx, panel_id)
                    .map(|state| state.size().x)
                    .unwrap_or_else(|| sidebar.response.rect.width())
                    .clamp(235.0, 365.0);
                if (width - saved_width).abs() > 1.0 {
                    self.config.settings.insert(
                        "management_sidebar_width".into(),
                        serde_json::Value::String(format!("{width:.0}")),
                    );
                }
            }
            #[cfg(test)]
            ctx.data_mut(|data| {
                let marker = if self.folder_filter == "CATALOG" {
                    "nsl_test_store_sidebar_rect"
                } else {
                    "nsl_test_download_sidebar_rect"
                };
                data.insert_temp(egui::Id::new(marker), sidebar.response.rect);
            });
            #[cfg(not(test))]
            let _ = sidebar;
        }
        let central_panel = egui::CentralPanel::default();
        let central_panel = if self.settings_open {
            central_panel
                .frame(egui::Frame::none().fill(ctx.style().visuals.widgets.inactive.bg_fill))
        } else {
            central_panel
        };
        central_panel.show(ctx, |ui| {
            if self.settings_open {
                self.settings_panel_ui(ui);
            } else {
                let folder_tabs = self.library_tabs_cached().to_vec();
                let tab_font = egui::TextStyle::Button.resolve(ui.style());
                let tab_color = ui.visuals().text_color();
                let preferred_widths: Vec<f32> = folder_tabs
                    .iter()
                    .map(|(_, label)| {
                        ui.painter()
                            .layout_no_wrap(label.clone(), tab_font.clone(), tab_color)
                            .size()
                            .x
                            + 28.0
                    })
                    .collect();
                let tab_count = folder_tabs.len();
                let available_tab_width = ui.available_width();
                let tab_widths = fit_directory_tab_widths(
                    &preferred_widths,
                    (available_tab_width - (tab_count.saturating_sub(1) as f32) - 2.0)
                        .max(0.0),
                );
                #[cfg(test)]
                let mut rendered_tab_rects = Vec::with_capacity(tab_count);
                egui::ScrollArea::horizontal()
                    .id_salt("directory_tabs")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            for (index, (key, label)) in folder_tabs.into_iter().enumerate() {
                                let selected = self.folder_filter == key;
                                let response = draw_tab_button(
                                    ui,
                                    &label,
                                    selected,
                                    tab_widths[index],
                                    index,
                                    tab_count,
                                );
                                #[cfg(test)]
                                rendered_tab_rects.push(response.rect);
                                if response.clicked() {
                                    self.folder_filter = key.clone();
                                    self.kind_filter = "base".into();
                                    self.current_page = 0;
                                    self.config.settings.insert(
                                        "last_tab_key".into(),
                                        serde_json::Value::String(key),
                                    );
                                    let _ = database::save_config(
                                        &self.base_dir.join("config.json"),
                                        &self.config,
                                    );
                                }
                            }
                        });
                    });
                #[cfg(test)]
                ctx.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_directory_tab_layout"),
                        (available_tab_width, tab_widths.clone(), rendered_tab_rects),
                    );
                });
                ui.separator();
                let library_focus_id = ui.make_persistent_id("library_keyboard_navigation");
                let library_focus_rect = ui.available_rect_before_wrap();
                let _library_focus_anchor = ui.interact(
                    library_focus_rect,
                    library_focus_id,
                    egui::Sense::focusable_noninteractive(),
                );
                if self.folder_filter == "CATALOG" {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        self.catalog_panel_ui(ui, ctx);
                    });
                    return;
                }
                if self.folder_filter == "DOWNLOADS" {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        self.downloads_panel_ui(ui);
                    });
                    return;
                }
                if matches!(self.folder_filter.as_str(), "MISSING_UPDATES" | "MISSING_DLCS") {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        self.missing_update_toolbar(ui);
                    });
                }
                let (grid_columns, grid_card_width, grid_thumbnail_size) =
                    library_grid_dimensions(ui.available_width());
                if self.rows.is_empty() {
                    // Full-screen centered welcome, matching the Python build
                    // instead of a small inline box above an empty list.
                    let full = ui.available_rect_before_wrap();
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(full), |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space((full.height() * 0.28).max(40.0));
                            ui.heading(
                                egui::RichText::new("Nintendo Switch Library")
                                    .size(28.0)
                                    .strong(),
                            );
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("라이브러리가 비어 있습니다")
                                    .size(16.0),
                            );
                            ui.add_space(24.0);
                            ui.label(
                                "1. prod.keys를 프로그램 폴더 또는 %USERPROFILE%\\.switch에 둡니다.",
                            );
                            ui.label("2. 타이틀 DB를 다운로드하거나 로컬 DB를 매칭합니다.");
                            ui.label("3. 설정에서 스캔 폴더를 추가한 뒤 전체 스캔을 실행합니다.");
                            ui.add_space(16.0);
                            let has_keys = scanner::has_prod_keys();
                            if has_keys {
                                ui.colored_label(
                                    egui::Color32::from_rgb(46, 160, 67),
                                    "prod.keys 발견됨",
                                );
                            } else {
                                ui.colored_label(
                                    egui::Color32::from_rgb(207, 34, 46),
                                    "prod.keys 없음",
                                );
                            }
                            ui.add_space(20.0);
                            ui.horizontal(|ui| {
                                let total = ui.available_width();
                                ui.add_space((total * 0.5 - 140.0).max(0.0));
                                if ui.button("설정 열기").clicked() {
                                    self.settings_open = true;
                                }
                                if ui.button("타이틀 DB 다운로드").clicked() {
                                    self.start_titledb_download();
                                }
                                if ui.button("빠른 스캔").clicked() {
                                    self.start_scan(true);
                                }
                            });
                        });
                    });
                    return;
                }
                let matching = self.library_rows_cached();
                let paging_on = self
                    .config
                    .settings
                    .get("paging_on")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1")
                    == "1";
                let page_size = configured_game_page_size(&self.config.settings);
                let page_count = matching.len().div_ceil(page_size).max(1);
                self.current_page = self.current_page.min(page_count - 1);
                let previous_page = self.current_page;
                let (up, down, left, right, page_up, page_down, home, end, enter_key) = ctx.input(|input| {
                    // Leave control-modified arrows and text-entry keys alone,
                    // but also accept the familiar Ctrl+Home / Ctrl+End jumps.
                    let plain = !input.modifiers.ctrl && !input.modifiers.alt && !input.modifiers.shift;
                    let jump = !input.modifiers.alt && !input.modifiers.shift;
                    (
                        plain && input.key_pressed(egui::Key::ArrowUp),
                        plain && input.key_pressed(egui::Key::ArrowDown),
                        plain && input.key_pressed(egui::Key::ArrowLeft),
                        plain && input.key_pressed(egui::Key::ArrowRight),
                        plain && input.key_pressed(egui::Key::PageUp),
                        plain && input.key_pressed(egui::Key::PageDown),
                        jump && input.key_pressed(egui::Key::Home),
                        jump && input.key_pressed(egui::Key::End),
                        plain && input.key_pressed(egui::Key::Enter),
                    )
                });
                let grid_navigation = self.view_mode == "grid";
                // Page Up/Down move by approximately one *visible viewport*,
                // not by an entire 200-item DB page. Keep the stride aligned
                // to grid rows so vertical navigation preserves its column.
                let viewport_height = (ui.available_height() - if paging_on { 34.0 } else { 0.0 })
                    .max(1.0);
                let grid_row_stride = grid_thumbnail_size + 38.0 + 12.0 + ui.spacing().item_spacing.y;
                let visible_grid_rows = (viewport_height / grid_row_stride).floor().max(1.0) as usize;
                let page_jump = if grid_navigation {
                    visible_grid_rows.saturating_mul(grid_columns)
                } else {
                    ((viewport_height - 28.0).max(24.0) / 24.0).floor().max(1.0) as usize
                };
                let navigation_delta = if page_up {
                    Some(-(page_jump as isize))
                } else if page_down {
                    Some(page_jump as isize)
                } else if grid_navigation {
                    if up {
                        Some(-(grid_columns as isize))
                    } else if down {
                        Some(grid_columns as isize)
                    } else if left {
                        Some(-1)
                    } else if right {
                        Some(1)
                    } else {
                        None
                    }
                } else if up || left {
                    Some(-1)
                } else if down || right {
                    Some(1)
                } else {
                    None
                };
                let library_has_focus = ctx.memory(|memory| memory.has_focus(library_focus_id));
                let search_has_focus =
                    ctx.memory(|memory| memory.has_focus(egui::Id::new("library_search")));
                let keyboard_navigation_enabled = library_has_focus || !search_has_focus;
                let mut keyboard_scroll_to_selection = self
                    .keyboard_reveal_next_frame.take()
                    .is_some_and(|id| self.selected == Some(id));
                if keyboard_navigation_enabled && !matching.is_empty() {
                    let current_index = matching
                        .iter()
                        .position(|row| self.selected == Some(row.id));
                    let visible_start = if paging_on { self.current_page * page_size } else { 0 };
                    let visible_end = if paging_on {
                        visible_start.saturating_add(page_size).min(matching.len())
                    } else {
                        matching.len()
                    };
                    let target_index = library_navigation_target(
                        current_index,
                        matching.len(),
                        visible_start,
                        visible_end,
                        navigation_delta,
                        home,
                        end,
                    );
                    if let Some(target_index) = target_index {
                        let target = &matching[target_index];
                        keyboard_scroll_to_selection = true;
                        self.selected = Some(target.id);
                        self.keyboard_reveal_next_frame = Some(target.id);
                        ctx.request_repaint();
                        ctx.memory_mut(|memory| memory.request_focus(library_focus_id));
                        if paging_on {
                            self.current_page = target_index / page_size;
                        }
                    }
                }
                if keyboard_navigation_enabled && enter_key {
                    if let Some(row) = matching
                        .iter()
                        .find(|row| self.selected == Some(row.id))
                        .cloned()
                    {
                        self.run_file(&row);
                    }
                }
                if paging_on {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.current_page > 0, egui::Button::new("이전"))
                            .clicked()
                        {
                            self.current_page -= 1;
                        }
                        ui.label(format!(
                            "페이지 {} / {} · {}개 항목",
                            self.current_page + 1,
                            page_count,
                            matching.len()
                        ));
                        if ui
                            .add_enabled(
                                self.current_page + 1 < page_count,
                                egui::Button::new("다음"),
                            )
                            .clicked()
                        {
                            self.current_page += 1;
                        }
                    });
                }
                let library_page_changed = self.current_page != previous_page;
                // `matching` is already filtered+sorted by the cache. Only the
                // visible page is cloned into owned rows for rendering.
                let page_start = if paging_on {
                    self.current_page * page_size
                } else {
                    0
                };
                let page_end = (page_start + if paging_on { page_size } else { matching.len() })
                    .min(matching.len());
                let page_items: &[library::GameFile] = &matching[page_start..page_end];
                if self.view_mode == "grid" {
                    // Virtualized grid: only rows inside the viewport are built.
                    // Building every card (800+ icons) each frame was the main
                    // cause of the library stutter.
                    let row_height = grid_thumbnail_size + 38.0 + 12.0;
                    let total_rows = page_items.len().div_ceil(grid_columns.max(1));
                    let mut grid_scroll = egui::ScrollArea::vertical()
                        .id_salt("library_grid_rows")
                        .animated(false)
                        .auto_shrink([false, false]);
                    if keyboard_scroll_to_selection {
                        if let Some(position) = page_items.iter().position(|row| self.selected == Some(row.id)) {
                            let id = ui.make_persistent_id(egui::Id::new("library_grid_rows"));
                            let old_offset = egui::scroll_area::State::load(ctx, id)
                                .map(|state| state.offset.y).unwrap_or(0.0);
                            let row_stride = row_height + ui.spacing().item_spacing.y;
                            if let Some(new_offset) = scroll_offset_for_selected_row(
                                old_offset, ui.available_height(),
                                position / grid_columns, row_stride, row_height,
                            ) {
                                grid_scroll = grid_scroll.vertical_scroll_offset(new_offset);
                            }
                        }
                    } else if library_page_changed {
                        grid_scroll = grid_scroll.vertical_scroll_offset(0.0);
                    }
                    let _grid_output = grid_scroll.show_rows(ui, row_height, total_rows, |ui, row_range| {
                            #[cfg(test)]
                            ctx.data_mut(|data| {
                                data.insert_temp(
                                    egui::Id::new("nsl_test_visible_grid_rows"), row_range.clone(),
                                );
                                data.insert_temp(egui::Id::new("nsl_test_grid_columns"), grid_columns);
                            });
                            egui::Frame::none()
                                .fill(ui.visuals().faint_bg_color)
                                .inner_margin(egui::Margin::ZERO)
                                .show(ui, |ui| {
                                    let mut icon_budget = ICON_LOAD_BUDGET;
                                    for row_index in row_range {
                                        let start = row_index * grid_columns;
                                        let end = (start + grid_columns).min(page_items.len());
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 0.0;
                                            for row in page_items.iter().skip(start).take(end - start) {
                                                self.ensure_icon_texture(
                                                    ctx,
                                                    &row.title_id,
                                                    &mut icon_budget,
                                                );
                                                let selected = self.selected == Some(row.id);
                                                let card_id = ui.make_persistent_id(("grid-card", row.id));
                                                let card = selectable_card_background(
                                                    ui,
                                                    card_id,
                                                    grid_card_width,
                                                    row_height,
                                                );
                                                let mut icon_response = None;
                                                let mut title_response = None;
                                                let _card_response = egui::Frame::none()
                                                    .fill(if selected {
                                                        ui.visuals().selection.bg_fill
                                                    } else {
                                                        egui::Color32::TRANSPARENT
                                                    })
                                                    .stroke(egui::Stroke::NONE)
                                                    .inner_margin(egui::Margin::same(6.0))
                                                    .show(ui, |ui| {
                                                        ui.set_width(grid_card_width - 12.0);
                                                        ui.set_height(grid_thumbnail_size + 38.0);
                                                        ui.vertical_centered(|ui| {
                                                            if let Some(icon) = self
                                                                .icon_textures
                                                                .get(&row.title_id)
                                                            {
                                                                icon_response = Some(ui.add(
                                                                    egui::Image::new((
                                                                        icon.id(),
                                                                        egui::vec2(
                                                                            grid_thumbnail_size,
                                                                            grid_thumbnail_size,
                                                                        ),
                                                                    ))
                                                                    .sense(egui::Sense::click()),
                                                                ));
                                                            } else {
                                                                let (rect, response) = ui.allocate_exact_size(
                                                                    egui::vec2(
                                                                        grid_thumbnail_size,
                                                                        grid_thumbnail_size,
                                                                    ),
                                                                    egui::Sense::click(),
                                                                );
                                                                ui.painter().text(
                                                                    rect.center(),
                                                                    egui::Align2::CENTER_CENTER,
                                                                    "🎮",
                                                                    egui::FontId::proportional(14.0),
                                                                    ui.visuals().text_color(),
                                                                );
                                                                icon_response = Some(response);
                                                            }
                                                            title_response = Some(ui.add(
                                                                egui::Label::new(&row.title_name)
                                                                    .truncate()
                                                                    .sense(egui::Sense::click()),
                                                            ));
                                                        });
                                                    });
                                                if matches!(self.folder_filter.as_str(), "MISSING_UPDATES" | "MISSING_DLCS")
                                                    && self.update_batch.as_ref().is_some_and(|batch| {
                                                        (batch.kind == if self.folder_filter == "MISSING_DLCS" { update_batch::BatchKind::Dlc } else { update_batch::BatchKind::Update })
                                                            && batch.has_match_for_game(row.id)
                                                    })
                                                {
                                                    if let Some(icon) = &icon_response {
                                                        let indicator = egui::Rect::from_min_size(
                                                            icon.rect.right_top() - egui::vec2(19.0, 0.0),
                                                            egui::vec2(19.0, 19.0),
                                                        );
                                                        ui.painter().rect_filled(indicator, 4.0, ui.visuals().hyperlink_color);
                                                        ui.painter().text(
                                                            indicator.center(),
                                                            egui::Align2::CENTER_CENTER,
                                                            if self.folder_filter == "MISSING_DLCS" { "+" } else { "↑" },
                                                            egui::FontId::proportional(14.0),
                                                            egui::Color32::WHITE,
                                                        );
                                                    }
                                                }
                                                // One click target for the complete grid card,
                                                // including the blank icon/label padding.
                                                remember_card_geometry(
                                                    ctx,
                                                    card_id,
                                                    card.rect,
                                                    _card_response.response.rect,
                                                );
                                                // Child image/label widgets have their own
                                                // hit regions. Merge them with the card so
                                                // clicking artwork or title selects exactly as
                                                // clicking the surrounding padding does.
                                                let mut response = card;
                                                if let Some(icon) = icon_response {
                                                    response |= icon;
                                                }
                                                if let Some(title) = title_response {
                                                    response |= title;
                                                }
                                                #[cfg(test)]
                                                let hit_rect = response.rect;
                                                if response.clicked() {
                                                    self.selected = Some(row.id);
                                                    ctx.memory_mut(|memory| {
                                                        memory.request_focus(library_focus_id)
                                                    });
                                                    ctx.request_repaint();
                                                }
                                                if response.double_clicked() {
                                                    self.run_file(row);
                                                }
                                                response
                                                    .on_hover_text(format!(
                                                        "{}\n{}\n{}",
                                                        row.title_id,
                                                        common::format_size(
                                                            row.size.max(0) as u64
                                                        ),
                                                        row.path
                                                    ))
                                                    .context_menu(|ui| {
                                                        self.row_context_menu(ui, row)
                                                    });
                                                #[cfg(test)]
                                                if row.id == page_items[0].id {
                                                    ctx.data_mut(|data| {
                                                        data.insert_temp(
                                                            egui::Id::new("nsl_test_grid_first_card"),
                                                            _card_response.response.rect,
                                                        );
                                                        data.insert_temp(
                                                            egui::Id::new("nsl_test_grid_first_card_hit_rect"),
                                                            hit_rect,
                                                        );
                                                    });
                                                }
                                                #[cfg(test)]
                                                if page_items.get(1).is_some_and(|second| second.id == row.id) {
                                                    ctx.data_mut(|data| {
                                                        data.insert_temp(
                                                            egui::Id::new("nsl_test_grid_second_card"),
                                                            _card_response.response.rect,
                                                        );
                                                    });
                                                }
                                                #[cfg(test)]
                                                if selected {
                                                    ctx.data_mut(|data| data.insert_temp(
                                                        egui::Id::new("nsl_test_selected_grid_rect"),
                                                        _card_response.response.rect,
                                                    ));
                                                }
                                            }
                                        });
                                    }
                                });
                        });
                    #[cfg(test)]
                    ctx.data_mut(|data| data.insert_temp(
                        egui::Id::new("nsl_test_grid_viewport_rect"), _grid_output.inner_rect,
                    ));
                } else {
                    let list_width = ui.available_width();
                    let name_width = (list_width - 480.0).max(240.0);
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        ui.set_min_width(list_width);
                        egui::Grid::new("library_header")
                            .striped(true)
                            .spacing(egui::vec2(0.0, 0.0))
                            .show(ui, |ui| {
                                for (label, key, width) in [
                                    ("게임 이름", "name", name_width),
                                    ("타이틀 ID", "id", 120.0),
                                    ("보유 버전", "version", 80.0),
                                    ("최신 버전", "latest_ver", 80.0),
                                    ("크기", "size", 60.0),
                                    ("상태", "status", 140.0),
                                ] {
                                    let marker = if self.sort_column == key {
                                        if self.sort_desc {
                                            " ▼"
                                        } else {
                                            " ▲"
                                        }
                                    } else {
                                        ""
                                    };
                                    let clicked = if key == "name" {
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(width, 26.0),
                                            egui::Sense::click(),
                                        );
                                        let visuals = ui.style().interact(&response);
                                        ui.painter().rect(
                                            rect.expand(visuals.expansion),
                                            visuals.rounding,
                                            visuals.weak_bg_fill,
                                            visuals.bg_stroke,
                                        );
                                        ui.painter()
                                            .with_clip_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
                                            .text(
                                                rect.left_center() + egui::vec2(6.0, 0.0),
                                                egui::Align2::LEFT_CENTER,
                                                format!("{label}{marker}"),
                                                egui::TextStyle::Button.resolve(ui.style()),
                                                visuals.text_color(),
                                            );
                                        response.clicked()
                                    } else {
                                        ui.add_sized(
                                            [width, 26.0],
                                            egui::Button::new(format!("{label}{marker}")),
                                        )
                                        .clicked()
                                    };
                                    if clicked {
                                        if self.sort_column == key {
                                            self.sort_desc = !self.sort_desc;
                                        } else {
                                            self.sort_column = key.into();
                                            self.sort_desc = false;
                                        }
                                        self.current_page = 0;
                                    }
                                }
                                ui.end_row();
                            });
                        // Only create table widgets for visible rows. A disabled
                        // paging setting must not render thousands at once.
                        let mut list_scroll = egui::ScrollArea::vertical()
                            .id_salt("library_list_rows")
                            .animated(false)
                            .auto_shrink([false, false]);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if keyboard_scroll_to_selection {
                            if let Some(position) = page_items.iter().position(|row| self.selected == Some(row.id)) {
                                let id = ui.make_persistent_id(egui::Id::new("library_list_rows"));
                                let old_offset = egui::scroll_area::State::load(ctx, id)
                                    .map(|state| state.offset.y).unwrap_or(0.0);
                                if let Some(new_offset) = scroll_offset_for_selected_row(
                                    old_offset, ui.available_height(), position, 24.0, 24.0,
                                ) {
                                    list_scroll = list_scroll.vertical_scroll_offset(new_offset);
                                }
                            }
                        } else if library_page_changed {
                            list_scroll = list_scroll.vertical_scroll_offset(0.0);
                        }
                        let _list_output = list_scroll.show_rows(ui, 24.0, page_items.len(), |ui, row_range| {
                            #[cfg(test)]
                            ctx.data_mut(|data| data.insert_temp(
                                egui::Id::new("nsl_test_visible_list_rows"), row_range.clone(),
                            ));
                            egui::Grid::new("library_rows")
                                .striped(true)
                                .spacing(egui::vec2(0.0, 0.0))
                                .show(ui, |ui| {
                                for row in &page_items[row_range] {
                                    let selected = self.selected == Some(row.id);
                                    let update_audited = matches!(self.folder_filter.as_str(), "MISSING_UPDATES" | "MISSING_DLCS")
                                        && self.update_batch.as_ref().is_some_and(|batch| {
                                            (batch.kind == if self.folder_filter == "MISSING_DLCS" { update_batch::BatchKind::Dlc } else { update_batch::BatchKind::Update })
                                                && batch.has_match_for_game(row.id)
                                        });
                                    let display_name: std::borrow::Cow<'_, str> = if update_audited {
                                        std::borrow::Cow::Owned(format!("{}  {}", if self.folder_filter == "MISSING_DLCS" { "+" } else { "↑" }, row.title_name))
                                    } else {
                                        std::borrow::Cow::Borrowed(row.title_name.as_str())
                                    };
                                    let response = selectable_table_text_cell(
                                        ui,
                                        name_width,
                                        24.0,
                                        &display_name,
                                        selected,
                                        egui::Align::Min,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    #[cfg(test)]
                                    if selected {
                                        ctx.data_mut(|data| data.insert_temp(
                                            egui::Id::new("nsl_test_selected_list_rect"), response.rect,
                                        ));
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let response = selectable_table_text_cell(
                                        ui,
                                        120.0,
                                        24.0,
                                        &row.title_id,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let installed_version = if row.update_version != 0 {
                                        row.update_version
                                    } else {
                                        row.version
                                    };
                                    let installed_text =
                                        common::version_to_str(Some(installed_version));
                                    let response = selectable_table_text_cell(
                                        ui,
                                        80.0,
                                        24.0,
                                        &installed_text,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let latest_text = common::version_to_str(
                                        (row.latest_version > 0).then_some(row.latest_version),
                                    );
                                    let response = selectable_table_text_cell(
                                        ui,
                                        80.0,
                                        24.0,
                                        &latest_text,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let size_text = common::format_size(row.size.max(0) as u64);
                                    let response = selectable_table_text_cell(
                                        ui,
                                        60.0,
                                        24.0,
                                        &size_text,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let dark_mode = ui.visuals().dark_mode;
                                    let unknown_text_color = if dark_mode {
                                        ui.visuals().weak_text_color()
                                    } else {
                                        egui::Color32::from_rgb(110, 119, 129)
                                    };
                                    let update_text_color = if dark_mode {
                                        egui::Color32::from_rgb(244, 174, 94)
                                    } else {
                                        egui::Color32::from_rgb(154, 103, 0)
                                    };
                                    let latest_text_color = if dark_mode {
                                        egui::Color32::from_rgb(100, 208, 139)
                                    } else {
                                        egui::Color32::from_rgb(26, 127, 55)
                                    };
                                    let (status_label, icon_color, text_color, icon) =
                                        if row.latest_version == 0 {
                                            (
                                                "미확인",
                                                egui::Color32::from_rgb(115, 125, 137),
                                                unknown_text_color,
                                                0,
                                            )
                                        } else if installed_version < row.latest_version {
                                            (
                                                "업데이트 가능",
                                                egui::Color32::from_rgb(216, 137, 49),
                                                update_text_color,
                                                1,
                                            )
                                        } else {
                                            (
                                                "최신",
                                                egui::Color32::from_rgb(43, 151, 95),
                                                latest_text_color,
                                                2,
                                            )
                                        };
                                    let (status_rect, status_response) = ui.allocate_exact_size(
                                        egui::vec2(140.0, 24.0),
                                        egui::Sense::click(),
                                    );
                                    paint_selectable_cell(
                                        ui,
                                        status_rect,
                                        &status_response,
                                        selected,
                                    );
                                    let painter = ui.painter();
                                    let icon_center =
                                        status_rect.left_center() + egui::vec2(11.0, 0.0);
                                    painter.circle_filled(icon_center, 7.0, icon_color);
                                    match icon {
                                        1 => {
                                            let stroke =
                                                egui::Stroke::new(1.7_f32, egui::Color32::WHITE);
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(0.0, 3.5),
                                                    icon_center + egui::vec2(0.0, -3.0),
                                                ],
                                                stroke,
                                            );
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(-3.0, 0.0),
                                                    icon_center + egui::vec2(0.0, -3.0),
                                                ],
                                                stroke,
                                            );
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(0.0, -3.0),
                                                    icon_center + egui::vec2(3.0, 0.0),
                                                ],
                                                stroke,
                                            );
                                        }
                                        2 => {
                                            let stroke =
                                                egui::Stroke::new(1.7_f32, egui::Color32::WHITE);
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(-3.0, 0.0),
                                                    icon_center + egui::vec2(-1.0, 2.0),
                                                ],
                                                stroke,
                                            );
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(-1.0, 2.0),
                                                    icon_center + egui::vec2(3.0, -2.5),
                                                ],
                                                stroke,
                                            );
                                        }
                                        _ => {
                                            painter.text(
                                                icon_center,
                                                egui::Align2::CENTER_CENTER,
                                                "?",
                                                egui::FontId::proportional(10.0),
                                                egui::Color32::WHITE,
                                            );
                                        }
                                    };
                                    painter.text(
                                        status_rect.left_center() + egui::vec2(23.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        status_label,
                                        egui::TextStyle::Button.resolve(ui.style()),
                                        text_color,
                                    );
                                    if status_response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if status_response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    status_response
                                        .context_menu(|ui| self.row_context_menu(ui, row));
                                    ui.end_row();
                                }
                            });
                        });
                        #[cfg(test)]
                        ctx.data_mut(|data| data.insert_temp(
                            egui::Id::new("nsl_test_list_viewport_rect"), _list_output.inner_rect,
                        ));
                    });
                }
                let duplicate_groups = self.cached_duplicates.clone();
                if !duplicate_groups.is_empty() {
                    ui.collapsing(
                        format!("중복 베이스 게임 {}개", duplicate_groups.len()),
                        |ui| {
                            for (title_id, name, paths) in &duplicate_groups {
                                ui.collapsing(
                                    format!("{name} [{title_id}] · {}개 파일", paths.len()),
                                    |ui| {
                                        for path in paths {
                                            ui.label(path);
                                        }
                                    },
                                );
                            }
                        },
                    );
                }
                if let Some(id) = self.selected {
                    if let Some(row) = self.rows.iter().find(|r| r.id == id).cloned() {
                        egui::CollapsingHeader::new("인라인 상세 정보")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.separator();
                                if let Some(icon) = self.icon_textures.get(&row.title_id) {
                                    ui.image((icon.id(), egui::vec2(120.0, 120.0)));
                                }
                                ui.heading(&row.title_name);
                                ui.label(format!(
                                    "{}  ·  {}  ·  {}",
                                    row.title_id,
                                    common::version_to_str(Some(row.version)),
                                    row.region
                                ));
                                ui.label(&row.path);
                                ui.horizontal(|ui| {
                                    if ui.button("실행").clicked() {
                                        self.run_file(&row);
                                    }
                                    if ui.button("파일명 정규화").clicked() {
                                        match self.rename_target(&row) {
                                            Ok(target) => {
                                                self.rename_preview = Some((
                                                    row.path.clone(),
                                                    target.to_string_lossy().into_owned(),
                                                ))
                                            }
                                            Err(e) => self.status = e.to_string(),
                                        }
                                    }
                                    if ui.button("파일 위치 열기").clicked() {
                                        let _ = std::process::Command::new("explorer")
                                            .arg("/select,")
                                            .arg(&row.path)
                                            .spawn();
                                    }
                                    if ui
                                        .add_enabled(
                                            self.media_result.is_none(),
                                            egui::Button::new("이 게임 스크린샷 다운로드"),
                                        )
                                        .clicked()
                                    {
                                        self.start_media_collection(MediaSelection::Screenshots, Some(&row));
                                    }
                                    if row.has_base
                                        && ui
                                            .add_enabled(
                                                self.media_result.is_none(),
                                                egui::Button::new("이 게임 아이콘 가져오기"),
                                            )
                                            .clicked()
                                    {
                                        self.start_media_collection(MediaSelection::Icons, Some(&row));
                                    }
                                    if ["nsz", "xcz"]
                                        .contains(&row.file_type.to_ascii_lowercase().as_str())
                                        && ui
                                            .add_enabled(
                                                self.decompress_result.is_none(),
                                                egui::Button::new("압축 해제"),
                                            )
                                            .clicked()
                                    {
                                        self.start_decompress(PathBuf::from(&row.path));
                                    }
                                    if let Some(converted) = &self.converted_file {
                                        if ui.button("변환 결과 폴더 열기").clicked() {
                                            let _ = std::process::Command::new("explorer")
                                                .arg("/select,")
                                                .arg(converted)
                                                .spawn();
                                        }
                                    }
                                    if ui.button("스크린샷 폴더 열기").clicked() {
                                        let _ = std::process::Command::new("explorer")
                                            .arg(self.screenshot_dir.join(&row.title_id))
                                            .spawn();
                                    }
                                });
                                if row.has_update {
                                    ui.label(format!(
                                        "업데이트 포함: {}",
                                        common::version_to_str(Some(row.update_version))
                                    ));
                                }
                                if row.latest_version > 0 {
                                    ui.label(format!(
                                        "DB 최신 버전: {}",
                                        common::version_to_str(Some(row.latest_version))
                                    ));
                                }
                                if !row.nacp_ko.is_empty() {
                                    ui.label(format!("한국어: {}", row.nacp_ko));
                                }
                                if !row.nacp_en.is_empty() {
                                    ui.label(format!("영어: {}", row.nacp_en));
                                }
                                if !row.nacp_ja.is_empty() {
                                    ui.label(format!("일본어: {}", row.nacp_ja));
                                }
                                if !row.languages.is_empty() {
                                    ui.label(format!("언어: {}", row.languages));
                                }
                                if row.db_dlc_count > 0 {
                                    ui.label(format!(
                                        "보유 DLC: {} / {}",
                                        row.dlc_count, row.db_dlc_count
                                    ));
                                }
                                if !row.owned_dlcs.is_empty() {
                                    ui.collapsing(
                                        format!("보유 DLC 목록 ({})", row.owned_dlcs.len()),
                                        |ui| {
                                            for (id, name) in &row.owned_dlcs {
                                                ui.label(format!("{name} [{id}]"));
                                            }
                                        },
                                    );
                                }
                                if !row.missing_dlcs.is_empty() {
                                    ui.collapsing(
                                        format!("미보유 DLC 목록 ({})", row.missing_dlcs.len()),
                                        |ui| {
                                            for (id, name) in &row.missing_dlcs {
                                                ui.label(format!("{name} [{id}]"));
                                            }
                                        },
                                    );
                                }
                                if let Ok(urls) =
                                    serde_json::from_str::<Vec<String>>(&row.screenshots)
                                {
                                    if let Some(url) = urls.first() {
                                        ui.hyperlink_to("스크린샷 열기", url);
                                    }
                                }
                                if !selected_screenshot_paths.is_empty() {
                                    for path in &selected_screenshot_paths {
                                        let key = path.to_string_lossy().into_owned();
                                        if !self.screenshot_textures.contains_key(&key)
                                            && !self.screenshot_missing.contains(&key)
                                        {
                                            self.image_loader.request(
                                                ImageKind::Screenshot,
                                                &key,
                                                path.clone(),
                                                900,
                                                self.selected,
                                            );
                                        }
                                    }
                                    if self.image_loader.has_pending() {
                                        ctx.request_repaint_after(std::time::Duration::from_millis(45));
                                    }
                                    ui.label("저장된 스크린샷");
                                    ui.horizontal_wrapped(|ui| {
                                        for path in &selected_screenshot_paths {
                                            let key = path.to_string_lossy().into_owned();
                                            if let Some(tex) = self.screenshot_textures.get(&key) {
                                                let texture_size = tex.size_vec2();
                                                let scale = (180.0 / texture_size.x.max(1.0))
                                                    .min(120.0 / texture_size.y.max(1.0));
                                                let response = ui.add(
                                                    egui::Image::new((
                                                        tex.id(),
                                                        texture_size * scale,
                                                    ))
                                                    .sense(egui::Sense::click()),
                                                );
                                                if response.clicked() {
                                                    let _ = std::process::Command::new("explorer")
                                                        .arg(&key)
                                                        .spawn();
                                                }
                                            }
                                        }
                                    });
                                }
                                if !row.description.is_empty() {
                                    ui.label(&row.description);
                                    if ui
                                        .add_enabled(
                                            self.translate_result.is_none(),
                                            egui::Button::new("설명 한국어 번역"),
                                        )
                                        .clicked()
                                    {
                                        self.start_translation(&row);
                                    }
                                }
                                if let Some((id, translated)) = &self.translated_description {
                                    if *id == row.id {
                                        ui.collapsing("한국어 번역", |ui| {
                                            ui.label(translated);
                                        });
                                    }
                                }
                            });
                    }
                }
            }
        });
        if self.config.settings != settings_before || self.folders != folders_before {
            self.config.scan_folders = self.folders.clone();
            if let Err(e) = database::save_config(&self.base_dir.join("config.json"), &self.config)
            {
                self.status = format!("설정 저장 실패: {e}");
            }
            let name_mode_after = self
                .config
                .settings
                .get("name_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("db");
            if name_mode_after != name_mode_before || self.folders != folders_before {
                self.start_reload();
            }
        }
        let main_style = ctx.style();
        let mut popup_style = (*main_style).clone();
        popup_style.spacing.window_margin = egui::Margin::symmetric(16.0, 12.0);
        popup_style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        popup_style.spacing.button_padding = egui::vec2(10.0, 6.0);
        popup_style.visuals.window_shadow = egui::Shadow::NONE;
        popup_style.visuals.popup_shadow = egui::Shadow::NONE;
        popup_style.wrap_mode = Some(egui::TextWrapMode::Wrap);
        ctx.set_style(popup_style);
        if let Some((from, to)) = self.rename_preview.clone() {
            show_centered_popup(
                ctx,
                "rename_confirm",
                "파일명 변경 확인",
                None,
                |window| window,
                |ui| {
                    ui.label(
                        std::path::Path::new(&from)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                    );
                    ui.label("→");
                    ui.label(
                        std::path::Path::new(&to)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                    );
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("변경").clicked() {
                                self.rename_preview = None;
                                if PathBuf::from(&to).exists() {
                                    self.status = "같은 이름의 파일이 이미 있습니다".into()
                                } else {
                                    match std::fs::rename(&from, &to) {
                                        Ok(()) => match self.db.rename_file_path(&from, &to) {
                                            Ok(()) => {
                                                self.status = "파일명을 정규화했습니다".into();
                                                self.start_reload();
                                            }
                                            Err(e) => {
                                                let _ = std::fs::rename(&to, &from);
                                                self.status = e.to_string()
                                            }
                                        },
                                        Err(e) => self.status = e.to_string(),
                                    }
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.rename_preview = None;
                            }
                        });
                    });
                },
            );
        }
        if let Some(mut dialog) = self.catalog_torrent_dialog.take() {
            if let Some(request) = &dialog.request {
                match request.receiver.try_recv() {
                    Ok(Ok(preview)) => {
                        dialog.request = None;
                        match torrent_file_picker::TorrentFilePicker::new(preview) {
                            Ok(files) => {
                                self.status = format!(
                                    "토렌트 파일 목록 준비 완료 · {}개",
                                    files.total_files()
                                );
                                dialog.files = Some(files);
                            }
                            Err(error) => {
                                self.status = format!("토렌트 파일 목록 조회 실패: {error}");
                                dialog.error = Some(error.to_string());
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        dialog.request = None;
                        self.status = format!("토렌트 파일 목록 조회 실패: {error}");
                        dialog.error = Some(error);
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        dialog.request = None;
                        let error = "토렌트 파일 목록을 가져올 수 없습니다";
                        self.status = format!("토렌트 파일 목록 조회 실패: {error}");
                        dialog.error = Some(error.into());
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(90));
                    }
                }
                if dialog.request.is_none() {
                    // The status bar was drawn before this modal, so force a
                    // fresh frame that removes the pending indicator promptly.
                    ctx.request_repaint();
                }
            }
            let mut canceled = false;
            let mut confirmed = false;
            let mut retry_requested = false;
            let response = show_centered_popup(
                ctx,
                "store_select_torrent_files",
                "다운로드 파일 선택",
                None,
                |window| window.resizable(true).min_width(620.0).max_width(880.0),
                |ui| {
                    // Wider than a generic confirmation dialog: the tree
                    // needs room for 30px nested indents and a size column.
                    ui.set_min_width(620.0);
                    ui.label(
                        egui::RichText::new(truncate_label(&dialog.item.title, 85))
                            .strong()
                            .size(15.0),
                    );
                    ui.label(
                        egui::RichText::new(
                            "다운로드할 파일과 하위 폴더를 확인하세요. 기본값은 전체 선택입니다.",
                        )
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(4.0);
                    if let Some(files) = &mut dialog.files {
                        files.ui(ui);
                        ui.label(
                            egui::RichText::new(
                                "토렌트 조각 무결성과 재개를 위해 미선택 파일에도 일부 경계 조각이 저장될 수 있습니다.",
                            )
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        );
                    } else if let Some(error) = &dialog.error {
                        ui.colored_label(
                            egui::Color32::LIGHT_RED,
                            format!("파일 목록 조회 실패: {error}"),
                        );
                        ui.label("피어 연결·트래커 응답 지연일 수 있습니다. 다시 조회해 보세요.");
                        let retry = ui.button("파일 목록 다시 조회");
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("nsl_test_torrent_preview_retry_button"),
                                retry.rect,
                            );
                        });
                        if retry.clicked() {
                            retry_requested = true;
                        }
                    } else {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("토렌트 메타데이터를 받는 중… 파일 다운로드는 아직 시작되지 않았습니다.");
                        });
                        ui.label(
                            "연결할 피어가 없으면 파일 목록을 가져오는 데 시간이 걸릴 수 있습니다.",
                        );
                    }
                    popup_footer(ui, |ui| {
                        let confirm = ui.add_enabled(
                            dialog
                                .files
                                .as_ref()
                                .is_some_and(|files| files.selected_count() > 0),
                            egui::Button::new("선택한 파일 다운로드"),
                        );
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("nsl_test_torrent_confirm_button"),
                                confirm.rect,
                            );
                        });
                        if confirm.clicked() {
                            confirmed = true;
                        }
                        let cancel = ui.button("취소");
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("nsl_test_torrent_cancel_button"),
                                cancel.rect,
                            );
                        });
                        if cancel.clicked() {
                            canceled = true;
                        }
                    });
                },
            );
            #[cfg(test)]
            if let Some(response) = response {
                ctx.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_torrent_file_selection_popup"),
                        response.response.rect,
                    );
                });
            }
            #[cfg(not(test))]
            let _ = response;
            if retry_requested && !confirmed && !canceled {
                match self.torrent_engine.as_ref() {
                    Some(engine) => match engine.preview_files(&dialog.item.magnet) {
                        Ok(request) => {
                            dialog.request = Some(request);
                            dialog.error = None;
                            dialog.files = None;
                            self.status = "토렌트 파일 목록 다시 조회 중".into();
                            ctx.request_repaint();
                        }
                        Err(error) => {
                            dialog.error = Some(error.to_string());
                            self.status = format!("토렌트 파일 목록 재조회 실패: {error}");
                        }
                    },
                    None => {
                        dialog.error = Some("토렌트 엔진을 사용할 수 없습니다".into());
                        self.status = "토렌트 엔진을 사용할 수 없습니다".into();
                    }
                }
            }
            if confirmed {
                if let Some(files) = dialog.files.take() {
                    self.start_selected_catalog_torrent(&dialog.item, files);
                }
            } else if !canceled {
                self.catalog_torrent_dialog = Some(dialog);
            } else {
                self.status = "토렌트 파일 선택 취소됨".into();
                ctx.request_repaint();
            }
        }
        if let Some((key, name)) = self.download_delete_confirm.clone() {
            let prefer_delete_files = self
                .config
                .settings
                .get("download_delete_files")
                .and_then(|value| value.as_str())
                == Some("1");
            let mut choice: Option<Option<bool>> = None;
            show_centered_popup(
                ctx,
                "download_delete_confirm",
                "다운로드 삭제 확인",
                None,
                |window| window,
                |ui| {
                    ui.label(format!("「{}」", truncate_label(&name, 60)));
                    ui.label(
                        "목록만 삭제하면 받은 파일은 그대로 유지됩니다.\n파일까지 삭제하면 내려받은 파일이 영구 삭제됩니다.",
                    );
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            // The setting only changes button order; it never
                            // deletes any files without an explicit choice.
                            let options = if prefer_delete_files {
                                [true, false]
                            } else {
                                [false, true]
                            };
                            for delete_files in options {
                                let caption = if delete_files {
                                    "파일까지 삭제"
                                } else {
                                    "목록만 삭제 (파일 유지)"
                                };
                                let button = ui.button(caption);
                                #[cfg(test)]
                                ui.ctx().data_mut(|data| {
                                    data.insert_temp(
                                        egui::Id::new((
                                            "nsl_test_download_delete_option",
                                            delete_files,
                                        )),
                                        button.rect,
                                    );
                                });
                                if button.clicked() {
                                    choice = Some(Some(delete_files));
                                }
                            }
                            let cancel = ui.button("취소");
                            #[cfg(test)]
                            ui.ctx().data_mut(|data| {
                                data.insert_temp(
                                    egui::Id::new("nsl_test_download_delete_cancel"),
                                    cancel.rect,
                                );
                            });
                            if cancel.clicked() {
                                choice = Some(None);
                            }
                        });
                    });
                },
            );
            if let Some(confirmed) = choice {
                self.download_delete_confirm = None;
                if let Some(delete_files) = confirmed {
                    if let Some(engine) = &self.torrent_engine {
                        if delete_files {
                            engine.cancel(&key, true);
                        } else {
                            engine.dismiss(&key);
                        }
                        self.status = if delete_files {
                            "다운로드 및 파일 삭제 요청됨"
                        } else {
                            "다운로드 목록 삭제 요청됨 · 받은 파일은 유지됩니다"
                        }
                        .into();
                        let _ = self.log_sender.send(format!(
                            "🗑 다운로드 {} · {}",
                            if delete_files {
                                "및 파일 삭제"
                            } else {
                                "목록 삭제 (파일 유지)"
                            },
                            truncate_label(&name, 60)
                        ));
                    } else {
                        self.status = "토렌트 엔진을 사용할 수 없습니다".into();
                    }
                }
            }
        }
        if let Some(changes) = self.bulk_rename_preview.clone() {
            show_centered_popup(
                ctx,
                "bulk_rename_confirm",
                "파일명 일괄 정규화 확인",
                None,
                |window| window,
                |ui| {
                    ui.label(format!(
                        "검색 및 종류 필터에 맞는 파일 {}개의 이름을 바꿉니다.",
                        changes.len()
                    ));
                    ui.label("새 파일이 이미 있거나 이름 변경에 실패하면 해당 파일은 건너뜁니다.");
                    for (from, to) in changes.iter().take(3) {
                        ui.label(format!(
                            "{} → {}",
                            PathBuf::from(from)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                            PathBuf::from(to)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        ));
                    }
                    if changes.len() > 3 {
                        ui.label(format!("외 {}개", changes.len() - 3));
                    }
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("이름 변경").clicked() {
                                self.bulk_rename_preview = None;
                                self.start_bulk_rename(changes);
                            }
                            if ui.button("취소").clicked() {
                                self.bulk_rename_preview = None;
                            }
                        });
                    });
                },
            );
        }
        if let Some(info) = self.titledb_info.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "titledb_info",
                "타이틀 DB 파일 현황",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(info);
                    popup_footer(ui, |ui| {
                        if ui.button("닫기").clicked() {
                            self.titledb_info = None;
                        }
                    });
                },
            );
            if !open {
                self.titledb_info = None;
            }
        }
        if self.clear_titledb_confirm {
            let mut open = self.clear_titledb_confirm;
            show_centered_popup(
                ctx,
                "clear_titledb_confirm",
                "타이틀 DB 초기화 확인",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(
                        "데이터베이스의 타이틀 정보를 비웁니다. 다운로드한 JSON 파일은 유지됩니다.",
                    );
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(active_tasks.is_empty(), egui::Button::new("초기화"))
                                .clicked()
                            {
                                self.clear_titledb_confirm = false;
                                match self.db.clear_titles() {
                                    Ok(()) => {
                                        self.status = "타이틀 DB를 초기화했습니다".into();
                                        self.start_reload();
                                    }
                                    Err(e) => self.status = e.to_string(),
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.clear_titledb_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.clear_titledb_confirm = false;
            }
        }
        if self.clear_library_confirm {
            let mut open = self.clear_library_confirm;
            show_centered_popup(
                ctx,
                "clear_library_confirm",
                "라이브러리 초기화 확인",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("스캔한 게임 목록과 타이틀 DB 정보를 지웁니다. 게임 파일과 아이콘/스크린샷 파일은 유지됩니다.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("초기화").clicked() {
                                self.clear_library_confirm = false;
                                match self.db.clear_library_data() {
                                    Ok(()) => {
                                        self.rows.clear();
                                        self.selected = None;
                                        self.status = "라이브러리를 초기화했습니다".into();
                                    }
                                    Err(e) => self.status = e.to_string(),
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.clear_library_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.clear_library_confirm = false;
            }
        }
        if let Some(folder) = self.remove_folder_confirm.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "remove_folder_confirm",
                "스캔 폴더 삭제 확인",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(format!("이 폴더를 스캔 목록에서 제거할까요?\n{folder}"));
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("목록에서 제거").clicked() {
                                self.folders.retain(|path| path != &folder);
                                self.settings_folder_selected = None;
                                self.config.scan_folders = self.folders.clone();
                                if self.folder_filter == folder {
                                    self.folder_filter.clear();
                                    self.config.settings.insert(
                                        "last_tab_key".into(),
                                        serde_json::Value::String(String::new()),
                                    );
                                }
                                self.remove_folder_confirm = None;
                                match database::save_config(
                                    &self.base_dir.join("config.json"),
                                    &self.config,
                                ) {
                                    Ok(()) => self.status = format!("폴더 제거: {folder}"),
                                    Err(e) => self.status = e.to_string(),
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.remove_folder_confirm = None;
                            }
                        });
                    });
                },
            );
            if !open {
                self.remove_folder_confirm = None;
            }
        }
        if let Some(message) = self.folder_notice.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "folder_notice",
                "알림",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(message);
                    popup_footer(ui, |ui| {
                        if ui.button("확인").clicked() {
                            self.folder_notice = None;
                        }
                    });
                },
            );
            if !open {
                self.folder_notice = None;
            }
        }
        if self.orphan_cleanup_confirm {
            let mut open = self.orphan_cleanup_confirm;
            show_centered_popup(
                ctx,
                "orphan_cleanup_confirm",
                "사용하지 않는 미디어 캐시 정리",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("현재 라이브러리에 없는 타이틀 ID의 아이콘과 스크린샷 폴더를 삭제합니다. 게임 파일은 삭제하지 않습니다.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.orphan_cleanup_result.is_none(),
                                    egui::Button::new("미디어 캐시 정리"),
                                )
                                .clicked()
                            {
                                self.orphan_cleanup_confirm = false;
                                self.start_orphan_cleanup();
                            }
                            if ui.button("취소").clicked() {
                                self.orphan_cleanup_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.orphan_cleanup_confirm = false;
            }
        }
        if self.workspace_cleanup_confirm {
            let mut open = self.workspace_cleanup_confirm;
            show_centered_popup(
                ctx,
                "workspace_cleanup_confirm",
                "변환 작업폴더 정리",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("7일 이상 지난 NSL 변환 작업폴더만 삭제합니다. 최근 변환 파일과 게임 원본은 보존하지만, 오래된 결과물을 다른 프로그램에서 사용 중이라면 정리하지 마세요.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.workspace_cleanup_result.is_none()
                                        && self.decompress_result.is_none(),
                                    egui::Button::new("오래된 폴더 정리"),
                                )
                                .clicked()
                            {
                                self.workspace_cleanup_confirm = false;
                                self.start_workspace_cleanup();
                            }
                            if ui.button("취소").clicked() {
                                self.workspace_cleanup_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.workspace_cleanup_confirm = false;
            }
        }
        if let Some(info) = self.update_info.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "update_info",
                format!("업데이트 · 커밋 {}", info.latest_version),
                Some(&mut open),
                |window| window.max_width(720.0),
                |ui| {
                    ui.label(format!("현재 커밋: {}", info.current_version));
                    ui.label(format!("새 커밋: {}", info.latest_version));
                    ui.label(format!("릴리스 태그: {}", info.release_tag));
                    ui.label("공식 NSL 배포 패키지만 설치 후보로 사용합니다.");
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| ui.label(&info.notes));
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.update_prepare_result.is_none(),
                                    egui::Button::new("업데이트 다운로드 및 확인"),
                                )
                                .clicked()
                            {
                                self.update_info = None;
                                if let Some(url) = info.download_url.clone() {
                                    self.start_update_download(url, info.latest_commit.clone());
                                }
                            }
                            if ui.button("나중에").clicked() {
                                self.update_info = None;
                            }
                        });
                    });
                },
            );
            if !open {
                self.update_info = None;
            }
        }
        if let Some(task_dir) = self.update_ready.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "update_ready",
                "업데이트 준비 완료",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("검증된 업데이트를 적용하려면 앱을 종료하고 다시 시작해야 합니다.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    active_tasks.is_empty(),
                                    egui::Button::new("적용하고 다시 시작"),
                                )
                                .on_hover_text("실행 중인 다운로드·스캔·DB 작업이 모두 끝나야 업데이트를 적용할 수 있습니다")
                                .clicked()
                            {
                                self.update_ready = None;
                                if let Err(e) =
                                    updater::launch_apply_and_exit(&task_dir, &self.base_dir)
                                {
                                    self.status = format!("업데이트 적용 실패: {e}");
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.update_ready = None;
                            }
                        });
                    });
                },
            );
            if !open {
                self.update_ready = None;
            }
        }
        if let Some(info) = self.cache_info.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "cache_info",
                "캐시 현황",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.monospace(info);
                },
            );
            if !open {
                self.cache_info = None;
            }
        }
        if self.shortcuts_open {
            let mut open = self.shortcuts_open;
            let mut close = false;
            show_centered_popup(
                ctx,
                "shortcuts",
                "단축키",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(74, 158, 255),
                            egui::RichText::new("ℹ").size(30.0),
                        );
                        ui.vertical(|ui| {
                            ui.label("빠른 스캔 F5");
                            ui.label("설정 열기/닫기 Ctrl+O · 팝업/설정/로그 닫기 Esc");
                            ui.label("검색창 포커스 Ctrl+F");
                            ui.label("종료 Alt+F4");
                            ui.label("검색어 초기화 Esc (열린 팝업·설정·로그가 없을 때)");
                            ui.label("게임 선택 이동 ↑ ↓ ← →");
                            ui.label("한 화면 이동 Page Up / Page Down");
                            ui.label("처음 / 마지막 Home / End (Ctrl+Home / Ctrl+End)");
                            ui.label("파일 실행 Enter");
                            ui.label("컨텍스트 메뉴 우클릭");
                        });
                    });
                    popup_footer(ui, |ui| close = ui.button("확인").clicked());
                },
            );
            if close {
                open = false;
            }
            self.shortcuts_open = open;
        }
        if self.about_open {
            let mut open = self.about_open;
            let mut close = false;
            show_centered_popup(
                ctx,
                "about",
                "Nintendo Switch Library 정보",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(74, 158, 255),
                            egui::RichText::new("ℹ").size(34.0),
                        );
                        ui.vertical(|ui| {
                            ui.label(format!(
                                "Nintendo Switch Library · {}",
                                updater::build_commit_short()
                            ));
                            ui.label("닌텐도 스위치 게임 라이브러리 관리 툴");
                            ui.label("지원 형식: NSP, NSZ, XCI, XCZ");
                            ui.label(format!(
                                "DB: {}",
                                self.base_dir.join("library.db").display()
                            ));
                            ui.label(format!("캐시: {}", self.base_dir.join("caches").display()));
                        });
                    });
                    popup_footer(ui, |ui| close = ui.button("확인").clicked());
                },
            );
            if close {
                open = false;
            }
            self.about_open = open;
        }
        if self.exit_confirm {
            let mut open = self.exit_confirm;
            show_centered_popup(
                ctx,
                "exit_confirm",
                "진행 중인 작업이 있습니다",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("종료하면 다음 작업이 중단되며 다시 실행해야 할 수 있습니다.");
                    for task in &active_tasks {
                        ui.label(format!("• {task}"));
                    }
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("종료").clicked() {
                                self.exit_confirm = false;
                                self.exit_confirmed = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if ui.button("계속 진행").clicked() {
                                self.exit_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.exit_confirm = false;
            }
        }
        ctx.set_style(main_style);
    }
}
