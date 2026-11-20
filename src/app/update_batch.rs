//! Missing Updates: catalog audit, per-title markings, and a selective queue.
//! The original game files are never changed by discovery or downloads.

use crate::*;

// Streaming game merges can occupy tens of GiB of scratch disk space. Never
// start one worker per completed torrent: process just one merge at a time.
const MAX_PARALLEL_UPDATE_MERGES: usize = 1;

/// A completed torrent is rechecked from disk after restart. Never trust a
/// filename or a progress bar without scanning the actual package CNMT.
fn spawn_batch_merge(base_dir: PathBuf, row: &mut update_batch::UpdateBatchRow) {
    let Some(job) = row.job.as_mut() else {
        return;
    };
    if job.verifying.is_some() {
        return;
    }
    job.transfer_finished = true;
    let path = job.output.clone();
    let title_id = row.title_id.clone();
    let wanted = job.expected_version;
    let installed = row.installed_version;
    let original = row.original_path.clone();
    let original_size = job.original_size;
    let expected_file_size = job.expected_file_size;
    let (sender, receiver) = mpsc::channel();
    let control = Arc::new(update_merge::MergeControl::default());
    control.stage("다운로드 파일 무결성 검증");
    job.verifying = Some(receiver);
    job.merge_control = Some(Arc::clone(&control));
    job.status = update_batch::UpdateBatchJobStatus::Verifying;
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Option<PathBuf>> {
            control.ensure_running()?;
            anyhow::ensure!(original_size > 0, "원본 게임 파일 크기 정보가 없습니다");
            anyhow::ensure!(
                std::fs::metadata(&original)?.len() == original_size,
                "다운로드 대기 중 원본 게임 크기가 변경되어 병합을 중단했습니다"
            );
            anyhow::ensure!(
                expected_file_size > 0 && std::fs::metadata(&path)?.len() == expected_file_size,
                "다운로드 파일 크기가 검증된 토렌트 목록과 일치하지 않습니다"
            );
            update_batch::verify_downloaded_patch(&base_dir, &path, &title_id, wanted, installed)?;
            control.ensure_running()?;
            let merged = update_merge::merge_or_recover_existing_controlled(
                &original, &path, &title_id, wanted, &control,
            )?;
            Ok(Some(merged))
        })()
        .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
}

impl NslApp {
    pub(crate) fn retry_failed_batch_merges(&mut self) {
        let Some(batch) = self.update_batch.as_mut() else {
            return;
        };
        let mut queued = 0;
        for row in &mut batch.rows {
            let Some(job) = row.job.as_mut() else {
                continue;
            };
            if job.transfer_finished
                && matches!(job.status, update_batch::UpdateBatchJobStatus::Failed(_))
            {
                job.status = update_batch::UpdateBatchJobStatus::Queued;
                job.recovering_since = None;
                queued += 1;
            }
        }
        if queued > 0 {
            self.status =
                format!("다운로드 완료 항목 {queued}개를 재검증·병합 대기열에 등록했습니다");
            self.persist_batch_jobs();
        }
    }

    pub(crate) fn persist_batch_jobs(&mut self) {
        if let Some(batch) = &self.update_batch {
            if let Err(error) = update_manifest::save(&self.base_dir, batch) {
                self.status = format!("일괄 업데이트 재시작 복구 기록 저장 실패: {error}");
                let _ = self.log_sender.send(format!("⚠ {}", self.status));
            }
        }
    }

