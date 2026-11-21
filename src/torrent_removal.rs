//! Crash-safe, narrowly scoped deletion intents for librqbit's JSON session.
//!
//! librqbit 9.1.0 removes a managed torrent from memory *before* saving the
//! updated session.json, and it logs/swallows persistence write errors. If a
//! write fails after the preflight, an old entry can return on next launch.
//! These intents keep the exact (session id, info hash, output folder) identity
//! and let the next NSL launch retry **session-only** removal. Never remove
//! downloaded payload files during journal recovery.

use librqbit::ManagedTorrent;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RemovalIntent {
    pub id: usize,
    pub info_hash: String,
    pub output_folder: PathBuf,
}

impl RemovalIntent {
    pub fn from_handle(handle: &ManagedTorrent) -> Self {
        Self {
            id: handle.id(),
            info_hash: format!("{:?}", handle.info_hash()),
            output_folder: handle.output_folder().to_path_buf(),
        }
    }

    pub fn matches_handle(&self, handle: &ManagedTorrent) -> bool {
        self.id == handle.id()
            && self.info_hash == format!("{:?}", handle.info_hash())
            && self.output_folder == handle.output_folder()
    }
}

fn journal_dir(persistence: &Path) -> PathBuf {
    persistence.join("nsl-deletion-intents")
}

fn safe_journal_dir(persistence: &Path) -> Result<PathBuf, String> {
    let dir = journal_dir(persistence);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err("토렌트 삭제 기록 폴더가 안전하지 않습니다".into());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Err(error) = fs::create_dir(&dir) {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!("토렌트 삭제 기록 폴더 생성 실패: {error}"));
                }
            }
        }
        Err(e) => return Err(format!("토렌트 삭제 기록 폴더 확인 실패: {e}")),
        _ => {}
    }
    // Another NSL command or process could create or replace the directory
    // after the initial stat. Never write an intent through a symlink.
    let checked = fs::symlink_metadata(&dir)
        .map_err(|e| format!("토렌트 삭제 기록 폴더 재확인 실패: {e}"))?;
    if !checked.is_dir() || checked.file_type().is_symlink() {
        return Err("토렌트 삭제 기록 폴더가 일반 디렉터리가 아닙니다".into());
    }
    Ok(dir)
}

fn intent_path(dir: &Path, intent: &RemovalIntent) -> PathBuf {
    dir.join(format!("{}-{}.json", intent.id, intent.info_hash))
}

/// Write the durable intent *before* attempting librqbit deletion. If a
/// crash interrupts an in-progress deletion, the next launch will suppress
/// only an exact-matching restored torrent, and never delete payload bytes.
pub(super) fn begin(
    persistence: &Path,
    handle: &ManagedTorrent,
) -> Result<(RemovalIntent, PathBuf), String> {
    begin_intent(persistence, RemovalIntent::from_handle(handle))
}

/// The same crash-durable write operation with an explicit identity; used by
/// the isolated process-restart test to reproduce an abrupt exit immediately
/// after the intent reached disk but before librqbit deleted any torrent.
pub(super) fn begin_intent(
    persistence: &Path,
    intent: RemovalIntent,
) -> Result<(RemovalIntent, PathBuf), String> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let dir = safe_journal_dir(persistence)?;
    let path = intent_path(&dir, &intent);
    if path.exists() {
        let recorded: RemovalIntent = serde_json::from_slice(
            &fs::read(&path).map_err(|e| format!("기존 삭제 기록 읽기 실패: {e}"))?,
        )
        .map_err(|e| format!("기존 삭제 기록 분석 실패: {e}"))?;
        if recorded == intent {
            return Ok((intent, path));
        }
        return Err("동일 ID의 기존 삭제 기록이 달라 안전하게 진행할 수 없습니다".into());
    }
    let temp = dir.join(format!(
        ".{}-{}-{}.tmp",
        intent.id,
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|e| format!("삭제 기록 임시 파일 생성 실패: {e}"))?;
        serde_json::to_writer(&mut file, &intent)
            .map_err(|e| format!("삭제 기록 직렬화 실패: {e}"))?;
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("삭제 기록 디스크 저장 실패: {e}"))?;
        drop(file);
        fs::rename(&temp, &path).map_err(|e| format!("삭제 기록 저장 실패: {e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map(|()| (intent, path))
}

/// Check the exact persisted torrent identity, not merely whether a numeric
/// ID exists: restored IDs can be reused for unrelated torrents.
pub(super) fn still_persisted(persistence: &Path, intent: &RemovalIntent) -> Result<bool, String> {
    let bytes = match fs::read(persistence.join("session.json")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("토렌트 세션 기록 읽기 실패: {e}")),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("토렌트 세션 기록 분석 실패: {e}"))?;
    let torrents = value
        .get("torrents")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "토렌트 세션 기록에 torrents 항목이 없습니다".to_string())?;
    Ok(torrents.get(&intent.id.to_string()).is_some_and(|value| {
        value
            .get("info_hash")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|hash| hash.eq_ignore_ascii_case(&intent.info_hash))
            && value
                .get("output_folder")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|folder| Path::new(folder) == intent.output_folder)
    }))
}

