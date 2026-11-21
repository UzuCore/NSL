use anyhow::{Context, Result};
use librqbit::{
    api::TorrentIdOrHash, AddTorrent, AddTorrentOptions, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig, TorrentStatsState,
};

#[path = "torrent_removal.rs"]
mod removal_journal;

/// The handle type as re-exported by the crate root is unavailable in 9.1.0;
/// mirror its definition here.
type ManagedTorrentHandle = Arc<ManagedTorrent>;

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

// Test-only deterministic terminal watcher failure: it leaves librqbit's
// paused/resumable payload intact and exercises NSL's retry/cancel lifecycle.
// It does not claim to simulate a real librqbit storage error.
#[cfg(test)]
fn simulated_terminal_faults() -> &'static Mutex<std::collections::HashSet<(PathBuf, String)>> {
    static FAULTS: std::sync::OnceLock<Mutex<std::collections::HashSet<(PathBuf, String)>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Test-only forced `Session::delete` lookup failure, keyed to one disposable
/// torrent and armed for exactly one request. The actual torrent stays live.
#[cfg(test)]
fn simulated_delete_failures() -> &'static Mutex<std::collections::HashSet<(PathBuf, String)>> {
    static FAULTS: std::sync::OnceLock<Mutex<std::collections::HashSet<(PathBuf, String)>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// A real Windows rename denial armed *after* the normal persistence
/// preflight, to reproduce librqbit 9.1.0's silently swallowed JSON flush
/// failure. No production build can arm this scheduling hook.
#[cfg(all(test, windows))]
fn simulated_late_json_locks() -> &'static Mutex<std::collections::HashSet<(PathBuf, String)>> {
    static FAULTS: std::sync::OnceLock<Mutex<std::collections::HashSet<(PathBuf, String)>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// One tracked download. The engine thread updates its fields; the UI reads
/// them without blocking.
#[derive(Clone, Debug, Default)]
pub struct TorrentProgress {
    pub name: String,
    /// Actual resolved download directory, also stable after librqbit
    /// restores a job with a new runtime key on the next NSL launch.
    pub output_folder: Option<PathBuf>,
    pub total_bytes: u64,
    pub progress_bytes: u64,
    pub download_speed_bps: u64,
    /// Currently connected seeders; None if the live connection state is unavailable.
    pub active_seeders: Option<u32>,
    /// Currently connected non-seeding peers (excludes active_seeders).
    pub active_peers: Option<u32>,
    pub finished: bool,
    pub paused: bool,
    pub state: String,
    pub error: Option<String>,
}

pub(crate) fn retryable_failed_transfer(progress: &TorrentProgress) -> bool {
    progress.error.is_some() && !progress.finished && progress.output_folder.is_some()
}

/// Split rqbit's connected-peer total into seeders and non-seeding peers.
/// A paused, completed or failed torrent has no valid live connection count.
fn connected_peer_counts(live: Option<(u32, u32)>, inactive: bool) -> (Option<u32>, Option<u32>) {
    match (live, inactive) {
        (Some((total, seeders)), false) => {
            let seeders = seeders.min(total);
            (Some(seeders), Some(total - seeders))
        }
        _ => (None, None),
    }
}

/// `(job key, progress)` pairs as delivered to the UI.
pub type ProgressList = Vec<(String, TorrentProgress)>;
/// Keep user commands separate from actual transfer completion.
pub enum EngineNotice {
    Download {
        key: String,
        result: Result<(), String>,
    },
    Command {
        key: String,
        action: &'static str,
        result: Result<(), String>,
    },
}
pub type CompletionList = Vec<EngineNotice>;

/// One file from the validated torrent metadata, preserving its original ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentFileEntry {
    pub index: usize,
    pub path: PathBuf,
    pub size: u64,
}

/// Read-only torrent preview; the raw metadata is reused when starting.
#[derive(Debug)]
pub struct TorrentPreview {
    pub files: Vec<TorrentFileEntry>,
    pub torrent_bytes: Vec<u8>,
}

pub struct TorrentPreviewRequest {
    pub receiver: mpsc::Receiver<Result<TorrentPreview, String>>,
    cancel: Arc<AtomicBool>,
}

#[cfg(test)]
pub(crate) fn synthetic_preview_request() -> (
    TorrentPreviewRequest,
    mpsc::Sender<Result<TorrentPreview, String>>,
    Arc<AtomicBool>,
) {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    (
        TorrentPreviewRequest {
            receiver,
            cancel: cancel.clone(),
        },
        sender,
        cancel,
    )
}

