//! Crash-safe batch update journal. Stores file paths and job identities,
//! never torrent metadata, API keys, downloaded payloads or decrypted NCAs.

use crate::update_batch::{
    BatchKind, UpdateBatch, UpdateBatchJob, UpdateBatchJobStatus, UpdateBatchRow,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

const SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_JOBS: usize = 5000;

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    jobs: Vec<SavedJob>,
}

#[derive(Serialize, Deserialize)]
struct SavedJob {
    #[serde(default)]
    kind: BatchKind,
    library_id: i64,
    title_id: String,
    #[serde(default)]
    dlc_id: Option<String>,
    name: String,
    installed_version: i64,
    latest_version: i64,
    original_path: PathBuf,
    original_type: String,
    engine_key: String,
    download_folder: PathBuf,
    download_file: PathBuf,
    expected_version: i64,
    expected_file_size: u64,
    original_size: u64,
    transfer_finished: bool,
    status: UpdateBatchJobStatus,
}

fn manifest_file(base_dir: &Path) -> PathBuf {
    base_dir.join("batch-updates.json")
}

fn valid_download_location(base_dir: &Path, folder: &Path, file: &Path) -> bool {
    let root = base_dir.join("downloads");
    let Ok(folder_rel) = folder.strip_prefix(&root) else {
        return false;
    };
    let Ok(file_rel) = file.strip_prefix(folder) else {
        return false;
    };
    !folder_rel.as_os_str().is_empty()
        && folder_rel.components().count() == 1
        && folder_rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !file_rel.as_os_str().is_empty()
        && file_rel.components().count() <= 64
        && file_rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Refuse stale or untrusted batch records that could resume an unrelated
/// Title ID, a non-newer update, or a zero-length file after restart.
fn validate_job_identity(job: &SavedJob) -> Result<()> {
    anyhow::ensure!(
        job.title_id.len() == 16 && job.title_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "일괄 작업 기록의 게임 Title ID가 올바르지 않습니다"
    );
    let base = u64::from_str_radix(&job.title_id, 16)?;
    anyhow::ensure!(base & 0x1fff == 0, "기본 게임 Title ID가 아닙니다");
    anyhow::ensure!(
        job.name.chars().count() <= 512
            && job.expected_file_size > 0
            && job.original_size > 0
            && !job.engine_key.is_empty()
            && job.engine_key.len() <= 256,
        "일괄 작업 기록의 파일 크기 또는 작업 정보가 올바르지 않습니다"
    );
    if job.kind == BatchKind::Dlc {
        let id = job.dlc_id.as_deref().context("DLC 작업 ID가 없습니다")?;
        let dlc = u64::from_str_radix(id, 16)?;
        anyhow::ensure!(
            id.len() == 16
                && dlc & !0x1fff == base
                && dlc & 0x1fff >= 0x1000
                && job.expected_version >= 0,
            "DLC 작업 기록의 Title ID 또는 버전이 일치하지 않습니다"
        );
    } else {
        anyhow::ensure!(
            job.dlc_id.is_none()
                && job.expected_version > 0
                && job.expected_version > job.installed_version
                && job.expected_version >= job.latest_version,
            "업데이트 작업 기록의 Title ID 또는 설치 버전이 올바르지 않습니다"
        );
    }
    Ok(())
}

pub(crate) fn save(base_dir: &Path, batch: &UpdateBatch) -> Result<()> {
    let jobs: Vec<SavedJob> = batch
        .rows
        .iter()
        .filter_map(|row| {
            row.job.as_ref().map(|job| SavedJob {
                kind: batch.kind,
                library_id: row.library_id,
                title_id: row.title_id.clone(),
                dlc_id: row.dlc_id.clone(),
                name: row.name.clone(),
                installed_version: row.installed_version,
                latest_version: row.latest_version,
                original_path: row.original_path.clone(),
                original_type: row.original_type.clone(),
                engine_key: job.key.clone(),
                download_folder: job.directory.clone(),
                download_file: job.output.clone(),
                expected_version: job.expected_version,
                expected_file_size: job.expected_file_size,
                original_size: job.original_size,
                transfer_finished: job.transfer_finished,
                status: job.status.clone(),
            })
        })
        .collect();
    let file = manifest_file(base_dir);
    if jobs.is_empty() {
        if let Err(error) = fs::remove_file(&file) {
            anyhow::ensure!(
                error.kind() == std::io::ErrorKind::NotFound,
                "일괄 업데이트 기록 정리 실패: {error}"
            );
        }
        return Ok(());
    }
    anyhow::ensure!(
        jobs.len() <= MAX_JOBS,
        "저장할 일괄 업데이트 작업 수 한도 초과"
    );
    for job in &jobs {
        validate_job_identity(job)?;
        anyhow::ensure!(
            valid_download_location(base_dir, &job.download_folder, &job.download_file),
            "저장할 일괄 다운로드 경로가 안전하지 않습니다"
        );
    }
    let bytes = serde_json::to_vec_pretty(&Manifest {
        version: SCHEMA_VERSION,
        jobs,
    })?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "일괄 업데이트 기록이 너무 큽니다"
    );
    let mut staged = tempfile::Builder::new()
        .prefix(".nsl-batch-")
        .suffix(".json.tmp")
        .tempfile_in(base_dir)?;
    staged.write_all(&bytes)?;
    staged.as_file_mut().sync_all()?;
    staged.persist(&file).map_err(|error| {
        anyhow::anyhow!(
            "일괄 업데이트 기록을 안전하게 저장하지 못했습니다: {}",
            error.error
        )
    })?;
    Ok(())
}

