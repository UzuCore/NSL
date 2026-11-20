//! Missing Updates: catalog audit, per-title markings, and a selective queue.
//! The original game files are never changed by discovery or downloads.

use crate::*;

impl NslApp {
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
        for row in &mut batch.rows {
            let Some(job) = row.job.as_mut() else {
                continue;
            };
            if job.status == update_batch::UpdateBatchJobStatus::Queued
                && self
                    .torrent_progress
                    .iter()
                    .any(|(key, state)| key == &job.key && !state.finished && state.error.is_none())
            {
                job.status = update_batch::UpdateBatchJobStatus::Downloading;
            }
            if let Some(receiver) = &job.verifying {
                match receiver.try_recv() {
                    Ok(Ok(output)) => {
                        job.verifying = None;
                        if let Some(output) = output {
                            job.status = update_batch::UpdateBatchJobStatus::Merged(output);
                            self.status = format!("업데이트 병합 완료: {}", row.name);
                            needs_scan = true;
                        } else {
                            job.status = update_batch::UpdateBatchJobStatus::Verified;
                            self.status =
                                format!("업데이트 검증 완료 · 압축 병합 대기: {}", row.name);
                        }
                    }
                    Ok(Err(error)) => {
                        job.verifying = None;
                        job.status = update_batch::UpdateBatchJobStatus::Failed(error);
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        job.verifying = None;
                        job.status = update_batch::UpdateBatchJobStatus::Failed(
                            "패키지 검증 작업이 예기치 않게 종료됨".into(),
                        );
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                }
            }
        }
        if needs_scan && self.scan_result.is_none() {
            self.start_scan(true);
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
        if let Err(error) = result {
            job.status = update_batch::UpdateBatchJobStatus::Failed(error.clone());
            return;
        }
        let path = job.output.clone();
        let base_dir = self.base_dir.clone();
        let title_id = row.title_id.clone();
        let wanted = row.found.as_ref().map(|item| item.version).unwrap_or(0);
        let installed = row.installed_version;
        let original = row.original_path.clone();
        let original_type = row.original_type.clone();
        let (send, recv) = mpsc::channel();
        job.verifying = Some(recv);
        job.status = update_batch::UpdateBatchJobStatus::Verifying;
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Option<PathBuf>> {
                update_batch::verify_downloaded_patch(
                    &base_dir, &path, &title_id, wanted, installed,
                )?;
                if original_type == "nsp"
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("nsp"))
                {
                    let output =
                        update_merge::merge_verified_nsp(&original, &path, &title_id, wanted)?;
                    Ok(Some(output))
                } else {
                    // NSZ/XCZ need genuine NCZ compression and XCI partition
                    // packing. Preserve the verified download and original.
                    Ok(None)
                }
            })()
            .map_err(|error| error.to_string());
            let _ = send.send(result);
        });
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
                        output,
                        status: update_batch::UpdateBatchJobStatus::Queued,
                        verifying: None,
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
                ui.label(format!(
                    "대조 {done}/{total} · 업데이트 가능 {}개",
                    batch.eligible()
                ));
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
                                        "패키지 검증 중".into()
                                    }
                                    update_batch::UpdateBatchJobStatus::Verified => {
                                        "패키지 검증 완료 · NSZ/XCZ 병합·재압축 대기".into()
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
                                "{} · 원본: {}",
                                row.title_id,
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