pub(super) fn finish(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("토렌트 삭제 기록 정리 실패: {e}")),
    }
}

pub(super) fn pending(persistence: &Path) -> Result<Vec<(RemovalIntent, PathBuf)>, String> {
    let dir = journal_dir(persistence);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let meta = fs::symlink_metadata(&dir).map_err(|e| format!("삭제 복구 폴더 확인 실패: {e}"))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("삭제 복구 폴더가 안전하지 않습니다".into());
    }
    let mut entries = Vec::new();
    for item in fs::read_dir(&dir).map_err(|e| format!("삭제 복구 폴더 읽기 실패: {e}"))?
    {
        let path = item
            .map_err(|e| format!("삭제 복구 항목 읽기 실패: {e}"))?
            .path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        if !fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
        {
            continue;
        }
        let intent: RemovalIntent = serde_json::from_slice(
            &fs::read(&path).map_err(|e| format!("삭제 복구 파일 읽기 실패: {e}"))?,
        )
        .map_err(|e| format!("삭제 복구 파일 분석 실패: {e}"))?;
        if intent_path(&dir, &intent) != path {
            return Err("삭제 복구 파일명과 내부 토렌트 ID가 일치하지 않습니다".into());
        }
        entries.push((intent, path));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(id: usize, hash: &str, dir: &Path) -> RemovalIntent {
        RemovalIntent {
            id,
            info_hash: hash.into(),
            output_folder: dir.join("one"),
        }
    }

    #[test]
    fn persistence_identity_match_never_confuses_reused_ids_or_different_hashes() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        let owned = intent(7, "aabbcc", folder);
        let unrelated = intent(7, "ddeeff", folder);
        let absent = intent(8, "aabbcc", folder);
        fs::write(
            folder.join("session.json"),
            serde_json::to_vec(&serde_json::json!({
                "torrents": {
                    "7": {"info_hash": "aabbcc", "output_folder": owned.output_folder},
                    "9": {"info_hash": "ddeeff", "output_folder": folder.join("other")}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(still_persisted(folder, &owned).unwrap());
        assert!(!still_persisted(folder, &unrelated).unwrap());
        assert!(!still_persisted(folder, &absent).unwrap());
        let same_id_and_hash_elsewhere = RemovalIntent {
            id: 7,
            info_hash: "aabbcc".into(),
            output_folder: folder.join("unrelated"),
        };
        assert!(
            !still_persisted(folder, &same_id_and_hash_elsewhere).unwrap(),
            "an unrelated destination with identical ID/hash must not be mistaken for a pending removal"
        );
        fs::write(folder.join("session.json"), b"invalid json").unwrap();
        assert!(still_persisted(folder, &owned).is_err());
    }

    #[test]
    fn pending_intents_require_exact_filename_and_valid_data() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        let dir = safe_journal_dir(folder).unwrap();
        let owned = intent(3, "012345", folder);
        let valid_path = intent_path(&dir, &owned);
        fs::write(&valid_path, serde_json::to_vec(&owned).unwrap()).unwrap();
        assert_eq!(
            pending(folder).unwrap(),
            vec![(owned.clone(), valid_path.clone())]
        );
        fs::write(
            dir.join("unexpected.json"),
            serde_json::to_vec(&owned).unwrap(),
        )
        .unwrap();
        assert!(pending(folder).is_err());
        fs::remove_file(dir.join("unexpected.json")).unwrap();
        finish(&valid_path).unwrap();
        finish(&valid_path).unwrap();
        assert!(pending(folder).unwrap().is_empty());
    }

    #[test]
    fn sixteen_concurrent_unique_intents_preserve_every_record_and_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        let barrier = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            let barrier = &barrier;
            for id in 0..16 {
                scope.spawn(move || {
                    barrier.wait();
                    let record = intent(id, &format!("{id:040x}"), folder);
                    let (saved, path) = begin_intent(folder, record.clone()).unwrap();
                    assert_eq!(saved, record);
                    assert!(path.is_file());
                });
            }
        });
        let mut records = pending(folder).unwrap();
        records.sort_by_key(|(intent, _)| intent.id);
        assert_eq!(records.len(), 16);
        for (id, (intent, path)) in records.into_iter().enumerate() {
            assert_eq!(intent.id, id);
            finish(&path).unwrap();
        }
        assert!(pending(folder).unwrap().is_empty());
        assert!(fs::read_dir(journal_dir(folder)).unwrap().all(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|ext| ext != "tmp")));
    }
}