impl Drop for TorrentPreviewRequest {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn safe_sub_folder(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn is_restorable_folder(output: &Path, default: &Path) -> bool {
    // A restored torrent may be deleted with its downloaded files. Only
    // restore dedicated direct children, never the downloads root itself,
    // a sibling, a nested path, or a symlink escaping the root. Incomplete
    // magnet downloads may not have created their child folder yet.
    let Some(std::path::Component::Normal(_)) = output.components().next_back() else {
        return false;
    };
    let (Ok(expected), Some(parent)) = (default.canonicalize(), output.parent()) else {
        return false;
    };
    if parent.canonicalize().ok().as_deref() != Some(expected.as_path()) {
        return false;
    }
    // Existing symlinks or junctions must resolve to a direct child; a
    // missing folder is also valid because rqbit creates it on demand.
    match std::fs::symlink_metadata(output) {
        Ok(_) => output
            .canonicalize()
            .ok()
            .is_some_and(|actual| actual.parent() == Some(expected.as_path())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Controls the engine executes against a torrent session.
pub enum EngineCommand {
    /// Add a new magnet download.
    Add(TorrentJob),
    /// Metadata-only preview; no file transfers.
    Preview {
        magnet: String,
        response: mpsc::Sender<Result<TorrentPreview, String>>,
        cancel: Arc<AtomicBool>,
    },
    /// Pause the torrent behind a progress key.
    Pause(String),
    /// Resume a previously paused torrent.
    Resume(String),
    /// Retry a terminal transfer error using its existing persisted torrent.
    Retry(String),
    /// Remove a torrent from the session. The flag controls whether the
    /// downloaded files are deleted along with it.
    Cancel(String, bool),
}

pub struct TorrentJob {
    pub key: String,
    pub magnet: String,
    /// Per-torrent sub-folder created under the session's default output
    /// folder, e.g. `downloads/Barbie Rewind/...`.
    pub sub_folder: PathBuf,
    pub torrent_bytes: Option<Vec<u8>>,
    pub only_files: Option<Vec<usize>>,
    pub cancel: Arc<AtomicBool>,
}

/// Owns the librqbit session on a dedicated tokio runtime. All public methods
/// are non-blocking so the egui UI never waits on the network.
pub struct TorrentEngine {
    command_tx: mpsc::Sender<EngineCommand>,
    progress: Arc<Mutex<HashMap<String, TorrentProgress>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    done_rx: Mutex<mpsc::Receiver<EngineNotice>>,
    counter: Arc<AtomicU64>,
}

fn spawn_engine(
    default_folder: PathBuf,
    command_rx: mpsc::Receiver<EngineCommand>,
    progress: Arc<Mutex<HashMap<String, TorrentProgress>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    handles: Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    done_tx: mpsc::Sender<EngineNotice>,
) -> Result<()> {
    let (ready_tx, ready_rx) = mpsc::sync_channel::<std::result::Result<(), String>>(1);
    std::thread::Builder::new()
        .name("torrent-engine".into())
        .spawn(move || -> anyhow::Result<()> {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error.to_string()));
                    return Err(error.into());
                }
            };
            // Session state lives in the app folder (not inside `downloads`)
            // so the user can wipe the whole download tree without losing
            // resume data, and without seeing dot-folders among game files.
            let persistence_folder = default_folder
                .parent()
                .map(|parent| parent.join("torrent-session"))
                .unwrap_or_else(|| default_folder.join(".rqbit-session"));
            #[allow(unused_mut)]
            let mut session_options = SessionOptions {
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(persistence_folder.clone()),
                }),
                ..Default::default()
            };
            // Every QA engine is loopback-only, including its listener. Never
            // expose even synthetic fixtures to LAN/public peers during tests.
            #[cfg(test)]
            {
                session_options.dht = None;
                session_options.disable_local_service_discovery = true;
                // Stage 21 isolated storage-fault QA: trigger the real
                // librqbit fastresume bitfield loading path in only a
                // disposable test folder. Production remains full-check by
                // default; do not weaken integrity checks for regular users.
                session_options.fastresume = default_folder.parent().is_some_and(|parent| {
                    parent.join(".stage21-enable-test-fastresume").is_file()
                });
                session_options.listen = Some(librqbit::ListenerOptions {
                    mode: librqbit::ListenerMode::TcpOnly,
                    listen_addr: "127.0.0.1:0".parse()?,
                    ..Default::default()
                });
            }
            let session = runtime.block_on(Session::new_with_opts(
                default_folder.clone(),
                session_options,
            ));
            let session = match session {
                Ok(session) => session,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("토렌트 세션 초기화 실패: {error:#}")));
                    return Err(error);
                }
            };
            let _ = ready_tx.send(Ok(()));
            // Reconcile any deletion that reached rqbit's in-memory removal
            // but failed to persist session.json (including a crash after
            // creating its durable intent). The identity check must match
            // id, info hash AND destination. Never delete payload files here.
            match removal_journal::pending(&persistence_folder) {
                Ok(pending) => {
                    for (intent, path) in pending {
                        let id = TorrentIdOrHash::Id(intent.id);
                        let registered = session.get(id);
                        if let Some(handle) = registered.as_ref() {
                            if !intent.matches_handle(handle) {
                                // The old id was reused for a different
                                // torrent: never remove the new owner's data.
                                let _ = removal_journal::finish(&path);
                                continue;
                            }
                            let _ = runtime.block_on(session.delete(id, false));
                        }
                        let removed = session.get(id).is_none();
                        match removal_journal::still_persisted(&persistence_folder, &intent) {
                            Ok(false) if removed => {
                                let _ = removal_journal::finish(&path);
                            }
                            state => {
                                if let Some(handle) = registered {
                                    // If recovery still cannot remove the
                                    // session, do not seed/download a job
                                    // the user explicitly asked to delete.
                                    if !removed {
                                        let _ = runtime.block_on(session.pause(&handle));
                                    }
                                }
                                let detail = match state {
                                    Err(error) => error,
                                    _ => "삭제된 토렌트의 세션 기록이 아직 남아 있습니다".into(),
                                };
                                let _ = done_tx.send(EngineNotice::Command {
                                    key: format!("r{}", intent.id),
                                    action: "remove",
                                    result: Err(format!(
                                        "토렌트 세션 복구 정리가 보류됐습니다: {detail}"
                                    )),
                                });
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = done_tx.send(EngineNotice::Command {
                        key: "session".into(),
                        action: "remove",
                        result: Err(format!("토렌트 삭제 복구 기록을 읽지 못했습니다: {error}")),
                    });
                }
            }
            // Torrents restored from the persisted session (a previous run)
            // resume downloading automatically but have no tracked job, so the
            // UI would show an empty download list. Adopt each of them into a
            // watch loop right away.
            let restored: Vec<ManagedTorrentHandle> = session
                .with_torrents(|torrents| torrents.map(|(_, handle)| handle.clone()).collect());
            let mut stale_ids: Vec<librqbit::api::TorrentIdOrHash> = Vec::new();
            for handle in restored {
                // A session saved by an older build may point at a scan folder
                // (or a since-deleted path). Drop those entries so the user can
                // re-download into the current downloads directory instead of
                // seeing a stale 100% row with no files behind it.
                let stale = !is_restorable_folder(handle.output_folder(), &default_folder);
                if stale {
                    stale_ids.push(librqbit::api::TorrentIdOrHash::Id(handle.id()));
                    continue;
                }
                let key = format!("r{}", handle.id());
                if let Ok(mut map) = handles.lock() {
                    map.insert(key.clone(), handle.clone());
                }
                let progress = progress.clone();
                let cancels = cancels.clone();
                let handles = handles.clone();
                let session = session.clone();
                let done_tx = done_tx.clone();
                runtime.spawn(async move {
                    let cancel = Arc::new(AtomicBool::new(false));
                    if let Some(result) = watch_torrent(
                        session,
                        key.clone(),
                        handle,
                        progress,
                        cancels,
                        handles,
                        cancel,
                    )
                    .await
                    {
                        let _ = done_tx.send(EngineNotice::Download { key, result });
                    }
                });
            }
            // Purge stale session entries (old output folders) off the runtime.
            if !stale_ids.is_empty() {
                let session = session.clone();
                runtime.spawn(async move {
                    for id in stale_ids {
                        let _ = session.delete(id, false).await;
                    }
                });
            }
            // `run_job` must be able to register handles too, so wrap the
            // shared maps once more for the loop below.
            let loop_handles = handles.clone();
            while let Ok(command) = command_rx.recv() {
                let session = session.clone();
                let progress = progress.clone();
                let cancels = cancels.clone();
                let handles = loop_handles.clone();
                let done_tx = done_tx.clone();
                let persistence_folder = persistence_folder.clone();
                runtime.spawn(async move {
                    let notice = match command {
                        EngineCommand::Add(job) => {
                            run_job(session, job, progress, cancels, handles).await
                        }
                        EngineCommand::Preview {
                            magnet,
                            response,
                            cancel,
                        } => {
                            let result = tokio::select! {
                                result = tokio::time::timeout(
                                    Duration::from_secs(90),
                                    preview_torrent(&session, &magnet),
                                ) => match result {
                                    Ok(result) => result.map_err(|error| format!("{error:#}")),
                                    Err(_) => Err("토렌트 파일 목록 조회 시간이 초과되었습니다".into()),
                                },
                                _ = async {
                                    while !cancel.load(Ordering::Relaxed) {
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                    }
                                } => return,
                            };
                            let _ = response.send(result);
                            None
                        }
                        EngineCommand::Pause(key) => {
                            let result = pause_torrent(&session, &handles, &key, true).await;
                            Some(EngineNotice::Command {
                                key,
                                action: "pause",
                                result,
                            })
                        }
                        EngineCommand::Resume(key) => {
                            let result = pause_torrent(&session, &handles, &key, false).await;
                            Some(EngineNotice::Command {
                                key,
                                action: "resume",
                                result,
                            })
                        }
                        EngineCommand::Retry(key) => {
                            let result = retry_failed_torrent(
                                session,
                                key.clone(),
                                progress,
                                cancels,
                                handles,
                                done_tx.clone(),
                            )
                            .await;
                            Some(EngineNotice::Command {
                                key,
                                action: "retry",
                                result,
                            })
                        }
                        EngineCommand::Cancel(key, delete_files) => {
                            // Keep the watcher and its progress visible until
                            // the session actually drops the torrent. A failed
                            // delete (e.g. a stale id) must not strand a
                            // cancelled but still-registered transfer.
                            let (result, session_removed) =
                                remove_torrent(
                                    &session,
                                    &handles,
                                    &key,
                                    delete_files,
                                    &persistence_folder,
                                )
                                .await;
                            if session_removed {
                                if let Ok(map) = cancels.lock() {
                                    if let Some(flag) = map.get(&key) {
                                        flag.store(true, Ordering::Release);
                                    }
                                }
                                if let Ok(mut map) = progress.lock() {
                                    map.remove(&key);
                                }
                                if let Ok(mut map) = cancels.lock() {
                                    map.remove(&key);
                                }
                                if let Ok(mut map) = handles.lock() {
                                    map.remove(&key);
                                }
                            }
                            Some(EngineNotice::Command {
                                key,
                                action: "remove",
                                result,
                            })
                        }
                    };
                    if let Some(notice) = notice {
                        let _ = done_tx.send(notice);
                    }
                });
            }
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!("토렌트 엔진 스레드를 시작하지 못했습니다: {e}"))?;
    let ready = ready_rx
        .recv_timeout(Duration::from_secs(30))
        .context("토렌트 엔진 초기화 응답 시간 초과")?;
    ready.map_err(anyhow::Error::msg)
}

async fn pause_torrent(
    session: &Arc<Session>,
    handles: &Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    key: &str,
    pause: bool,
) -> Result<(), String> {
    let handle = handles
        .lock()
        .ok()
        .and_then(|map| map.get(key).cloned())
        .ok_or_else(|| "해당 다운로드 항목을 찾을 수 없습니다".to_string())?;
    let outcome = if pause {
        session.pause(&handle).await
    } else {
        session.unpause(&handle).await
    };
    outcome.map_err(|e| format!("토렌트 상태 변경 실패: {e:#}"))
}

/// A failed transfer has no watch task anymore: merely unpausing its session
/// handle would leave the UI stuck on an old error forever. Reserve the key
/// before awaiting so repeated retry clicks cannot start multiple watchers.
async fn retry_failed_torrent(
    session: Arc<Session>,
    key: String,
    progress: Arc<Mutex<HashMap<String, TorrentProgress>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    handles: Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    done_tx: mpsc::Sender<EngineNotice>,
) -> Result<(), String> {
    let failed = progress.lock().ok().and_then(|map| map.get(&key).cloned());
    if !failed.is_some_and(|row| retryable_failed_transfer(&row)) {
        return Err(
            "실패한 전송 기록이 없습니다. 메타데이터 실패라면 스토어에서 다시 선택하세요".into(),
        );
    }
    let handle = handles
        .lock()
        .ok()
        .and_then(|map| map.get(&key).cloned())
        .ok_or_else(|| "복구할 토렌트 세션을 찾을 수 없습니다".to_owned())?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = cancels
            .lock()
            .map_err(|_| "토렌트 재시도 상태를 잠글 수 없습니다".to_owned())?;
        if active.contains_key(&key) {
            return Err("이 다운로드를 이미 복구 중입니다".into());
        }
        active.insert(key.clone(), cancel.clone());
    }
    // Delete may race with the queued retry before the storage is re-opened.
    // If the command is cancelled, do not touch a removed torrent handle.
    if cancel.load(Ordering::Acquire) {
        if let Ok(mut active) = cancels.lock() {
            if active
                .get(&key)
                .is_some_and(|flag| Arc::ptr_eq(flag, &cancel))
            {
                active.remove(&key);
            }
        }
        return Err("취소한 다운로드는 재시도할 수 없습니다".into());
    }
    if let Err(error) = session.unpause(&handle).await {
        if let Ok(mut active) = cancels.lock() {
            active.remove(&key);
        }
        return Err(format!("다운로드 재연결 실패: {error:#}"));
    }
    if cancel.load(Ordering::Relaxed) {
        if let Ok(mut active) = cancels.lock() {
            if active
                .get(&key)
                .is_some_and(|flag| Arc::ptr_eq(flag, &cancel))
            {
                active.remove(&key);
            }
        }
        return Err("취소된 다운로드는 재시도할 수 없습니다".into());
    }
    let updated = progress.lock().ok().is_some_and(|mut map| {
        if let Some(row) = map.get_mut(&key) {
            if row.error.is_some() && !row.finished {
                row.error = None;
                row.paused = false;
                row.state = "재연결 중".into();
                row.download_speed_bps = 0;
                return true;
            }
        }
        false
    });
    if !updated {
        if let Ok(mut active) = cancels.lock() {
            active.remove(&key);
        }
        return Err("재시도 중 다운로드 항목이 변경되었습니다".into());
    }
    tokio::spawn(async move {
        if let Some(result) = watch_torrent(
            session,
            key.clone(),
            handle,
            progress,
            cancels,
            handles,
            cancel,
        )
        .await
        {
            let _ = done_tx.send(EngineNotice::Download { key, result });
        }
    });
    Ok(())
}