    pub(crate) fn start_missing_update_audit(&mut self) {
        if self.folder_filter != "MISSING_UPDATES" {
            return;
        }
        if self.catalog_state.items.is_empty() {
            self.status = "스토어에서 게임 카탈로그를 먼저 받아주세요".into();
            return;
        }
        if self.torrent_engine.is_none() {
            self.status =
                "토렌트 엔진을 사용할 수 없어 카탈로그 파일 목록을 검사할 수 없습니다".into();
            return;
        }
        if self.update_batch.as_ref().is_some_and(|batch| {
            batch.rows.iter().any(|row| {
                row.job.as_ref().is_some_and(|job| {
                    matches!(
                        job.status,
                        update_batch::UpdateBatchJobStatus::Queued
                            | update_batch::UpdateBatchJobStatus::Downloading
                            | update_batch::UpdateBatchJobStatus::Verifying
                    )
                })
            })
        }) {
            self.status =
                "기존 일괄 다운로드가 진행 중입니다. 작업이 완료된 후 다시 대조할 수 있습니다"
                    .into();
            return;
        }
        let excluded = self
            .config
            .settings
            .get("exception_update")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let batch = update_batch::new_update_batch(&self.rows, &self.catalog_state.items, excluded);
        let eligible = batch
            .rows
            .iter()
            .filter(|row| !row.candidates.is_empty())
            .count();
        self.status = format!(
            "업데이트 카탈로그 대조 시작: 게임 {}개, 토렌트 후보 {}개",
            batch.rows.len(),
            eligible
        );
        self.update_batch = Some(batch);
    }

