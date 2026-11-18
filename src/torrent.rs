use anyhow::{Context, Result};
use librqbit::{
    api::TorrentIdOrHash, AddTorrent, AddTorrentOptions, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig, TorrentStatsState,
};

/// The handle type as re-exported by the crate root is unavailable in 9.1.0;
/// mirror its definition here.
type ManagedTorrentHandle = Arc<ManagedTorrent>;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

/// One tracked download. The engine thread updates its fields; the UI reads
/// them without blocking.
#[derive(Clone, Debug, Default)]
pub struct TorrentProgress {
    pub name: String,
    pub total_bytes: u64,
    pub progress_bytes: u64,
    pub download_speed_bps: u64,
    pub finished: bool,
    pub paused: bool,
    pub state: String,
    pub error: Option<String>,
}

/// `(job key, progress)` pairs as delivered to the UI.
pub type ProgressList = Vec<(String, TorrentProgress)>;
/// `(job key, final result)` for jobs that finished since the last poll.
pub type CompletionList = Vec<(String, anyhow::Result<(), String>)>;

/// Controls the engine executes against a torrent session.
pub enum EngineCommand {
    /// Add a new magnet download.
    Add(TorrentJob),
    /// Pause the torrent behind a progress key.
    Pause(String),
    /// Resume a previously paused torrent.
    Resume(String),
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
    pub cancel: Arc<AtomicBool>,
}

/// Owns the librqbit session on a dedicated tokio runtime. All public methods
/// are non-blocking so the egui UI never waits on the network.
pub struct TorrentEngine {
    command_tx: mpsc::Sender<EngineCommand>,
    progress: Arc<Mutex<HashMap<String, TorrentProgress>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    handles: Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    done_rx: Mutex<mpsc::Receiver<(String, anyhow::Result<(), String>)>>,
    counter: Arc<AtomicU64>,
}