/// File removal in librqbit 9.1.0 is best-effort and can quietly ignore
/// Windows sharing violations. Test DELETE access without changing any file
/// before dropping the torrent from its resumable session. Files may still be
/// locked between this check and deletion; the post-check handles that race.
#[cfg(windows)]
fn preflight_file_deletion(paths: &[PathBuf]) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    for path in paths {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("삭제할 파일 상태를 확인하지 못했습니다: {error}")),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("파일 링크가 포함돼 안전하게 삭제할 수 없습니다".into());
            }
            Ok(_) => {}
        }
        // DELETE access (0x10000) and FILE_SHARE_READ|WRITE|DELETE (0x7).
        // An open handle that denies delete or an ACL restriction produces
        // an error without ever modifying the target file.
        std::fs::OpenOptions::new()
            .access_mode(0x0001_0000)
            .share_mode(0x1 | 0x2 | 0x4)
            .open(path)
            .map_err(|error| {
                format!(
                    "파일이 사용 중이거나 삭제 권한이 없습니다. 잠금 해제 후 다시 삭제하세요 ({}): {error}",
                    path.display()
                )
            })?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn preflight_file_deletion(_paths: &[PathBuf]) -> Result<(), String> {
    Ok(())
}

/// A locked or read-only session.json can make librqbit's persistence flush
/// fail silently *after* removing the live session entry. Refuse deletion
/// before changing any session or file data when that known condition exists.
#[cfg(windows)]
fn preflight_session_persistence(folder: &Path) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    let db = folder.join("session.json");
    let metadata = match std::fs::symlink_metadata(&db) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("토렌트 세션 기록을 확인하지 못했습니다: {error}")),
    };
    if !metadata.is_file() || metadata.permissions().readonly() {
        return Err("토렌트 세션 기록이 읽기 전용이거나 일반 파일이 아닙니다".into());
    }
    std::fs::OpenOptions::new()
        .access_mode(0x0001_0000)
        .share_mode(0x1 | 0x2 | 0x4)
        .open(&db)
        .map_err(|error| {
            format!("세션 기록 파일이 사용 중입니다. 잠금 해제 후 다시 삭제하세요: {error}")
        })?;
    Ok(())
}