    pub(crate) fn poll_missing_update_audit(&mut self, ctx: &egui::Context) {
        let Some(batch) = self.update_batch.as_mut() else {
            return;
        };
        let mut needs_scan = false;
        let mut changed = false;
        let mut verify_rows = Vec::new();
        if batch.running {
            batch.tick(&self.catalog_state.items, self.torrent_engine.as_deref());
            if !batch.running {
                self.status = format!(
                    "카탈로그 대조 완료: {}개 중 업데이트 파일 확인 {}개",
                    batch.rows.len(),
                    batch.eligible()
                );
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        for (index, row) in batch.rows.iter_mut().enumerate() {
            let Some(job) = row.job.as_mut() else {
                continue;
            };
            if matches!(
                job.status,
                update_batch::UpdateBatchJobStatus::Queued
                    | update_batch::UpdateBatchJobStatus::Downloading
            ) {
                if job.transfer_finished {
                    verify_rows.push(index);
                } else if let Some((key, progress)) =
                    self.torrent_progress.iter().find(|(key, progress)| {
                        key == &job.key
                            || progress.output_folder.as_deref() == Some(job.directory.as_path())
                    })
                {
                    if job.key != *key {
                        job.key = key.clone();
                        changed = true;
                    }
                    job.recovering_since = None;
                    if let Some(error) = &progress.error {
                        job.status = update_batch::UpdateBatchJobStatus::Failed(error.clone());
                        changed = true;
                    } else if progress.finished {
                        verify_rows.push(index);
                    } else if job.status != update_batch::UpdateBatchJobStatus::Downloading {
                        job.status = update_batch::UpdateBatchJobStatus::Downloading;
                        changed = true;
                    }
                } else if job
                    .recovering_since
                    .is_some_and(|started| started.elapsed() > std::time::Duration::from_secs(60))
                {
                    job.status = update_batch::UpdateBatchJobStatus::Failed(
                        "재시작 후 다운로드 세션을 복구할 수 없습니다. 다운로드 목록을 확인하세요"
                            .into(),
                    );
                    changed = true;
                }
            }
            if let Some(receiver) = &job.verifying {
                match receiver.try_recv() {
                    Ok(Ok(output)) => {
                        job.verifying = None;
                        job.merge_control = None;
                        changed = true;
                        if let Some(output) = output {
                            job.status = update_batch::UpdateBatchJobStatus::Merged(output);
                            self.status = format!("업데이트 병합 완료: {}", row.name);
                            let _ = self.log_sender.send(format!("✅ {}", self.status));
                            needs_scan = true;
                        } else {
                            job.status = update_batch::UpdateBatchJobStatus::Verified;
                            self.status =
                                format!("업데이트 검증 완료 · 압축 병합 대기: {}", row.name);
                        }
                    }
                    Ok(Err(error)) => {
                        job.verifying = None;
                        job.merge_control = None;
                        job.status = update_batch::UpdateBatchJobStatus::Failed(error);
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        job.verifying = None;
                        job.merge_control = None;
                        job.status = update_batch::UpdateBatchJobStatus::Failed(
                            "패키지 검증 작업이 예기치 않게 종료됨".into(),
                        );
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                }
            }
        }
        let active_merges = batch
            .rows
            .iter()
            .filter(|row| row.job.as_ref().is_some_and(|job| job.verifying.is_some()))
            .count();
        for index in verify_rows
            .into_iter()
            .take(MAX_PARALLEL_UPDATE_MERGES.saturating_sub(active_merges))
        {
            spawn_batch_merge(self.base_dir.clone(), &mut batch.rows[index]);
            changed = true;
        }
        if changed {
            self.persist_batch_jobs();
        }
        if needs_scan {
            self.batch_rescan_pending = true;
        }
    }

    /// Called only for a matching job key, after the torrent engine's actual
    /// completion notice (not merely a progress bar reaching 100 percent).
    pub(crate) fn handle_missing_update_completion(
        &mut self,
        key: &str,
        result: &Result<(), String>,
    ) {
        let Some(batch) = self.update_batch.as_mut() else {
            return;
        };
        let Some(row) = batch
            .rows
            .iter_mut()
            .find(|row| row.job.as_ref().is_some_and(|job| job.key == key))
        else {
            return;
        };
        let Some(job) = row.job.as_mut() else {
            return;
        };
        if !matches!(
            job.status,
            update_batch::UpdateBatchJobStatus::Queued
                | update_batch::UpdateBatchJobStatus::Downloading
        ) {
            return;
        }
        if let Err(error) = result {
            job.status = update_batch::UpdateBatchJobStatus::Failed(error.clone());
        } else {
            // Mark durable completion first. The frame schedules exactly one
            // merge worker after the remaining transfers have been polled.
            job.transfer_finished = true;
            job.status = update_batch::UpdateBatchJobStatus::Queued;
        }
        self.persist_batch_jobs();
    }

    pub(crate) fn queue_missing_updates(&mut self) {
        let Some(engine) = self.torrent_engine.as_deref() else {
            self.status = "토렌트 엔진을 사용할 수 없습니다".into();
            return;
        };
        let Some(batch) = self.update_batch.as_mut() else {
            return;
        };
        if batch.running {
            self.status = "카탈로그 대조가 완료된 뒤 일괄 다운로드를 시작하세요".into();
            return;
        }
        batch.confirm_queue = false;
        let download_root = self.base_dir.join("downloads");
        let mut queued = 0;
        let mut failed = 0;
        for row in &mut batch.rows {
            if !row.checked || row.job.is_some() {
                continue;
            }
            let Some(found) = row.found.as_ref() else {
                continue;
            };
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos().to_string())
                .unwrap_or_default();
            let destination = update_batch::safe_update_output(
                &self.base_dir,
                &found.release.topic_id,
                &row.title_id,
                &timestamp,
            );
            let sub_folder = match destination.strip_prefix(&download_root) {
                Ok(path) => path.to_path_buf(),
                Err(_) => {
                    failed += 1;
                    row.failure = Some("다운로드 경로 검증 실패".into());
                    continue;
                }
            };
            if destination.exists() {
                failed += 1;
                row.failure = Some("일괄 다운로드 폴더가 이미 존재함".into());
                continue;
            }
            let original_size = match std::fs::metadata(&row.original_path) {
                Ok(meta) if meta.is_file() && meta.len() > 0 => meta.len(),
                _ => {
                    failed += 1;
                    row.failure = Some("원본 게임 파일을 확인할 수 없어 큐 등록 중단".into());
                    continue;
                }
            };
            // The torrent preview enforces safe, relative file paths; only
            // one original torrent index is passed to librqbit.
            let output = destination.join(&found.file.path);
            let result = engine.start_download_selected(
                &format!("업데이트 · {}", row.name),
                &found.release.magnet,
                sub_folder,
                Some(found.torrent_bytes.clone()),
                Some(vec![found.file.index]),
            );
            match result {
                Ok(key) => {
                    row.job = Some(update_batch::UpdateBatchJob {
                        key,
                        directory: destination,
                        output,
                        expected_version: found.version,
                        expected_file_size: found.file.size,
                        original_size,
                        transfer_finished: false,
                        recovering_since: None,
                        status: update_batch::UpdateBatchJobStatus::Queued,
                        verifying: None,
                        merge_control: None,
                    });
                    queued += 1;
                }
                Err(error) => {
                    row.failure = Some(format!("큐 등록 실패: {error}"));
                    failed += 1;
                }
            }
        }
        self.status = format!("업데이트 다운로드 큐 등록: {queued}개 · 실패 {failed}개");
        let _ = self
            .log_sender
            .send(format!("ℹ 일괄 업데이트 · {}", self.status));
        self.persist_batch_jobs();
    }

    /// Rendered exclusively in the Missing Updates view. No changes to
    /// the global menu bar or other tabs.
    pub(crate) fn missing_update_toolbar(&mut self, ui: &mut egui::Ui) {
        let ongoing = self.update_batch.as_ref().is_some_and(|batch| {
            batch.rows.iter().any(|row| {
                row.job.as_ref().is_some_and(|job| {
                    !matches!(
                        job.status,
                        update_batch::UpdateBatchJobStatus::Failed(_)
                            | update_batch::UpdateBatchJobStatus::Verified
                            | update_batch::UpdateBatchJobStatus::Merged(_)
                    )
                })
            })
        });
        let mut scan = false;
        let mut queue = false;
        let mut stop = false;
        let mut retry = false;
        let mut cancel_merge = false;
        ui.horizontal_wrapped(|ui| {
            scan = ui
                .add_enabled(
                    !ongoing
                        && !self
                            .update_batch
                            .as_ref()
                            .is_some_and(|batch| batch.running),
                    egui::Button::new("카탈로그 전체 대조"),
                )
                .clicked();
            if let Some(batch) = self.update_batch.as_ref() {
                let (done, total) = batch.progress();
                let jobs = batch.rows.iter().filter(|row| row.job.is_some()).count();
                if jobs > 0 {
                    let merged = batch
                        .rows
                        .iter()
                        .filter(|row| {
                            row.job.as_ref().is_some_and(|job| {
                                matches!(job.status, update_batch::UpdateBatchJobStatus::Merged(_))
                            })
                        })
                        .count();
                    ui.label(format!(
                        "다운로드·병합 작업 {jobs}개 · 병합 완료 {merged}개"
                    ));
                } else {
                    ui.label(format!(
                        "대조 {done}/{total} · 업데이트 가능 {}개",
                        batch.eligible()
                    ));
                }
                let retryable = batch
                    .rows
                    .iter()
                    .filter(|row| {
                        row.job.as_ref().is_some_and(|job| {
                            job.transfer_finished
                                && matches!(
                                    job.status,
                                    update_batch::UpdateBatchJobStatus::Failed(_)
                                )
                        })
                    })
                    .count();
                if retryable > 0 {
                    retry = ui
                        .button(format!("실패한 병합 재시도 {retryable}개"))
                        .clicked();
                }
                if let Some(control) = batch
                    .rows
                    .iter()
                    .filter_map(|row| row.job.as_ref())
                    .find_map(|job| {
                        job.verifying.as_ref()?;
                        job.merge_control.as_ref()
                    })
                {
                    cancel_merge = ui
                        .add_enabled(
                            !control.is_cancel_requested(),
                            egui::Button::new(if control.is_cancel_requested() {
                                "병합 중단 처리 중"
                            } else {
                                "병합 중단"
                            }),
                        )
                        .clicked();
                }
                if batch.running {
                    stop = ui.button("대조 중단").clicked();
                } else {
                    queue = ui
                        .add_enabled(
                            batch.chosen() > 0,
                            egui::Button::new(format!(
                                "선택 {}개 다운로드 큐에 등록",
                                batch.chosen()
                            )),
                        )
                        .clicked();
                }
            } else {
                ui.label("버튼을 누르면 모든 미보유 게임의 토렌트 업데이트를 확인합니다");
            }
        });
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_update_batch_toolbar"),
                ui.min_rect(),
            )
        });
        if stop {
            if let Some(batch) = self.update_batch.as_mut() {
                batch.stop();
                self.status = "카탈로그 대조 중단 · 이미 확인한 후보는 유지됨".into();
            }
        }
        if scan {
            self.start_missing_update_audit();
        }
        if retry {
            self.retry_failed_batch_merges();
        }
        if cancel_merge {
            if let Some(batch) = self.update_batch.as_ref() {
                for job in batch.rows.iter().filter_map(|row| row.job.as_ref()) {
                    if job.verifying.is_some() {
                        if let Some(control) = &job.merge_control {
                            control.request_cancel();
                        }
                    }
                }
            }
            self.status = "병합 중단 요청 · 안전하게 작업을 끝내고 임시 파일을 정리하는 중".into();
        }
        if queue {
            if let Some(batch) = self.update_batch.as_mut() {
                batch.confirm_queue = true;
            }
        }
        let Some(batch) = self.update_batch.as_mut() else {
            return;
        };
        if batch.confirm_queue {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "확인된 {}개 업데이트를 다운로드 큐에 추가할까요?",
                    batch.chosen()
                ));
                if ui.button("큐 추가 확인").clicked() {
                    queue = true;
                }
                if ui.button("취소").clicked() {
                    batch.confirm_queue = false;
                }
            });
            if queue {
                self.queue_missing_updates();
            }
        }
        let Some(batch) = self.update_batch.as_mut() else {
            return;
        };
        if batch.rows.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(format!(
            "대조 상세 · 확인됨 {}개 / 총 {}개",
            batch.eligible(),
            batch.rows.len()
        ))
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("전체 선택").clicked() {
                    for row in &mut batch.rows {
                        if row.found.is_some() && row.job.is_none() {
                            row.checked = true;
                        }
                    }
                }
                if ui.button("전체 해제").clicked() {
                    for row in &mut batch.rows {
                        row.checked = false;
                    }
                }
            });
            egui::ScrollArea::vertical()
                .id_salt("update_audit_detail_scroll")
                .max_height(180.0)
                .show(ui, |ui| {
                    for row in &mut batch.rows {
                        ui.horizontal(|ui| {
                            let has_update = row.found.is_some();
                            ui.add_enabled_ui(has_update && row.job.is_none(), |ui| {
                                ui.checkbox(&mut row.checked, "");
                            });
                            let status = if let Some(job) = &row.job {
                                match &job.status {
                                    update_batch::UpdateBatchJobStatus::Queued => "큐 대기".into(),
                                    update_batch::UpdateBatchJobStatus::Downloading => {
                                        "다운로드 중".into()
                                    }
                                    update_batch::UpdateBatchJobStatus::Verifying => {
                                        job.merge_control.as_ref().map_or_else(
                                            || "패키지 검증 중".into(),
                                            |control| {
                                                if control.is_cancel_requested() {
                                                    "병합 중단 요청 중".into()
                                                } else {
                                                    let progress = control.progress.snapshot();
                                                    match progress.total {
                                                        Some(total) => format!(
                                                            "{} · {}%",
                                                            progress.label,
                                                            progress.completed.min(total) * 100
                                                                / total.max(1)
                                                        ),
                                                        None => progress.label,
                                                    }
                                                }
                                            },
                                        )
                                    }
                                    update_batch::UpdateBatchJobStatus::Verified => {
                                        "패키지 검증 완료 · 병합 대기".into()
                                    }
                                    update_batch::UpdateBatchJobStatus::Merged(path) => {
                                        format!("병합 완료: {}", path.display())
                                    }
                                    update_batch::UpdateBatchJobStatus::Failed(error) => {
                                        format!("실패: {error}")
                                    }
                                }
                            } else if let Some(found) = &row.found {
                                format!("다운로드 가능 v{}", found.version)
                            } else if batch.running && row.failure.is_none() {
                                "조회 대기".into()
                            } else {
                                row.failure
                                    .clone()
                                    .unwrap_or_else(|| "대조 결과 없음".into())
                            };
                            ui.label(&row.name).on_hover_text(format!(
                                "{} · {} 원본: {}",
                                row.title_id,
                                row.original_type.to_ascii_uppercase(),
                                row.original_path.display()
                            ));
                            ui.label(egui::RichText::new(status).small().color(if has_update {
                                ui.visuals().hyperlink_color
                            } else {
                                ui.visuals().weak_text_color()
                            }));
                        });
                    }
                });
        });
    }
}
