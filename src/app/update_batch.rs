//! Missing Updates: catalog audit, per-title markings, and a selective queue.
//! The original game files are never changed by discovery or downloads.

use crate::*;
use anyhow::Context;
use std::path::Path;

// Streaming game merges can occupy tens of GiB of scratch disk space. Never
// start one worker per completed torrent: process just one merge at a time.
const MAX_PARALLEL_UPDATE_MERGES: usize = 1;

/// A torrent may contain *multiple games' patches* as well as dozens of DLCs.
/// Register a single selected-files transfer per infohash for BOTH kinds:
/// librqbit keys by infohash, not the magnet tracker URL, and cannot safely
/// host multiple independent only_files handles of the same torrent.
pub(crate) fn grouped_batch_downloads(batch: &update_batch::UpdateBatch) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut magnets = HashMap::<String, usize>::new();
    for (index, row) in batch.rows.iter().enumerate() {
        if !row.checked || row.job.is_some() {
            continue;
        }
        let Some(found) = &row.found else {
            continue;
        };
        // Different tracker URLs can refer to the same infohash. Fall back
        // to the full magnet only when no infohash can be parsed at all.
        let identity = found
            .release
            .info_hash()
            .unwrap_or_else(|| found.release.magnet.clone());
        if let Some(group) = magnets.get(&identity) {
            groups[*group].push(index);
            continue;
        }
        magnets.insert(identity, groups.len());
        groups.push(vec![index]);
    }
    groups
}

/// Guard against conflicting previews for the same torrent. A stale or
/// untrusted catalog release must not map one selected torrent index to two
/// paths, sizes or different Title IDs, or assign two indices to one output
/// filename. Compute exactly once for both download selection and disk budget.
pub(crate) fn checked_group_selection(
    batch: &update_batch::UpdateBatch,
    ready: &[(usize, u64)],
) -> Result<(Vec<usize>, u64), String> {
    let mut indices =
        std::collections::BTreeMap::<usize, (PathBuf, u64, String, Option<String>)>::new();
    let mut output_paths = HashMap::<PathBuf, usize>::new();
    for &(row_index, _) in ready {
        let row = batch
            .rows
            .get(row_index)
            .ok_or_else(|| "일괄 다운로드 행 인덱스가 잘못되었습니다".to_string())?;
        let found = row
            .found
            .as_ref()
            .ok_or_else(|| "일괄 다운로드 선택 파일의 메타데이터가 없습니다".to_string())?;
        let file = &found.file;
        if file.size == 0
            || !file
                .path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err("토렌트 파일 크기 또는 경로가 올바르지 않습니다".into());
        }
        let identity = (
            file.path.clone(),
            file.size,
            row.title_id.clone(),
            row.dlc_id.clone(),
        );
        if let Some(previous) = indices.get(&file.index) {
            if previous != &identity {
                return Err(
                    "동일 토렌트의 파일 인덱스에 서로 다른 게임 ID·경로·크기가 있습니다".into(),
                );
            }
        } else {
            if output_paths.insert(file.path.clone(), file.index).is_some() {
                return Err("다른 토렌트 인덱스가 동일 출력 파일명을 사용합니다".into());
            }
            indices.insert(file.index, identity);
        }
    }
    let mut bytes = 0u64;
    for (_, length, _, _) in indices.values() {
        bytes = bytes
            .checked_add(*length)
            .ok_or_else(|| "토렌트 파일 선택 크기 합계가 계산 범위를 초과했습니다".to_string())?;
    }
    Ok((indices.into_keys().collect(), bytes))
}

/// Persisted torrent runtime keys can be reused after restart. Rebind a
/// restored job using its dedicated output directory, never a stale `tNN`.
pub(crate) fn batch_job_progress<'a>(
    job: &update_batch::UpdateBatchJob,
    progress: &'a [(String, torrent::TorrentProgress)],
) -> Option<(&'a String, &'a torrent::TorrentProgress)> {
    let folder_match = |item: &&(String, torrent::TorrentProgress)| {
        item.1.output_folder.as_deref() == Some(job.directory.as_path())
    };
    if job.recovering_since.is_some() {
        return progress
            .iter()
            .find(folder_match)
            .map(|(key, value)| (key, value));
    }
    progress
        .iter()
        .find(|(key, value)| {
            key == &job.key
                && value
                    .output_folder
                    .as_deref()
                    .is_none_or(|folder| folder == job.directory.as_path())
        })
        .or_else(|| progress.iter().find(folder_match))
        .map(|(key, value)| (key, value))
}