#[cfg(not(windows))]
fn preflight_session_persistence(_folder: &Path) -> Result<(), String> {
    Ok(())
}

async fn remove_torrent(
    session: &Arc<Session>,
    handles: &Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    key: &str,
    delete_files: bool,
    persistence_folder: &Path,
) -> (Result<(), String>, bool) {
    // Deleting files requires the handle: rqbit removes the torrent's output
    // tree from the handle's resolved folder.
    let handle = handles.lock().ok().and_then(|map| map.get(key).cloned());
    let id = match &handle {
        Some(handle) => handle.id(),
        None => {
            // A magnet can still be fetching metadata: the shared cancel flag
            // stops its future, with no managed torrent to delete yet.
            if key.starts_with('t') {
                return (Ok(()), true);
            }
            // Restored torrents adopted as r{session id}; parse the id back.
            match key
                .strip_prefix('r')
                .and_then(|id| id.parse::<usize>().ok())
            {
                Some(id) => id,
                None => return (Err("해당 다운로드 항목을 찾을 수 없습니다".into()), false),
            }
        }
    };
    if let Err(error) = preflight_session_persistence(persistence_folder) {
        return (Err(error), false);
    }
    // Snapshot only validated torrent-owned file paths. librqbit 9.1.0 logs
    // individual remove_file failures but may still return Ok; verify those
    // outputs afterward before claiming that file deletion succeeded.
    let delete_paths = if delete_files {
        let owned_handle = handle
            .clone()
            .or_else(|| session.get(TorrentIdOrHash::Id(id)));
        let Some(owned_handle) = owned_handle else {
            return (
                Err("삭제할 토렌트 파일 목록을 확인하지 못했습니다".into()),
                false,
            );
        };
        match owned_handle.with_metadata(|metadata| {
            metadata
                .file_infos
                .iter()
                .filter(|info| !info.attrs.padding)
                .map(|info| info.relative_filename.clone())
                .collect::<Vec<_>>()
        }) {
            Ok(relative_paths)
                if relative_paths.iter().all(|relative| {
                    relative
                        .components()
                        .all(|component| matches!(component, Component::Normal(_)))
                }) =>
            {
                relative_paths
                    .into_iter()
                    .map(|relative| owned_handle.output_folder().join(relative))
                    .collect::<Vec<_>>()
            }
            _ => {
                return (
                    Err(
                        "토렌트 삭제를 중단했습니다: 안전한 파일 목록을 확인하지 못했습니다".into(),
                    ),
                    false,
                );
            }
        }
    } else {
        Vec::new()
    };
    if let Err(error) = preflight_file_deletion(&delete_paths) {
        // No session mutation took place, so the current watcher and its
        // persisted download stay usable. The user can release the lock and
        // retry the same deletion without losing completed pieces.
        return (Err(error), false);
    }
    let owned_for_intent = handle
        .clone()
        .or_else(|| session.get(TorrentIdOrHash::Id(id)));
    let Some(owned_for_intent) = owned_for_intent else {
        return (Err("삭제할 토렌트 세션을 찾을 수 없습니다".into()), false);
    };
    let (intent, intent_path) = match removal_journal::begin(persistence_folder, &owned_for_intent)
    {
        Ok(record) => record,
        Err(error) => return (Err(error), false),
    };
    // Fault injection calls the real librqbit delete API with a guaranteed
    // absent identifier. This tests a genuine delete error without ever
    // modifying the live synthetic transfer or its output data.
    #[cfg(test)]
    let requested_id = if handle.as_ref().is_some_and(|handle| {
        simulated_delete_failures().lock().is_ok_and(|mut faults| {
            faults.remove(&(handle.output_folder().to_path_buf(), key.to_owned()))
        })
    }) {
        usize::MAX
    } else {
        id
    };
    #[cfg(not(test))]
    let requested_id = id;
    // The test-only hook introduces an actual Windows sharing violation
    // after the preflight, exercising rqbit's swallowed JSON rename error.
    #[cfg(all(test, windows))]
    let late_json_guard = if simulated_late_json_locks().lock().is_ok_and(|mut faults| {
        faults.remove(&(
            owned_for_intent.output_folder().to_path_buf(),
            key.to_owned(),
        ))
    }) {
        use std::os::windows::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(persistence_folder.join("session.json"))
        {
            Ok(guard) => Some(guard),
            Err(error) => {
                let _ = removal_journal::finish(&intent_path);
                return (Err(format!("Stage 21 fault setup failed: {error}")), false);
            }
        }
    } else {
        None
    };
    // When delete_files is false the downloaded bytes stay on disk.
    let result = session
        .delete(TorrentIdOrHash::Id(requested_id), delete_files)
        .await;
    #[cfg(all(test, windows))]
    drop(late_json_guard);
    // librqbit can remove a torrent from the session before encountering an
    // error in file cleanup. Reconcile against the real session, not merely
    // the delete() result, to prevent a dead row with a stale handle.
    let session_removed = session.get(TorrentIdOrHash::Id(id)).is_none();
    let mut result = result.map_err(|error| {
        if session_removed && delete_files {
            format!(
                "토렌트 세션은 제거됐지만 파일 정리를 완료하지 못했습니다. 다운로드 폴더에 남은 파일을 확인하세요: {error:#}"
            )
        } else {
            format!("토렌트 제거 실패: {error:#}")
        }
    });
    if session_removed {
        match removal_journal::still_persisted(persistence_folder, &intent) {
            Ok(true) => {
                result = Err(
                    "토렌트는 제거됐지만 세션 기록 저장에 실패했습니다. 다음 NSL 실행에서 삭제 기록을 자동 정리합니다".into(),
                );
            }
            Ok(false) => {
                if let Err(error) = removal_journal::finish(&intent_path) {
                    result = Err(error);
                }
            }
            Err(error) => {
                result = Err(format!(
                    "토렌트는 제거됐지만 세션 저장을 확인하지 못했습니다. 다음 실행에서 복구합니다: {error}"
                ));
            }
        }
    } else if let Err(error) = removal_journal::finish(&intent_path) {
        result = Err(format!("토렌트 제거 실패 후 삭제 기록 정리 실패: {error}"));
    }
    if session_removed && delete_files && result.is_ok() {
        let remaining = delete_paths
            .iter()
            .filter(|path| match std::fs::symlink_metadata(path) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => true, // Unknown I/O status must not become a false success.
            })
            .count();
        if remaining > 0 {
            result = Err(format!(
                "토렌트 세션은 제거됐지만 {remaining}개 파일이 남았습니다. 다운로드 폴더를 확인하세요"
            ));
        }
    }
    (result, session_removed)
}