pub(crate) fn load(base_dir: &Path) -> Result<Option<UpdateBatch>> {
    let path = manifest_file(base_dir);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.len() <= MAX_MANIFEST_BYTES,
        "일괄 업데이트 기록 크기가 안전 한도를 초과했습니다"
    );
    let manifest: Manifest = serde_json::from_slice(&fs::read(&path)?)
        .context("일괄 업데이트 작업 기록 형식이 올바르지 않습니다")?;
    anyhow::ensure!(
        manifest.version == SCHEMA_VERSION,
        "지원하지 않는 일괄 업데이트 기록 버전"
    );
    anyhow::ensure!(
        manifest.jobs.len() <= MAX_JOBS,
        "일괄 업데이트 작업 기록 수가 너무 많습니다"
    );
    let kind = manifest
        .jobs
        .first()
        .map_or(BatchKind::Update, |job| job.kind);
    anyhow::ensure!(
        manifest.jobs.iter().all(|job| job.kind == kind),
        "한 작업 기록에서 업데이트와 DLC가 섞여 있습니다"
    );
    let mut rows = Vec::with_capacity(manifest.jobs.len());
    for saved in manifest.jobs {
        validate_job_identity(&saved)?;
        anyhow::ensure!(
            valid_download_location(base_dir, &saved.download_folder, &saved.download_file),
            "안전하지 않은 일괄 업데이트 다운로드 경로"
        );
        let status = if matches!(saved.status, UpdateBatchJobStatus::Verifying) {
            // The worker did not survive exit. A recorded completed transfer
            // is safe to verify/merge again; a partial transfer must wait for
            // the restored librqbit session to report actual completion.
            UpdateBatchJobStatus::Queued
        } else {
            saved.status
        };
        let job = UpdateBatchJob {
            key: saved.engine_key,
            directory: saved.download_folder,
            output: saved.download_file,
            expected_version: saved.expected_version,
            expected_file_size: saved.expected_file_size,
            original_size: saved.original_size,
            transfer_finished: saved.transfer_finished,
            recovering_since: Some(std::time::Instant::now()),
            status,
            verifying: None,
            merge_control: None,
        };
        rows.push(UpdateBatchRow {
            library_id: saved.library_id,
            title_id: saved.title_id,
            dlc_id: saved.dlc_id,
            name: saved.name,
            installed_version: saved.installed_version,
            latest_version: saved.latest_version,
            original_path: saved.original_path,
            original_type: saved.original_type,
            candidates: Vec::new(),
            next_candidate: 0,
            found: None,
            checked: false,
            failure: None,
            job: Some(job),
        });
    }
    let count = rows.len();
    Ok(Some(UpdateBatch {
        kind,
        rows,
        running: false,
        cursor: count,
        active: None,
        confirm_queue: false,
        preview_cache: Default::default(),
        preview_cache_bytes: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_batch::UpdateBatchJobStatus;

    fn example_batch(root: &Path) -> UpdateBatch {
        let folder = root.join("downloads").join("batch-safe-123");
        UpdateBatch {
            kind: BatchKind::Update,
            rows: vec![UpdateBatchRow {
                library_id: 42,
                title_id: "0100D2C0115B6000".into(),
                dlc_id: None,
                name: "Game".into(),
                installed_version: 65536,
                latest_version: 196608,
                original_path: root.join("original.nsz"),
                original_type: "nsz".into(),
                candidates: vec![],
                next_candidate: 0,
                found: None,
                checked: false,
                failure: None,
                job: Some(UpdateBatchJob {
                    key: "t14".into(),
                    directory: folder.clone(),
                    output: folder.join("nested/update.nsz"),
                    expected_version: 196608,
                    expected_file_size: 8192,
                    original_size: 900,
                    transfer_finished: true,
                    recovering_since: None,
                    status: UpdateBatchJobStatus::Verifying,
                    verifying: None,
                    merge_control: None,
                }),
            }],
            running: false,
            cursor: 1,
            active: None,
            confirm_queue: false,
            preview_cache: Default::default(),
            preview_cache_bytes: 0,
        }
    }

    #[test]
    fn journal_round_trip_restores_metadata_without_resuming_stale_worker() {
        let dir = tempfile::tempdir().unwrap();
        let mut example = example_batch(dir.path());
        let control = std::sync::Arc::new(crate::update_merge::MergeControl::default());
        control.request_cancel();
        example.rows[0].job.as_mut().unwrap().merge_control = Some(control);
        save(dir.path(), &example).unwrap();
        let recovered = load(dir.path()).unwrap().unwrap();
        let row = &recovered.rows[0];
        let job = row.job.as_ref().unwrap();
        assert_eq!(job.key, "t14");
        assert_eq!(job.expected_version, 196608);
        assert!(job.transfer_finished);
        assert_eq!(job.status, UpdateBatchJobStatus::Queued);
        assert!(
            job.merge_control.is_none(),
            "stale cancellation must not survive restart"
        );
        assert!(job.recovering_since.is_some());
        assert!(job.output.ends_with("nested/update.nsz"));
        assert!(!recovered.running);
    }

    #[test]
    fn dlc_journal_preserves_id_and_zero_version_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let mut batch = example_batch(dir.path());
        batch.kind = BatchKind::Dlc;
        batch.rows[0].dlc_id = Some("0100D2C0115B7001".into());
        batch.rows[0].job.as_mut().unwrap().expected_version = 0;
        save(dir.path(), &batch).unwrap();
        let restored = load(dir.path()).unwrap().unwrap();
        assert_eq!(restored.kind, BatchKind::Dlc);
        assert_eq!(restored.rows[0].dlc_id.as_deref(), Some("0100D2C0115B7001"));
        assert_eq!(restored.rows[0].job.as_ref().unwrap().expected_version, 0);
        assert_eq!(
            restored.rows[0].job.as_ref().unwrap().status,
            UpdateBatchJobStatus::Queued
        );
        assert!(restored.rows[0]
            .job
            .as_ref()
            .unwrap()
            .merge_control
            .is_none());
        let mut corrupt: serde_json::Value =
            serde_json::from_slice(&fs::read(manifest_file(dir.path())).unwrap()).unwrap();
        corrupt["jobs"][0]["dlc_id"] = serde_json::json!("0100D2C0115B7002BAD");
        fs::write(
            manifest_file(dir.path()),
            serde_json::to_vec(&corrupt).unwrap(),
        )
        .unwrap();
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn journal_rejects_path_traversal_and_preserves_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut example = example_batch(dir.path());
        save(dir.path(), &example).unwrap();
        let previous = fs::read(manifest_file(dir.path())).unwrap();
        example.rows[0].job.as_mut().unwrap().output = dir.path().join("other/place.nsp");
        assert!(save(dir.path(), &example).is_err());
        assert_eq!(fs::read(manifest_file(dir.path())).unwrap(), previous);
        let mut value: serde_json::Value = serde_json::from_slice(&previous).unwrap();
        value["jobs"][0]["download_file"] = serde_json::json!("C:\\Other\\malicious.nsp");
        fs::write(
            manifest_file(dir.path()),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn stage25_corrupt_batch_identity_and_lengths_fail_closed_without_original_loss() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.nsz");
        std::fs::write(&source, b"stage25 untouched original").unwrap();
        let example = example_batch(dir.path());
        save(dir.path(), &example).unwrap();
        let trusted_bytes = fs::read(manifest_file(dir.path())).unwrap();
        let baseline: serde_json::Value = serde_json::from_slice(&trusted_bytes).unwrap();
        let corrupt = [
            ("title_id", serde_json::json!("0100D2C0115B6800")),
            ("title_id", serde_json::json!("0100D2C0115B7001")),
            ("expected_version", serde_json::json!(65536)),
            ("expected_version", serde_json::json!(0)),
            ("expected_file_size", serde_json::json!(0)),
            ("original_size", serde_json::json!(0)),
            ("engine_key", serde_json::json!("")),
            ("dlc_id", serde_json::json!("0100D2C0115B7001")),
        ];
        for (field, value) in corrupt {
            let mut entry = baseline.clone();
            entry["jobs"][0][field] = value;
            let payload = serde_json::to_vec(&entry).unwrap();
            fs::write(manifest_file(dir.path()), &payload).unwrap();
            let error = load(dir.path())
                .err()
                .expect("corrupted batch journal must not be restored");
            assert!(!error.to_string().is_empty());
            assert_eq!(fs::read(manifest_file(dir.path())).unwrap(), payload);
            assert_eq!(fs::read(&source).unwrap(), b"stage25 untouched original");
        }
        fs::write(manifest_file(dir.path()), &trusted_bytes).unwrap();
        assert!(load(dir.path()).unwrap().is_some());
        let mut invalid_save = example_batch(dir.path());
        invalid_save.rows[0]
            .job
            .as_mut()
            .unwrap()
            .expected_file_size = 0;
        assert!(save(dir.path(), &invalid_save).is_err());
        assert_eq!(fs::read(manifest_file(dir.path())).unwrap(), trusted_bytes);
        assert_eq!(fs::read(&source).unwrap(), b"stage25 untouched original");
    }
}