fn spawn_engine(
    default_folder: PathBuf,
    command_rx: mpsc::Receiver<EngineCommand>,
    progress: Arc<Mutex<HashMap<String, TorrentProgress>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    handles: Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    done_tx: mpsc::Sender<(String, Result<(), String>)>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("torrent-engine".into())
        .spawn(move || -> anyhow::Result<()> {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            // Session state lives in the app folder (not inside `downloads`)
            // so the user can wipe the whole download tree without losing
            // resume data, and without seeing dot-folders among game files.
            let persistence_folder = default_folder
                .parent()
                .map(|parent| parent.join("torrent-session"))
                .unwrap_or_else(|| default_folder.join(".rqbit-session"));
            let expected_output =
                default_folder.canonicalize().unwrap_or_else(|_| default_folder.clone());
            let session = runtime.block_on(Session::new_with_opts(
                default_folder,
                SessionOptions {
                    persistence: Some(SessionPersistenceConfig::Json {
                        folder: Some(persistence_folder),
                    }),
                    ..Default::default()
                },
            ))?;
            // Torrents restored from the persisted session (a previous run)
            // resume downloading automatically but have no tracked job, so the
            // UI would show an empty download list. Adopt each of them into a
            // watch loop right away.
            let restored: Vec<ManagedTorrentHandle> = session.with_torrents(|torrents| {
                torrents.map(|(_, handle)| handle.clone()).collect()
            });
            let mut stale_ids: Vec<librqbit::api::TorrentIdOrHash> = Vec::new();
            for handle in restored {
                // A session saved by an older build may point at a scan folder
                // (or a since-deleted path). Drop those entries so the user can
                // re-download into the current downloads directory instead of
                // seeing a stale 100% row with no files behind it.
                let stale = !handle
                    .output_folder()
                    .canonicalize()
                    .map(|p| p == expected_output)
                    .unwrap_or(true);
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
                runtime.spawn(watch_torrent(
                    session, key, handle, progress, cancels, handles,
                ));
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
                runtime.spawn(async move {
                    let result = match command {
                        EngineCommand::Add(job) => {
                            run_job(session, job, progress, cancels, handles).await
                        }
                        EngineCommand::Pause(key) => {
                            let outcome = pause_torrent(&session, &handles, &key, true).await;
                            (key, outcome)
                        }
                        EngineCommand::Resume(key) => {
                            let outcome = pause_torrent(&session, &handles, &key, false).await;
                            (key, outcome)
                        }
                        EngineCommand::Cancel(key, delete_files) => {
                            // Signal the watch loop first so it exits; then
                            // drop the torrent from the session. The persisted
                            // state file is refreshed by rqbit itself.
                            if let Ok(map) = cancels.lock() {
                                if let Some(flag) = map.get(&key) {
                                    flag.store(true, Ordering::Relaxed);
                                }
                            }
                            let outcome =
                                remove_torrent(&session, &handles, &key, delete_files).await;
                            (key, outcome)
                        }
                    };
                    // The receiver may be gone during shutdown; that is fine.
                    let _ = done_tx.send(result);
                });
            }
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!("토렌트 엔진 스레드를 시작하지 못했습니다: {e}"))?;
    Ok(())
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

async fn remove_torrent(
    session: &Arc<Session>,
    handles: &Arc<Mutex<HashMap<String, ManagedTorrentHandle>>>,
    key: &str,
    delete_files: bool,
) -> Result<(), String> {
    // Deleting files requires the handle: rqbit removes the torrent's output
    // tree from the handle's resolved folder.
    let handle = handles.lock().ok().and_then(|map| map.get(key).cloned());
    let id = match handle {
        Some(handle) => TorrentIdOrHash::Id(handle.id()),
        None => {
            // Restored torrents adopted as r{session id}; parse the id back.
            let restored_id = key
                .strip_prefix('r')
                .and_then(|id| id.parse::<usize>().ok())
                .ok_or_else(|| "해당 다운로드 항목을 찾을 수 없습니다".to_string())?;
            TorrentIdOrHash::Id(restored_id)
        }
    };
    // When delete_files is false the downloaded bytes stay on disk.
    session
        .delete(id, delete_files)
        .await
        .map_err(|e| format!("토렌트 제거 실패: {e:#}"))
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
) {
    if let Ok(mut map) = handles.lock() {
        map.insert(key.clone(), handle.clone());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    if let Ok(mut map) = cancels.lock() {
        map.insert(key.clone(), cancel.clone());
    }
    let name = handle.name().unwrap_or_default();
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
            if let Ok(mut map) = handles.lock() {
                map.remove(&key);
            }
            return;
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
        let update = TorrentProgress {
            name: name.to_owned(),
            total_bytes: stats.total_bytes,
            progress_bytes: stats.progress_bytes,
            finished: stats.finished,
            paused,
            state: if paused {
                "일시정지".into()
            } else {
                match stats.state {
                    TorrentStatsState::Initializing { .. } => "초기화".into(),
                    TorrentStatsState::Live => "다운로드".into(),
                    TorrentStatsState::Paused => "일시정지".into(),
                    TorrentStatsState::Error => "오류".into(),
                }
            },
            error: stats.error.clone(),
            download_speed_bps: if paused { 0 } else { live_speed_bps },
        };
        let finished = update.finished || update.error.is_some();
        if let Ok(mut map) = progress.lock() {
            map.insert(key.clone(), update);
        }
        if finished {
            // Release the file locks so emulators can open the freshly
            // downloaded files immediately (rqbit keeps them open for seeding
            // otherwise, and the emulator then fails with permission denied).
            let _ = session.pause(&handle).await;
            if let Ok(mut map) = cancels.lock() {
                map.remove(&key);
            }
            return;
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
) -> (String, Result<(), String>) {
    let key = job.key.clone();
    let result = match add_torrent(&session, &job).await {
        Ok(handle) => {
            // Hand the torrent over to the shared watch loop so pause/resume
            // and cancel keep working after the magnet handshake.
            watch_torrent(
                session,
                key.clone(),
                handle,
                progress,
                cancels,
                handles,
            )
            .await;
            Ok(())
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
            Err(text)
        }
    };
    (key, result)
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
        overwrite: true,
        ..Default::default()
    };
    session
        .add_torrent(AddTorrent::from_url(job.magnet.clone()), Some(opts))
        .await
        .and_then(|response| {
            response
                .into_handle()
                .context("이 토렌트는 파일 목록만 조회할 수 있습니다")
        })
        .map_err(|e| anyhow::anyhow!("{e}"))
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
        for entry in walkdir::WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
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
            handles,
            done_rx: Mutex::new(done_rx),
            counter: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Queue a magnet for download. Returns the progress key.
    pub fn start_download(
        &self,
        label: &str,
        magnet: &str,
        sub_folder: PathBuf,
    ) -> Result<String> {
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let key = format!("t{seq}");
        let job = TorrentJob {
            key: key.clone(),
            magnet: magnet.to_owned(),
            sub_folder,
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
        self.command_tx
            .send(EngineCommand::Add(job))
            .map_err(|_| anyhow::anyhow!("토렌트 엔진이 중지되었습니다"))?;
        Ok(key)
    }

    /// Pause a running download. Downloaded bytes are kept.
    pub fn pause(&self, key: &str) {
        let _ = self
            .command_tx
            .send(EngineCommand::Pause(key.to_owned()));
    }

    /// Resume a paused download.
    pub fn resume(&self, key: &str) {
        let _ = self
            .command_tx
            .send(EngineCommand::Resume(key.to_owned()));
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
        if let Ok(mut map) = self.progress.lock() {
            map.remove(key);
        }
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.remove(key);
        }
        if let Ok(mut handles) = self.handles.lock() {
            handles.remove(key);
        }
    }

    /// Drain completion events and snapshot live progress for the UI.
    /// Returns the finished jobs since the last poll so the caller can log
    /// them (the entries stay in the progress map until dismissed).
    pub fn poll(&self) -> (ProgressList, CompletionList) {
        let mut completions = Vec::new();
        loop {
            let received = self
                .done_rx
                .lock()
                .ok()
                .and_then(|rx| rx.try_recv().ok());
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
    }

    #[test]
    fn speed_computation_never_divides_by_zero() {
        // Regression: `delta / elapsed.max(0.001) as u64` truncated the
        // sub-second elapsed time (0.5s) to 0 and panicked with a division
        // by zero as soon as the first speed sample was due. The fixed
        // formula divides in f64 space, so any positive elapsed time yields
        // a finite speed.
        let compute = |delta: u64, elapsed: f64| -> u64 {
            (delta as f64 / elapsed.max(0.001)) as u64
        };
        assert_eq!(compute(1_000_000, 0.5), 2_000_000);
        assert_eq!(compute(0, 0.5), 0);
        assert_eq!(compute(500, 2.0), 250);
        // A sub-millisecond elapsed value must not blow up either.
        assert!(compute(1, 0.0000001) > 0);
    }
}