/// Watch one torrent (restored or newly added) and mirror its progress into
/// the shared map. Exits when the torrent finishes, errors, or the cancel flag
/// is set by an engine command.
async fn watch_torrent(
    session: Arc<Session>,
    key: String,
    handle: ManagedTorrentHandle,
    progress: Arc<Mutex<HashMap<String, TorrentProgress>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    handles: Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    cancel: Arc<AtomicBool>,
) -> Option<Result<(), String>> {
    if let Ok(mut map) = handles.lock() {
        map.insert(key.clone(), handle.clone());
    }
    if let Ok(mut map) = cancels.lock() {
        map.insert(key.clone(), cancel.clone());
    }
    let name = handle.name().unwrap_or_default();
    let mut no_peers_since: Option<std::time::Instant> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            // Cancel semantics: the entry disappears from the list; downloaded
            // bytes stay on disk.
            if let Ok(mut map) = progress.lock() {
                map.remove(&key);
            }
            if let Ok(mut map) = cancels.lock() {
                map.remove(&key);
            }
            // Do not remove the managed handle here: the Cancel command still
            // needs its session id to delete persistence and optional files.
            return None;
        }
        #[cfg(test)]
        if simulated_terminal_faults().lock().is_ok_and(|mut faults| {
            faults.remove(&(handle.output_folder().to_path_buf(), key.clone()))
        }) {
            // Pause the real torrent so already verified pieces are retained;
            // deliberately end only the NSL watcher with a terminal failure.
            let error = "STAGE19_TEST_TERMINAL_WATCHER_FAILURE".to_owned();
            let _ = session.pause(&handle).await;
            if let Ok(mut map) = progress.lock() {
                if let Some(row) = map.get_mut(&key) {
                    row.error = Some(error.clone());
                    row.paused = false;
                    row.state = "실패".into();
                    row.download_speed_bps = 0;
                }
            }
            if let Ok(mut map) = cancels.lock() {
                map.remove(&key);
            }
            return Some(Err(error));
        }
        let stats = handle.stats();
        let paused = handle.is_paused();
        // rqbit's own speed estimator uses a moving average, which reads much
        // smoother than per-sample deltas (that oscillated 0 ↔ burst).
        let live_speed_bps = stats
            .live
            .as_ref()
            .map(|live| live.download_speed.as_bytes())
            .unwrap_or(0);
        let (active_seeders, active_peers) = connected_peer_counts(
            stats.live.as_ref().map(|live| {
                (
                    live.snapshot.peer_stats.live,
                    live.snapshot.peer_stats.live_seeders,
                )
            }),
            paused || stats.finished || stats.error.is_some(),
        );
        // Zero *connected* peers does not prove zero available seeds in the
        // swarm. Show an informational state only after sustained inactivity.
        let no_live_connections = !paused
            && !stats.finished
            && stats.error.is_none()
            && stats.total_bytes > stats.progress_bytes
            && live_speed_bps == 0
            && active_seeders == Some(0)
            && active_peers == Some(0);
        if no_live_connections {
            no_peers_since.get_or_insert_with(std::time::Instant::now);
        } else {
            no_peers_since = None;
        }
        let prolonged_wait =
            no_peers_since.is_some_and(|since| since.elapsed() >= Duration::from_secs(15));
        let update = TorrentProgress {
            name: name.to_owned(),
            output_folder: Some(handle.output_folder().to_path_buf()),
            total_bytes: stats.total_bytes,
            progress_bytes: stats.progress_bytes,
            active_seeders,
            active_peers,
            finished: stats.finished,
            paused,
            state: if paused {
                "일시정지".into()
            } else {
                match stats.state {
                    TorrentStatsState::Initializing { .. } => "초기화".into(),
                    TorrentStatsState::Live if prolonged_wait => {
                        "연결된 시더·피어 없음 · 재연결 대기".into()
                    }
                    TorrentStatsState::Live => "다운로드".into(),
                    TorrentStatsState::Paused => "일시정지".into(),
                    TorrentStatsState::Error => "오류".into(),
                }
            },
            error: stats.error.clone(),
            download_speed_bps: if paused { 0 } else { live_speed_bps },
        };
        let completed = update.finished;
        let error = update.error.clone();
        if let Ok(mut map) = progress.lock() {
            // A successful Cancel can remove the row while this watcher is
            // awaiting its next sample. Never resurrect a deleted transfer.
            if !cancel.load(Ordering::Acquire) {
                map.insert(key.clone(), update);
            }
        }
        if completed || error.is_some() {
            // Release the file locks so emulators can open the freshly
            // downloaded files immediately (rqbit keeps them open for seeding
            // otherwise, and the emulator then fails with permission denied).
            let pause_result = session.pause(&handle).await;
            if let Ok(mut map) = cancels.lock() {
                map.remove(&key);
            }
            return Some(match error {
                Some(error) => Err(error),
                None => pause_result
                    .map_err(|e| format!("완료된 토렌트를 일시정지하지 못했습니다: {e:#}")),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn run_job(
    session: Arc<Session>,
    job: TorrentJob,
    progress: Arc<Mutex<HashMap<String, TorrentProgress>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    handles: Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
) -> Option<EngineNotice> {
    let key = job.key.clone();
    let added = tokio::select! {
        result = add_torrent(&session, &job) => result,
        _ = async {
            while !job.cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } => return None,
    };
    let result = match added {
        Ok(handle) => {
            if job.cancel.load(Ordering::Relaxed) {
                return session
                    .delete(TorrentIdOrHash::Id(handle.id()), false)
                    .await
                    .err()
                    .map(|error| EngineNotice::Download {
                        key,
                        result: Err(format!("취소한 다운로드의 세션 정리 실패: {error:#}")),
                    });
            }
            // Hand the torrent over to the shared watch loop so pause/resume
            // and cancel keep working after the magnet handshake.
            watch_torrent(
                session,
                key.clone(),
                handle,
                progress,
                cancels,
                handles,
                job.cancel,
            )
            .await
        }
        Err(error) => {
            let text = format!("{error:#}");
            let mut final_state = progress
                .lock()
                .ok()
                .and_then(|map| map.get(&key).cloned())
                .unwrap_or_default();
            final_state.state = "실패".into();
            final_state.error = Some(text.clone());
            if let Ok(mut map) = progress.lock() {
                map.insert(key.clone(), final_state);
            }
            Some(Err(text))
        }
    };
    result.map(|result| EngineNotice::Download { key, result })
}

async fn preview_torrent(session: &Arc<Session>, magnet: &str) -> Result<TorrentPreview> {
    let response = session
        .add_torrent(
            AddTorrent::from_url(magnet.to_owned()),
            Some(AddTorrentOptions {
                list_only: true,
                ..Default::default()
            }),
        )
        .await
        .context("토렌트 파일 목록을 가져오지 못했습니다")?;
    let librqbit::AddTorrentResponse::ListOnly(info) = response else {
        anyhow::bail!("다운로드 없이 파일 목록을 확인할 수 없습니다");
    };
    anyhow::ensure!(
        info.torrent_bytes.len() <= 64 * 1024 * 1024,
        "토렌트 메타데이터가 64MB를 초과했습니다"
    );
    let mut files = Vec::new();
    for (index, file) in info.info.iter_file_details().enumerate() {
        anyhow::ensure!(index < 100_000, "토렌트 파일이 너무 많습니다");
        let path = file.filename.to_pathbuf();
        anyhow::ensure!(
            !path.as_os_str().is_empty()
                && path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "안전하지 않은 토렌트 파일 경로: {}",
            path.display()
        );
        files.push(TorrentFileEntry {
            index,
            path,
            size: file.len,
        });
    }
    anyhow::ensure!(!files.is_empty(), "이 토렌트에 다운로드할 파일이 없습니다");
    Ok(TorrentPreview {
        files,
        torrent_bytes: info.torrent_bytes.to_vec(),
    })
}

async fn add_torrent(
    session: &Arc<Session>,
    job: &TorrentJob,
) -> anyhow::Result<ManagedTorrentHandle> {
    // Every torrent lands in its own sub-folder under the session's default
    // output folder (`downloads`), so single-file and multi-file releases stay
    // separated per game instead of mixing into one flat directory.
    let opts = AddTorrentOptions {
        sub_folder: Some(job.sub_folder.to_string_lossy().into_owned()),
        only_files: job.only_files.clone(),
        // Metadata returned by a magnet peer need not carry the original
        // magnet's tracker URLs. Restore them when starting from the cached
        // .torrent bytes, otherwise tracker-only/private torrents can stall.
        trackers: librqbit::Magnet::parse(&job.magnet)
            .ok()
            .map(|magnet| magnet.trackers),
        overwrite: true,
        ..Default::default()
    };
    let torrent = match &job.torrent_bytes {
        Some(bytes) => AddTorrent::from_bytes(bytes.clone()),
        None => AddTorrent::from_url(job.magnet.clone()),
    };
    let handle = session
        .add_torrent(torrent, Some(opts))
        .await
        .and_then(|response| {
            response
                .into_handle()
                .context("이 토렌트는 파일 목록만 조회할 수 있습니다")
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(expected) = &job.only_files {
        anyhow::ensure!(
            handle.only_files().as_ref() == Some(expected),
            "토렌트 엔진에 선택된 파일 목록이 적용되지 않았습니다"
        );
    }
    Ok(handle)
}

impl TorrentEngine {
    /// Where the engine stores its resume state (`session.json` + torrent
    /// metadata). Exposed so the settings UI can offer to clear it.
    pub fn session_state_path(default_folder: &std::path::Path) -> PathBuf {
        default_folder
            .parent()
            .map(|parent| parent.join("torrent-session"))
            .unwrap_or_else(|| default_folder.join(".rqbit-session"))
    }

    /// Remove all session state (resume data, cached torrent metadata).
    /// Downloads already on disk are untouched. Returns the number of bytes
    /// freed. Only works when the engine is idle enough to not race with new
    /// writes; called from the settings screen before any torrent is added.
    pub fn clear_session_state(default_folder: &std::path::Path) -> Result<u64> {
        let path = Self::session_state_path(default_folder);
        if !path.exists() {
            return Ok(0);
        }
        let mut freed = 0u64;
        for entry in walkdir::WalkDir::new(&path)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                freed += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        std::fs::remove_dir_all(&path)
            .map_err(|e| anyhow::anyhow!("세션 데이터 삭제 실패: {e}"))?;
        Ok(freed)
    }

    pub fn new(default_folder: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&default_folder)?;
        let (command_tx, command_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let progress: Arc<Mutex<HashMap<String, TorrentProgress>>> = Arc::default();
        let cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> = Arc::default();
        let handles: Arc<Mutex<HashMap<String, ManagedTorrentHandle>>> = Arc::default();
        spawn_engine(
            default_folder,
            command_rx,
            progress.clone(),
            cancels.clone(),
            handles.clone(),
            done_tx,
        )?;
        Ok(Self {
            command_tx,
            progress,
            cancels,
            done_rx: Mutex::new(done_rx),
            counter: Arc::new(AtomicU64::new(1)),
        })
    }

    #[cfg(test)]
    fn inject_terminal_watcher_failure(&self, key: &str) -> Result<()> {
        let output = self
            .progress
            .lock()
            .ok()
            .and_then(|jobs| jobs.get(key).cloned())
            .filter(|job| !job.finished && job.error.is_none() && job.progress_bytes > 0)
            .and_then(|job| job.output_folder)
            .context("Stage 19 fault injection requires a live partially downloaded torrent")?;
        simulated_terminal_faults()
            .lock()
            .map_err(|_| anyhow::anyhow!("Stage 19 test fault map unavailable"))?
            .insert((output, key.to_owned()));
        Ok(())
    }

    #[cfg(test)]
    fn inject_session_delete_failure(&self, key: &str) -> Result<()> {
        let output = self
            .progress
            .lock()
            .ok()
            .and_then(|jobs| jobs.get(key).cloned())
            .filter(|job| !job.finished && job.error.is_none() && job.progress_bytes > 0)
            .and_then(|job| job.output_folder)
            .context("Stage 20 delete fault needs an active partially downloaded torrent")?;
        simulated_delete_failures()
            .lock()
            .map_err(|_| anyhow::anyhow!("Stage 20 test delete fault map unavailable"))?
            .insert((output, key.to_owned()));
        Ok(())
    }

    #[cfg(all(test, windows))]
    fn inject_late_session_json_rename_denial(&self, key: &str) -> Result<()> {
        let output = self
            .progress
            .lock()
            .ok()
            .and_then(|jobs| jobs.get(key).cloned())
            .filter(|job| job.output_folder.is_some())
            .and_then(|job| job.output_folder)
            .context("Stage 21 late JSON fault requires a managed torrent")?;
        simulated_late_json_locks()
            .lock()
            .map_err(|_| anyhow::anyhow!("Stage 21 fault map unavailable"))?
            .insert((output, key.to_owned()));
        Ok(())
    }

    /// Queue a magnet for download. Returns the progress key.
    #[cfg(test)]
    pub fn start_download(&self, label: &str, magnet: &str, sub_folder: PathBuf) -> Result<String> {
        self.start_download_selected(label, magnet, sub_folder, None, None)
    }

    /// Resolve the magnet metadata without starting a download. The receiver
    /// is polled by egui; dropping the request cancels metadata resolution.
    pub fn preview_files(&self, magnet: &str) -> Result<TorrentPreviewRequest> {
        anyhow::ensure!(
            magnet.starts_with("magnet:?"),
            "올바른 마그넷 링크가 아닙니다"
        );
        let (response, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.command_tx
            .send(EngineCommand::Preview {
                magnet: magnet.to_owned(),
                response,
                cancel: cancel.clone(),
            })
            .map_err(|_| anyhow::anyhow!("토렌트 엔진이 중지되었습니다"))?;
        Ok(TorrentPreviewRequest { receiver, cancel })
    }

    /// Start exactly the files chosen in the metadata preview. The original
    /// torrent bytes let rqbit apply only_files BEFORE creating any payloads,
    /// rather than downloading all files and filtering after transfer.
    pub fn start_download_selected(
        &self,
        label: &str,
        magnet: &str,
        sub_folder: PathBuf,
        torrent_bytes: Option<Vec<u8>>,
        only_files: Option<Vec<usize>>,
    ) -> Result<String> {
        anyhow::ensure!(
            safe_sub_folder(&sub_folder),
            "다운로드 하위 폴더가 안전하지 않습니다"
        );
        anyhow::ensure!(
            magnet.starts_with("magnet:?"),
            "올바른 마그넷 링크가 아닙니다"
        );
        if let Some(indices) = &only_files {
            anyhow::ensure!(!indices.is_empty(), "파일을 하나 이상 선택하세요");
            let unique = indices
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            anyhow::ensure!(
                indices.len() == unique.len(),
                "중복된 토렌트 파일 선택입니다"
            );
            anyhow::ensure!(
                torrent_bytes.is_some(),
                "파일별 다운로드에는 검증된 토렌트 메타데이터가 필요합니다"
            );
        }
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let key = format!("t{seq}");
        let job = TorrentJob {
            key: key.clone(),
            magnet: magnet.to_owned(),
            sub_folder,
            torrent_bytes,
            only_files,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.insert(key.clone(), job.cancel.clone());
        }
        if let Ok(mut map) = self.progress.lock() {
            // Show the entry immediately with the human-readable game title,
            // before any peer connects. The engine overwrites `name` from the
            // torrent metadata, but the catalog title is friendlier, so it is
            // only used as the initial placeholder.
            map.insert(
                key.clone(),
                TorrentProgress {
                    name: label.to_owned(),
                    state: "메타데이터 대기".into(),
                    ..Default::default()
                },
            );
        }
        if self.command_tx.send(EngineCommand::Add(job)).is_err() {
            if let Ok(mut map) = self.cancels.lock() {
                map.remove(&key);
            }
            if let Ok(mut map) = self.progress.lock() {
                map.remove(&key);
            }
            anyhow::bail!("토렌트 엔진이 중지되었습니다");
        }
        Ok(key)
    }

    /// Pause a running download. Downloaded bytes are kept.
    pub fn pause(&self, key: &str) {
        let _ = self.command_tx.send(EngineCommand::Pause(key.to_owned()));
    }

    /// Resume a paused download.
    pub fn resume(&self, key: &str) {
        let _ = self.command_tx.send(EngineCommand::Resume(key.to_owned()));
    }

    /// Retry a failed transfer only when its managed torrent still exists.
    /// Metadata-only failures require a fresh selection from the store.
    pub fn retry(&self, key: &str) {
        let _ = self.command_tx.send(EngineCommand::Retry(key.to_owned()));
    }

    /// Ask a running job to stop. The engine removes the torrent from the
    /// session; when `delete_files` is set the downloaded files are deleted
    /// too. The entry disappears from the list either way.
    pub fn cancel(&self, key: &str, delete_files: bool) {
        let _ = self
            .command_tx
            .send(EngineCommand::Cancel(key.to_owned(), delete_files));
    }

    /// Remove a finished/failed entry from the visible list only.
    pub fn dismiss(&self, key: &str) {
        // Remove persistence too, otherwise dismissed jobs return on restart.
        self.cancel(key, false);
    }

    /// Drain completion events and snapshot live progress for the UI.
    /// Returns the finished jobs since the last poll so the caller can log
    /// them (the entries stay in the progress map until dismissed).
    pub fn poll(&self) -> (ProgressList, CompletionList) {
        let mut completions = Vec::new();
        loop {
            let received = self.done_rx.lock().ok().and_then(|rx| rx.try_recv().ok());
            match received {
                Some(event) => completions.push(event),
                None => break,
            }
        }
        let snapshot = self
            .progress
            .lock()
            .map(|map| {
                let mut items: Vec<(String, TorrentProgress)> =
                    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                items.sort_by(|a, b| a.0.cmp(&b.0));
                items
            })
            .unwrap_or_default();
        (snapshot, completions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_defaults_are_sane() {
        let p = TorrentProgress::default();
        assert!(!p.finished);
        assert_eq!(p.total_bytes, 0);
        assert!(p.error.is_none());
        assert_eq!((p.active_seeders, p.active_peers), (None, None));
        assert!(!retryable_failed_transfer(&p));
    }

    #[test]
    fn retry_requires_a_failed_transfer_with_a_persisted_session() {
        let mut p = TorrentProgress {
            output_folder: Some(PathBuf::from("downloads/session")),
            error: Some("temporary storage error".into()),
            ..Default::default()
        };
        assert!(retryable_failed_transfer(&p));
        p.finished = true;
        assert!(!retryable_failed_transfer(&p));
        p.finished = false;
        p.output_folder = None;
        assert!(!retryable_failed_transfer(&p));
        p.output_folder = Some(PathBuf::from("downloads/session"));
        p.error = None;
        assert!(!retryable_failed_transfer(&p));
    }

    #[test]
    fn peer_counts_are_live_per_torrent_nonseeding_and_not_invented_when_idle() {
        assert_eq!(
            connected_peer_counts(Some((8, 3)), false),
            (Some(3), Some(5))
        );
        assert_eq!(
            connected_peer_counts(Some((0, 0)), false),
            (Some(0), Some(0))
        );
        assert_eq!(
            connected_peer_counts(Some((2, 5)), false),
            (Some(2), Some(0))
        );
        assert_eq!(connected_peer_counts(None, false), (None, None));
        assert_eq!(connected_peer_counts(Some((8, 3)), true), (None, None));
    }

    #[test]
    fn speed_computation_never_divides_by_zero() {
        // Regression: `delta / elapsed.max(0.001) as u64` truncated the
        // sub-second elapsed time (0.5s) to 0 and panicked with a division
        // by zero as soon as the first speed sample was due. The fixed
        // formula divides in f64 space, so any positive elapsed time yields
        // a finite speed.
        let compute =
            |delta: u64, elapsed: f64| -> u64 { (delta as f64 / elapsed.max(0.001)) as u64 };
        assert_eq!(compute(1_000_000, 0.5), 2_000_000);
        assert_eq!(compute(0, 0.5), 0);
        assert_eq!(compute(500, 2.0), 250);
        // A sub-millisecond elapsed value must not blow up either.
        assert!(compute(1, 0.0000001) > 0);
    }

    #[test]
    fn resume_accepts_children_but_rejects_external_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("downloads");
        let child = root.join("BASE-HASH");
        let outside = temp.path().join("unrelated");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        assert!(!is_restorable_folder(&root, &root));
        assert!(is_restorable_folder(&child, &root));
        assert!(!is_restorable_folder(&outside, &root));
        let grandchild = child.join("nested");
        std::fs::create_dir(&grandchild).unwrap();
        assert!(!is_restorable_folder(&grandchild, &root));
        assert!(is_restorable_folder(&root.join("missing"), &root));
        assert!(!is_restorable_folder(
            &root.join("..").join("unrelated"),
            &root
        ));
        #[cfg(windows)]
        {
            let link = root.join("link-out");
            if std::os::windows::fs::symlink_dir(&outside, &link).is_ok() {
                assert!(!is_restorable_folder(&link, &root));
            }
        }
        assert!(safe_sub_folder(Path::new("BASE-HASH")));
        assert!(!safe_sub_folder(Path::new("../escape")));
        assert!(!safe_sub_folder(&child));
    }

    #[test]
    fn unsafe_download_inputs_never_create_jobs_or_files() {
        let root = tempfile::tempdir().unwrap();
        let engine = TorrentEngine::new(root.path().join("downloads")).unwrap();
        for (magnet, folder) in [
            ("https://invalid.example/file", PathBuf::from("valid")),
            ("magnet:?xt=urn:btih:bad", PathBuf::from("../escape")),
            ("magnet:?xt=urn:btih:bad", root.path().join("escape")),
        ] {
            assert!(engine.start_download("invalid", magnet, folder).is_err());
        }
        let (progress, notices) = engine.poll();
        assert!(progress.is_empty());
        assert!(notices.is_empty());
        assert!(!root.path().join("escape").exists());
    }

    #[test]
    fn rejected_magnet_can_be_dismissed_without_deleting_other_files() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        let engine = TorrentEngine::new(downloads.clone()).unwrap();
        let sentinel = downloads.join("keep.bin");
        std::fs::write(&sentinel, b"keep this").unwrap();
        let key = engine
            .start_download("bad", "magnet:?xt=urn:btih:invalid", PathBuf::from("case"))
            .unwrap();
        let start = std::time::Instant::now();
        loop {
            let (items, notices) = engine.poll();
            if !notices.is_empty() {
                assert!(items
                    .iter()
                    .any(|(id, item)| id == &key && item.error.is_some()));
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(12),
                "invalid magnet hung"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        engine.dismiss(&key);
        let start = std::time::Instant::now();
        loop {
            if engine.poll().0.iter().all(|(id, _)| id != &key) {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(12), "dismiss hung");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep this");
    }
}

#[cfg(test)]
#[path = "torrent_e2e_tests.rs"]
mod localhost_tests;