/// For DLCs of one base game, finish previously queued packages first and
/// feed the last published result into the next merge. Otherwise each DLC
/// would be merged against the original and the final file would lose DLCs.
pub(crate) fn cumulative_dlc_input(
    batch: &update_batch::UpdateBatch,
    index: usize,
) -> Option<Option<PathBuf>> {
    if batch.kind == update_batch::BatchKind::Update {
        return Some(None);
    }
    let row = &batch.rows[index];
    let mut latest = None;
    for earlier in batch.rows[..index]
        .iter()
        .filter(|prior| prior.title_id == row.title_id && prior.original_path == row.original_path)
    {
        if let Some(job) = &earlier.job {
            match &job.status {
                update_batch::UpdateBatchJobStatus::Merged(path) => latest = Some(path.clone()),
                update_batch::UpdateBatchJobStatus::Verified => {}
                _ => return None, // Incomplete or failed earlier DLC must not be bypassed.
            }
        }
    }
    Some(latest)
}

/// Only after the last selected DLC for one game has been merged may its
/// previous NSL-generated results be retired. First atomically redirect the
/// crash-recovery journal to the final output, then remove obsolete outputs.
/// Downloaded packages and the original game are never cleanup candidates.
///
/// Paths must exactly match native output_for_dlc() for a job in this batch.
/// This is deliberately stricter than merely checking the "[NSL DLC]" prefix.
pub(crate) fn consolidate_finished_dlc_outputs(
    base_dir: &Path,
    batch: &mut update_batch::UpdateBatch,
) -> anyhow::Result<u64> {
    if batch.kind != update_batch::BatchKind::Dlc || batch.running {
        return Ok(0);
    }
    // Group once. A library can have thousands of DLC rows, so scanning all
    // rows again for each game would stall the egui frame (quadratic work).
    let mut groups: HashMap<(String, PathBuf), Vec<usize>> = HashMap::new();
    for (index, row) in batch.rows.iter().enumerate() {
        groups
            .entry((row.title_id.clone(), row.original_path.clone()))
            .or_default()
            .push(index);
    }
    let mut plans: Vec<(Vec<usize>, PathBuf, Vec<PathBuf>)> = Vec::new();
    for (identity, members) in groups {
        let related = members
            .iter()
            .map(|index| (*index, &batch.rows[*index]))
            .collect::<Vec<_>>();
        if related.iter().any(|(_, row)| {
            (row.checked && row.found.is_some() && row.job.is_none())
                || row.job.as_ref().is_some_and(|job| {
                    !matches!(job.status, update_batch::UpdateBatchJobStatus::Merged(_))
                })
        }) {
            continue;
        }
        let completed = related
            .iter()
            .filter_map(|(index, row)| row.job.as_ref().map(|job| (*index, *row, job)))
            .collect::<Vec<_>>();
        if completed.len() < 2 {
            continue;
        }
        let expected = completed
            .iter()
            .map(|(_, row, job)| {
                update_merge::expected_dlc_output(
                    &row.original_path,
                    &row.title_id,
                    row.dlc_id.as_deref().context("DLC ID가 없는 병합 작업")?,
                    job.expected_version,
                )
            })
            .collect::<anyhow::Result<std::collections::HashSet<_>>>()?;
        let published = completed
            .iter()
            .map(|(_, _, job)| match &job.status {
                update_batch::UpdateBatchJobStatus::Merged(path) => Ok(path.clone()),
                _ => anyhow::bail!("완료되지 않은 DLC 병합"),
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        anyhow::ensure!(
            published.iter().all(|path| expected.contains(path)),
            "병합 결과가 NSL이 생성한 DLC 경로와 일치하지 않아 정리하지 않았습니다"
        );
        let final_path = published.last().context("DLC 최종 결과 없음")?.clone();
        let latest = completed.last().context("DLC 마지막 작업 없음")?;
        let latest_expected = update_merge::expected_dlc_output(
            &latest.1.original_path,
            &latest.1.title_id,
            latest.1.dlc_id.as_deref().context("최종 DLC ID 누락")?,
            latest.2.expected_version,
        )?;
        anyhow::ensure!(
            final_path == latest_expected,
            "최신 DLC 병합 결과가 최종 경로와 일치하지 않습니다"
        );
        let retired = published
            .iter()
            .filter(|path| **path != final_path)
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if retired.is_empty() {
            continue;
        }
        let original_parent = identity
            .1
            .parent()
            .context("원본 게임 상위 폴더 누락")?
            .canonicalize()?;
        for path in std::iter::once(&final_path).chain(retired.iter()) {
            anyhow::ensure!(
                path != &identity.1
                    && path.parent().is_some_and(|parent| {
                        parent.canonicalize().ok().as_deref() == Some(original_parent.as_path())
                    }),
                "원본 게임 폴더 밖의 파일은 정리할 수 없습니다"
            );
            match std::fs::symlink_metadata(path) {
                Ok(metadata) => {
                    anyhow::ensure!(
                        metadata.file_type().is_file() && metadata.len() > 0,
                        "DLC 중간 결과가 일반 파일이 아니거나 비어 있습니다"
                    );
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound && path != &final_path => {}
                Err(error) => return Err(error.into()),
            }
        }
        plans.push((
            completed.iter().map(|(index, _, _)| *index).collect(),
            final_path,
            retired,
        ));
    }
    if plans.is_empty() {
        return Ok(0);
    }
    let mut rollback = Vec::new();
    for (indices, final_path, _) in &plans {
        for &index in indices {
            let job = batch.rows[index].job.as_mut().context("DLC 작업 누락")?;
            rollback.push((index, job.status.clone()));
            job.status = update_batch::UpdateBatchJobStatus::Merged(final_path.clone());
        }
    }
    if let Err(error) = update_manifest::save(base_dir, batch) {
        for (index, old) in rollback {
            batch.rows[index].job.as_mut().unwrap().status = old;
        }
        return Err(error);
    }
    let mut freed = 0u64;
    for (_, _, retired) in plans {
        for path in retired {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    // We deliberately do not guess the original size here;
                    // success is the number of files removed, not bytes.
                    freed += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => anyhow::bail!(
                    "완료된 DLC의 이전 병합 파일을 정리하지 못했습니다 (원본 보존): {}: {error}",
                    path.display()
                ),
            }
        }
    }
    Ok(freed)
}

/// A completed torrent is rechecked from disk after restart. Never trust a
/// filename or a progress bar without scanning the actual package CNMT.
fn spawn_batch_merge(
    base_dir: PathBuf,
    row: &mut update_batch::UpdateBatchRow,
    cumulative_input: Option<PathBuf>,
) {
    let Some(job) = row.job.as_mut() else {
        return;
    };
    if job.verifying.is_some() {
        return;
    }
    job.transfer_finished = true;
    let path = job.output.clone();
    let title_id = row.title_id.clone();
    let dlc_id = row.dlc_id.clone();
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
            let source = if let Some(previous) = cumulative_input {
                anyhow::ensure!(
                    previous.parent() == original.parent()
                        && previous
                            .file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with("[NSL DLC]"))
                        && previous.is_file(),
                    "이전 DLC 누적 병합 결과를 확인할 수 없습니다"
                );
                previous
            } else {
                original
            };
            anyhow::ensure!(
                expected_file_size > 0 && std::fs::metadata(&path)?.len() == expected_file_size,
                "다운로드 파일 크기가 검증된 토렌트 목록과 일치하지 않습니다"
            );
            if let Some(dlc_id) = &dlc_id {
                update_batch::verify_downloaded_dlc(&base_dir, &path, &title_id, dlc_id, wanted)?;
            } else {
                update_batch::verify_downloaded_patch(
                    &base_dir, &path, &title_id, wanted, installed,
                )?;
            }
            control.ensure_running()?;
            let merged = if let Some(dlc_id) = &dlc_id {
                update_merge::merge_or_recover_dlc_controlled(
                    &source, &path, &title_id, dlc_id, wanted, &control,
                )?
            } else {
                update_merge::merge_or_recover_existing_controlled(
                    &source, &path, &title_id, wanted, &control,
                )?
            };
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
        let is_dlc = self.folder_filter == "MISSING_DLCS";
        if !is_dlc && self.folder_filter != "MISSING_UPDATES" {
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
            .get(if is_dlc {
                "exception_dlc"
            } else {
                "exception_update"
            })
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let batch = if is_dlc {
            update_batch::new_dlc_batch(&self.rows, &self.catalog_state.items, excluded)
        } else {
            update_batch::new_update_batch(&self.rows, &self.catalog_state.items, excluded)
        };
        let eligible = batch
            .rows
            .iter()
            .filter(|row| !row.candidates.is_empty())
            .count();
        self.status = format!(
            "{} 카탈로그 대조 시작: 항목 {}개, 토렌트 후보 {}개",
            if is_dlc { "DLC" } else { "업데이트" },
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
                    "카탈로그 대조 완료: {}개 중 {} 파일 확인 {}개",
                    batch.rows.len(),
                    if batch.kind == update_batch::BatchKind::Dlc {
                        "DLC"
                    } else {
                        "업데이트"
                    },
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
            ) || (matches!(job.status, update_batch::UpdateBatchJobStatus::Failed(_))
                && !job.transfer_finished)
            {
                if job.transfer_finished {
                    verify_rows.push(index);
                } else if let Some((key, progress)) =
                    batch_job_progress(job, &self.torrent_progress)
                {
                    if job.key != *key {
                        job.key = key.clone();
                        changed = true;
                    }
                    job.recovering_since = None;
                    if let Some(error) = &progress.error {
                        let next = update_batch::UpdateBatchJobStatus::Failed(error.clone());
                        if job.status != next {
                            job.status = next;
                            changed = true;
                        }
                    } else if progress.finished {
                        // A 100% progress snapshot is published BEFORE the
                        // engine pauses the torrent and releases file locks.
                        // Wait for its actual successful Download notice,
                        // which durably marks transfer_finished. Otherwise
                        // Windows can reject the merge with a sharing error.
                        if job.status != update_batch::UpdateBatchJobStatus::Downloading {
                            job.status = update_batch::UpdateBatchJobStatus::Downloading;
                            changed = true;
                        }
                    } else {
                        // A previously failed download can resume. A paused
                        // torrent is queued, not a failed post-download merge.
                        let next = if progress.paused {
                            update_batch::UpdateBatchJobStatus::Queued
                        } else {
                            update_batch::UpdateBatchJobStatus::Downloading
                        };
                        if job.status != next {
                            job.status = next;
                            changed = true;
                        }
                    }
                } else if job
                    .recovering_since
                    .is_some_and(|started| started.elapsed() > std::time::Duration::from_secs(60))
                    && !matches!(job.status, update_batch::UpdateBatchJobStatus::Failed(_))
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
                            self.status = format!(
                                "{} 병합 완료: {}",
                                if row.dlc_id.is_some() {
                                    "DLC"
                                } else {
                                    "업데이트"
                                },
                                row.name
                            );
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
        let mut available = MAX_PARALLEL_UPDATE_MERGES.saturating_sub(active_merges);
        for index in verify_rows {
            if available == 0 {
                break;
            }
            let Some(cumulative_input) = cumulative_dlc_input(batch, index) else {
                continue;
            };
            spawn_batch_merge(
                self.base_dir.clone(),
                &mut batch.rows[index],
                cumulative_input,
            );
            changed = true;
            available -= 1;
        }
        if changed {
            self.persist_batch_jobs();
        }
        if needs_scan {
            if let Some(batch) = self.update_batch.as_mut() {
                match consolidate_finished_dlc_outputs(&self.base_dir, batch) {
                    Ok(cleaned) if cleaned > 0 => {
                        self.status = format!(
                            "DLC 누적 병합 완료 · 최종 파일만 유지 · 중간 파일 {cleaned}개 정리"
                        );
                        let _ = self.log_sender.send(format!("✅ {}", self.status));
                    }
                    Err(error) => {
                        let _ = self.log_sender.send(format!(
                            "⚠ DLC 최종 결과는 유지되지만 중간 파일 자동 정리를 완료하지 못했습니다: {error}"
                        ));
                    }
                    _ => {}
                }
            }
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
        let mut changed = false;
        for row in &mut batch.rows {
            let Some(job) = row.job.as_mut().filter(|job| job.key == key) else {
                continue;
            };
            if !matches!(
                job.status,
                update_batch::UpdateBatchJobStatus::Queued
                    | update_batch::UpdateBatchJobStatus::Downloading
            ) {
                continue;
            }
            if let Err(error) = result {
                job.status = update_batch::UpdateBatchJobStatus::Failed(error.clone());
            } else {
                // Multiple DLC files from this torrent share one key. All of
                // them become durable transfer-completed jobs together.
                job.transfer_finished = true;
                job.status = update_batch::UpdateBatchJobStatus::Queued;
            }
            changed = true;
        }
        if changed {
            self.persist_batch_jobs();
        }
    }

    pub(crate) fn queue_missing_updates(&mut self) {
        if self.torrent_engine.is_none() {
            self.status = "토렌트 엔진을 사용할 수 없습니다".into();
            return;
        }
        if self.update_batch.is_none() {
            return;
        }
        let download_root = self.base_dir.join("downloads");
        let available_bytes = match fs2::available_space(&download_root) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status = format!("다운로드 드라이브의 여유 공간 확인 실패: {error}");
                return;
            }
        };
        self.queue_missing_updates_with_capacity(available_bytes);
    }

    /// Accept the physical free-space reading as an explicit input, allowing
    /// isolated QA to exercise insufficient-capacity rejection without ever
    /// filling a user's disk or altering the real filesystem quota.
    pub(crate) fn queue_missing_updates_with_capacity(&mut self, available_bytes: u64) {
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
        // Reserve remaining bytes of active downloads before admitting any
        // new batch. Count recovered jobs without an attached runtime row as
        // well; otherwise a second queue click could overcommit the drive.
        let live_folders = self
            .torrent_progress
            .iter()
            .filter_map(|(_, progress)| progress.output_folder.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut reserved_bytes = self
            .torrent_progress
            .iter()
            .filter(|(_, progress)| !progress.finished)
            .fold(0u64, |bytes, (_, progress)| {
                bytes.saturating_add(progress.total_bytes.saturating_sub(progress.progress_bytes))
            });
        for row in &batch.rows {
            if let Some(job) = row
                .job
                .as_ref()
                .filter(|job| !job.transfer_finished && !live_folders.contains(&job.directory))
            {
                reserved_bytes = reserved_bytes.saturating_add(job.expected_file_size);
            }
        }
        let mut queued = 0;
        let mut failed = 0;
        let mut torrent_jobs = 0;
        let mut space_rejections = 0;
        for indices in grouped_batch_downloads(batch) {
            let first = &batch.rows[indices[0]];
            let first_found = first
                .found
                .as_ref()
                .expect("a selected DLC must have a match");
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos().to_string())
                .unwrap_or_default();
            let destination = update_batch::safe_update_output(
                &self.base_dir,
                &first_found.release.topic_id,
                &first.title_id,
                &timestamp,
            );
            let sub_folder = match destination.strip_prefix(&download_root) {
                Ok(path) => path.to_path_buf(),
                Err(_) => {
                    for index in indices {
                        batch.rows[index].failure = Some("다운로드 경로 검증 실패".into());
                        failed += 1;
                    }
                    continue;
                }
            };
            if destination.exists() {
                for index in indices {
                    batch.rows[index].failure = Some("일괄 다운로드 폴더가 이미 존재함".into());
                    failed += 1;
                }
                continue;
            }
            let mut ready = Vec::new();
            for index in indices {
                let row = &mut batch.rows[index];
                match std::fs::metadata(&row.original_path) {
                    Ok(meta) if meta.is_file() && meta.len() > 0 => {
                        ready.push((index, meta.len()));
                    }
                    _ => {
                        row.failure = Some("원본 게임 파일을 확인할 수 없어 큐 등록 중단".into());
                        failed += 1;
                    }
                }
            }
            if ready.is_empty() {
                continue;
            }
            let (only_files, requested_bytes) = match checked_group_selection(batch, &ready) {
                Ok(selection) => selection,
                Err(error) => {
                    // Reject every row in this torrent rather than partially
                    // downloading metadata with conflicting file indices.
                    for (index, _) in ready {
                        batch.rows[index].failure = Some(error.clone());
                        failed += 1;
                    }
                    continue;
                }
            };
            let proposed_reservation = match update_batch::reserve_batch_download_space(
                available_bytes,
                reserved_bytes,
                requested_bytes,
            ) {
                Ok(bytes) => bytes,
                Err(error) => {
                    for (index, _) in ready {
                        batch.rows[index].failure = Some(error.clone());
                        failed += 1;
                        space_rejections += 1;
                    }
                    continue;
                }
            };
            let first_index = ready[0].0;
            let found = batch.rows[first_index].found.as_ref().unwrap();
            let label = format!(
                "{} · {}{}",
                if batch.kind == update_batch::BatchKind::Dlc {
                    "DLC"
                } else {
                    "업데이트"
                },
                batch.rows[first_index].name,
                if ready.len() > 1 {
                    format!(" 외 {}개", ready.len() - 1)
                } else {
                    String::new()
                }
            );
            let result = engine.start_download_selected(
                &label,
                &found.release.magnet,
                sub_folder,
                Some((*found.torrent_bytes).clone()),
                Some(only_files),
            );
            match result {
                Ok(key) => {
                    reserved_bytes = proposed_reservation;
                    torrent_jobs += 1;
                    for (index, original_size) in ready {
                        let row = &mut batch.rows[index];
                        let found = row.found.as_ref().unwrap();
                        row.job = Some(update_batch::UpdateBatchJob {
                            key: key.clone(),
                            directory: destination.clone(),
                            output: destination.join(&found.file.path),
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
                }
                Err(error) => {
                    for (index, _) in ready {
                        batch.rows[index].failure = Some(format!("큐 등록 실패: {error}"));
                        failed += 1;
                    }
                }
            }
        }
        self.status = format!(
            "{} 다운로드 큐 등록: {queued}개 · 토렌트 {torrent_jobs}건 · 실패 {failed}개{}",
            if batch.kind == update_batch::BatchKind::Dlc {
                "DLC"
            } else {
                "업데이트"
            },
            if space_rejections > 0 {
                format!(" · 저장 공간 부족 {space_rejections}개")
            } else {
                String::new()
            }
        );
        let _ = self
            .log_sender
            .send(format!("ℹ 일괄 업데이트 · {}", self.status));
        self.persist_batch_jobs();
    }

    /// Rendered exclusively in the Missing Updates view. No changes to
    /// the global menu bar or other tabs.
    pub(crate) fn missing_update_toolbar(&mut self, ui: &mut egui::Ui) {
        let desired_kind = if self.folder_filter == "MISSING_DLCS" {
            update_batch::BatchKind::Dlc
        } else {
            update_batch::BatchKind::Update
        };
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
        if self
            .update_batch
            .as_ref()
            .is_some_and(|batch| batch.kind != desired_kind)
        {
            let mut scan = false;
            ui.horizontal_wrapped(|ui| {
                scan = ui
                    .add_enabled(!ongoing, egui::Button::new("카탈로그 전체 대조"))
                    .clicked();
                ui.label(if ongoing {
                    "다른 일괄 작업 진행 중 · 완료 후 이 목록을 대조할 수 있습니다"
                } else {
                    "이 목록은 아직 카탈로그와 대조하지 않았습니다"
                });
            });
            if scan {
                self.start_missing_update_audit();
            }
            return;
        }
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
                        "대조 {done}/{total} · {} 확인 {}개",
                        if desired_kind == update_batch::BatchKind::Dlc {
                            "DLC"
                        } else {
                            "업데이트"
                        },
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
                ui.label(if desired_kind == update_batch::BatchKind::Dlc {
                    "카탈로그에서 미보유 DLC의 Title ID를 대조하고 확인된 파일만 선택합니다"
                } else {
                    "버튼을 누르면 모든 미보유 게임의 토렌트 업데이트를 확인합니다"
                });
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
                    "확인된 {}개 {}를 다운로드 큐에 추가할까요?",
                    batch.chosen(),
                    if desired_kind == update_batch::BatchKind::Dlc {
                        "DLC"
                    } else {
                        "업데이트"
                    }
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
                                if row.dlc_id.is_some() {
                                    format!("DLC 파일 확인 · v{}", found.version)
                                } else {
                                    format!("다운로드 가능 v{}", found.version)
                                }
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
