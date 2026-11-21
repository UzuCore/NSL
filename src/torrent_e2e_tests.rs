//! End-to-end torrent QA restricted to loopback and generated test bytes.
//! Never uses the real NSL data root, public trackers, or DHT.
use super::*;
use librqbit::{
    limits::LimitsConfig, spawn_utils::BlockingSpawner, CreateTorrentOptions, ListenerMode,
    ListenerOptions,
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

/// In-process loopback-only HTTP tracker. A persisted tracker URL allows
/// the downloader to rediscover the local seeder after a simulated restart,
/// exactly as a real magnet tracker does when DHT is unavailable.
struct LocalTracker {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LocalTracker {
    fn new(seed_peer: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut body = b"d8:intervali5e5:peers6:".to_vec();
            body.extend_from_slice(&[127, 0, 0, 1]);
            body.extend_from_slice(&seed_peer.port().to_be_bytes());
            body.push(b'e');
            while !flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut request = [0u8; 4096];
                        let _ = stream.read(&mut request);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}/announce"),
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for LocalTracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            handle.join().unwrap();
        }
    }
}

fn wait_for(
    engine: &TorrentEngine,
    timeout: Duration,
    predicate: impl Fn(&str, &TorrentProgress) -> bool,
) -> (String, TorrentProgress) {
    let start = Instant::now();
    let mut last_state = String::new();
    loop {
        let (jobs, _) = engine.poll();
        for (id, progress) in jobs {
            last_state = format!(
                "id={} state={} completed={}/{} paused={} finished={}",
                id,
                progress.state,
                progress.progress_bytes,
                progress.total_bytes,
                progress.paused,
                progress.finished
            );
            assert!(
                progress.error.is_none(),
                "unexpected torrent error: {progress:?}"
            );
            if predicate(&id, &progress) {
                return (id, progress);
            }
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?}; last state: {last_state}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Reusable loopback-only seeder; no DHT or public trackers are contacted.
fn local_file_seeder(
    file: &Path,
    seed_dir: &Path,
    upload_bytes_per_second: u32,
) -> (
    tokio::runtime::Runtime,
    Arc<Session>,
    ManagedTorrentHandle,
    LocalTracker,
    String,
) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (session, handle, peer, magnet) = runtime.block_on(async {
        let piece_length = if std::fs::metadata(file).unwrap().len() >= 64 * 1024 * 1024 {
            512 * 1024
        } else {
            32 * 1024
        };
        let fixture = librqbit::create_torrent(
            file,
            CreateTorrentOptions {
                piece_length: Some(piece_length),
                ..Default::default()
            },
            &BlockingSpawner::new(1),
        )
        .await
        .unwrap();
        let session = Session::new_with_opts(
            seed_dir.to_path_buf(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                listen: Some(ListenerOptions {
                    mode: ListenerMode::TcpOnly,
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                }),
                ratelimits: LimitsConfig {
                    upload_bps: NonZeroU32::new(upload_bytes_per_second),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(fixture.as_bytes().unwrap()),
                Some(AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(seed_dir.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .into_handle()
            .unwrap();
        let started = tokio::time::Instant::now();
        while !handle.stats().finished {
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "generated test seeder did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let peer = session.listen_addr().expect("loopback listener");
        let magnet = librqbit::Magnet::from_id20(fixture.info_hash(), vec![], None).to_string();
        (session, handle, peer, magnet)
    });
    assert!(peer.ip().is_loopback());
    let tracker = LocalTracker::new(peer);
    let magnet = format!(
        "{}&tr={}",
        magnet,
        url::form_urlencoded::byte_serialize(tracker.url.as_bytes()).collect::<String>()
    );
    (runtime, session, handle, tracker, magnet)
}

/// Invoked only as a dedicated child test binary by the Stage 23 regression.
/// This is a true separate Windows process, not a second thread sharing a
/// process-wide mutex. Running the child without the private environment
/// marker is a harmless no-op.
#[test]
#[ignore = "child-only real process-wide session lock probe"]
fn stage23_cross_process_session_lock_child() {
    let Some(folder) = std::env::var_os("NSL_STAGE23_LOCK_CHILD_PROFILE") else {
        return;
    };
    if std::env::var("NSL_STAGE23_LOCK_CHILD_MODE").ok().as_deref() == Some("hold-until-killed") {
        let engine = TorrentEngine::new(PathBuf::from(folder)).unwrap();
        let marker = PathBuf::from(std::env::var_os("NSL_STAGE23_LOCK_CHILD_MARKER").unwrap());
        std::fs::write(marker, b"isolated child engine ready").unwrap();
        // Only the parent that launched this exact child process kills it.
        // Direct test execution without the env opt-in never enters this loop.
        loop {
            let _ = engine.poll();
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    let expect_busy = std::env::var("NSL_STAGE23_LOCK_CHILD_EXPECT_BUSY").unwrap() == "yes";
    match TorrentEngine::new(PathBuf::from(folder)) {
        Ok(engine) => {
            assert!(
                !expect_busy,
                "second process opened the already-locked session"
            );
            let _ = engine.poll();
            drop(engine);
            eprintln!("STAGE23_CHILD_PROCESS_SESSION_OPEN=PASS");
        }
        Err(error) => {
            assert!(expect_busy, "unrelated child start error: {error:#}");
            assert!(
                error.to_string().contains("다른 NSL 인스턴스"),
                "wrong failure: {error:#}"
            );
            eprintln!("STAGE23_CHILD_PROCESS_SESSION_REFUSED=PASS");
        }
    }
}

/// Keep a genuine session running while a distinct test process tries to
/// initialize the exact same persisted profile. The child must fail before
/// reading/writing librqbit state; session clearing must fail too. Once the
/// first process exits, the child may start normally, and the original
/// downloaded payload remains exact.
#[test]
#[ignore = "separate Windows child process and localhost-generated torrent"]
fn stage23_cross_process_lock_preserves_active_session_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let data = (0..4 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(107).wrapping_add((i / 4096) as u32) as u8)
        .collect::<Vec<_>>();
    let source = seed_dir.join("stage23-lock.bin");
    std::fs::write(&source, &data).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&source, &seed_dir, 768 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download("stage23 cross process", &magnet, PathBuf::from("lock-qa"))
        .unwrap();
    let _ = wait_for(&engine, Duration::from_secs(35), |id, p| {
        id == key && p.progress_bytes >= 256 * 1024
    });
    let state = TorrentEngine::session_state_path(&client_dir);
    let db = state.join("session.json");
    let started = Instant::now();
    while !db.is_file() {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
    let before: serde_json::Value = serde_json::from_slice(&std::fs::read(&db).unwrap()).unwrap();
    let assert_same_torrent_identities = || {
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&db).unwrap()).unwrap();
        let expected = before["torrents"].as_object().unwrap();
        let actual = after["torrents"].as_object().unwrap();
        assert_eq!(expected.len(), actual.len());
        for (id, entry) in expected {
            let current = actual.get(id).expect("second process replaced a session");
            assert_eq!(entry["info_hash"], current["info_hash"]);
            assert_eq!(entry["output_folder"], current["output_folder"]);
        }
        // is_paused may legitimately change when the first process finishes
        // its download; only compare immutable identity fields, not raw JSON.
    };
    let exe = std::env::current_exe().unwrap();
    let run_child = |expected_busy: bool| {
        let result = std::process::Command::new(&exe)
            .args([
                "--exact",
                "torrent::localhost_tests::stage23_cross_process_session_lock_child",
                "--ignored",
                "--nocapture",
            ])
            .env("NSL_STAGE23_LOCK_CHILD_PROFILE", &client_dir)
            .env(
                "NSL_STAGE23_LOCK_CHILD_EXPECT_BUSY",
                if expected_busy { "yes" } else { "no" },
            )
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "separate child process failed: stdout={} stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains(if expected_busy {
                "STAGE23_CHILD_PROCESS_SESSION_REFUSED=PASS"
            } else {
                "STAGE23_CHILD_PROCESS_SESSION_OPEN=PASS"
            }),
            "child did not report the expected lock outcome: {stderr}"
        );
    };
    run_child(true);
    assert_same_torrent_identities();
    let error = TorrentEngine::clear_session_state(&client_dir)
        .expect_err("settings must not clear an active session owned by another process");
    assert!(error.to_string().contains("다른 NSL 인스턴스"));
    assert_same_torrent_identities();
    eprintln!("STAGE23_CROSS_PROCESS_DENIED=PASS child_no_mutation=true active_clear_denied=true");

    drop(engine);
    run_child(false);
    let restored = TorrentEngine::new(client_dir.clone()).unwrap();
    let (key, final_job) = wait_for(&restored, Duration::from_secs(35), |id, p| {
        id.starts_with('r') && p.finished
    });
    assert_eq!(final_job.progress_bytes, data.len() as u64);
    let output = client_dir.join("lock-qa").join("stage23-lock.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);
    restored.cancel(&key, false);
    let started = Instant::now();
    while restored.poll().0.iter().any(|(id, _)| id == &key) {
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(restored);
    let _ = TorrentEngine::clear_session_state(&client_dir).unwrap();
    assert!(!state.exists());
    assert_eq!(std::fs::read(&output).unwrap(), data);
    eprintln!("STAGE23_CROSS_PROCESS_RECOVERY=PASS bytes={} child_after_close=true clear_after_close=true payload_unchanged=true", data.len());
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Abruptly kill only a test child that this test spawned and owns. Windows
/// must release the process-wide session lock; the next engine then restores
/// all verified pieces and completes the same local generated payload.
#[test]
#[ignore = "kill only owned child test process; localhost generated torrent"]
fn stage23_owned_child_abrupt_exit_releases_lock_and_restores_data() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let data = (0..5 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(113).wrapping_add((i / 4096) as u32) as u8)
        .collect::<Vec<_>>();
    let seed_path = seed_dir.join("stage23-abrupt.bin");
    std::fs::write(&seed_path, &data).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&seed_path, &seed_dir, 768 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage23 killed child",
            &magnet,
            PathBuf::from("child-crash"),
        )
        .unwrap();
    let (_, progress) = wait_for(&engine, Duration::from_secs(30), |id, p| {
        id == key && p.progress_bytes > 512 * 1024 && !p.finished
    });
    assert!(progress.progress_bytes < data.len() as u64);
    drop(engine);

    let ready = temp.path().join("stage23-child-ready.txt");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "torrent::localhost_tests::stage23_cross_process_session_lock_child",
            "--ignored",
            "--nocapture",
        ])
        .env("NSL_STAGE23_LOCK_CHILD_PROFILE", &client_dir)
        .env("NSL_STAGE23_LOCK_CHILD_MODE", "hold-until-killed")
        .env("NSL_STAGE23_LOCK_CHILD_MARKER", &ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut owned = OwnedChild(child);
    let started = Instant::now();
    while !ready.is_file() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "test child could not open its isolated session"
        );
        assert!(owned.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(50));
    }
    let refused = TorrentEngine::new(client_dir.clone())
        .err()
        .expect("concurrent process unexpectedly opened the test session");
    assert!(refused.to_string().contains("다른 NSL 인스턴스"));
    owned.0.kill().unwrap();
    let _ = owned.0.wait().unwrap();
    eprintln!("STAGE23_OWNED_CHILD_KILLED=PASS lock_prevented_concurrent=true");

    let recovered = TorrentEngine::new(client_dir.clone()).unwrap();
    let (restored_key, complete) = wait_for(&recovered, Duration::from_secs(40), |id, p| {
        id.starts_with('r') && p.finished
    });
    assert_eq!(complete.progress_bytes, data.len() as u64);
    let output = client_dir.join("child-crash").join("stage23-abrupt.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);
    recovered.cancel(&restored_key, false);
    let started = Instant::now();
    while recovered.poll().0.iter().any(|(id, _)| id == &restored_key) {
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(recovered);
    assert_eq!(std::fs::read(&output).unwrap(), data);
    eprintln!("STAGE23_ABRUPT_CHILD_EXIT_RECOVERED=PASS bytes={} owned_child_only=true sha_bytes_exact=true", data.len());
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Corrupt deletion journals must stop startup *before* librqbit opens and
/// restores an existing session, never silently resume an unwanted torrent.
#[test]
fn stage23_corrupt_deletion_journal_fails_closed_without_modifying_session() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = temp.path().join("downloads");
    let state = TorrentEngine::session_state_path(&downloads);
    let journal = state.join("nsl-deletion-intents");
    std::fs::create_dir_all(&journal).unwrap();
    let json = state.join("session.json");
    let before = br#"{"torrents":{},"other_metadata":"preserve"}"#;
    std::fs::write(&json, before).unwrap();
    let corrupt_path = journal.join("1-deadbeef.json");
    std::fs::write(&corrupt_path, b"{invalid-json").unwrap();
    let error = TorrentEngine::new(downloads.clone())
        .err()
        .expect("startup must fail closed on a corrupted delete journal");
    assert!(error.to_string().contains("삭제 복구"), "{error:#}");
    assert_eq!(std::fs::read(&json).unwrap(), before);
    assert_eq!(std::fs::read(&corrupt_path).unwrap(), b"{invalid-json");
    // Only the deliberately broken disposable test record is removed. The
    // originally valid profile may now launch without losing its metadata.
    std::fs::remove_file(&corrupt_path).unwrap();
    let engine = TorrentEngine::new(downloads).unwrap();
    assert!(engine.poll().0.is_empty());
    drop(engine);
}

/// A real, quota-exhausted filesystem reached over Windows' WSL UNC bridge.
/// Only runs against a separately created private 4 MiB tmpfs mount. Unlike
/// an injected failure this performs real librqbit payload writes until the
/// filesystem returns ENOSPC, then enlarges only this owned mount and retries.
/// Windows may report Linux ENOSPC as generic os error 31, not Win32 112.
#[cfg(windows)]
#[test]
#[ignore = "requires isolated WSL tmpfs mount and opt-in Stage 24 environment"]
fn stage24_real_quota_exhaustion_download_retry_exact_bytes() {
    let Some(win_path) = std::env::var_os("NSL_STAGE24_QUOTA_UNC") else {
        return;
    };
    let linux_root = std::env::var("NSL_STAGE24_QUOTA_LINUX_ROOT").unwrap();
    let linux_name = linux_root
        .strip_prefix("/tmp/nsl-stage24-quota-")
        .expect("refuse to remount anything except an isolated test quota");
    assert!(
        !linux_name.is_empty()
            && linux_name.len() <= 40
            && linux_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid isolated mount name"
    );
    let client_root = PathBuf::from(win_path);
    assert!(
        client_root
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(r"\\wsl.localhost\ubuntu\tmp\nsl-stage24-quota-"),
        "this test may only write to its temporary WSL memory filesystem"
    );
    assert!(client_root.is_dir());
    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let payload = (0..12 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(83).wrapping_add((i / 4096) as u32) as u8)
        .collect::<Vec<_>>();
    let seed_file = seed_dir.join("stage24-real-enospc.bin");
    std::fs::write(&seed_file, &payload).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&seed_file, &seed_dir, 2 * 1024 * 1024);
    let client_dir = client_root.join("client").join("downloads");
    // WSL's Plan9 UNC bridge does not implement Windows LockFileEx. Preserve
    // production's real session-lock semantics using a disposable NTFS test
    // profile, while routing the actual payload file to the quota-limited
    // memory volume. No user state or user download tree is touched.
    let local_session_root = temp.path().join("state").join("downloads");
    let engine =
        TorrentEngine::new_with_session_root(client_dir.clone(), local_session_root).unwrap();
    let key = engine
        .start_download(
            "stage24 isolated real ENOSPC",
            &magnet,
            PathBuf::from("quota"),
        )
        .unwrap();
    let started = Instant::now();
    let mut failure = None;
    let mut progressed = 0;
    while failure.is_none() {
        let (jobs, notices) = engine.poll();
        for (id, progress) in jobs {
            if id == key {
                progressed = progressed.max(progress.progress_bytes);
                if progress.error.is_some() {
                    failure = progress.error;
                }
            }
        }
        for notice in notices {
            if let EngineNotice::Download {
                key: id,
                result: Err(error),
            } = notice
            {
                if id == key {
                    eprintln!("STAGE24_REAL_QUOTA_FATAL_NOTICE={error}");
                    failure.get_or_insert(error);
                }
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(35),
            "private WSL quota did not trigger a real torrent payload-write fault"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let message = failure.unwrap();
    eprintln!("STAGE24_REAL_QUOTA_FATAL bytes_before_failure={progressed} error={message}");
    assert!(
        message.contains("writing") || message.contains("write") || message.contains("os error 31"),
        "unexpected non-payload failure: {message}"
    );
    assert!(
        progressed < payload.len() as u64,
        "limited 4 MiB tmpfs unexpectedly stored the entire 12 MiB payload"
    );

    // Change the quota of ONLY the test-owned WSL mount, not any user drive.
    let remount = std::process::Command::new("wsl.exe")
        .args([
            "-d",
            "Ubuntu",
            "-u",
            "root",
            "--",
            "mount",
            "-o",
            "remount,size=48m",
            &linux_root,
        ])
        .output()
        .unwrap();
    assert!(
        remount.status.success(),
        "isolated WSL tmpfs remount failed: {}",
        String::from_utf8_lossy(&remount.stderr)
    );
    engine.retry(&key);
    let accepted_at = Instant::now();
    let mut accepted = false;
    while !accepted {
        let (_, notices) = engine.poll();
        for notice in notices {
            if let EngineNotice::Command {
                key: id,
                action: "retry",
                result,
            } = notice
            {
                if id == key {
                    result.expect("real quota exhaustion was not retryable");
                    accepted = true;
                }
            }
        }
        assert!(accepted_at.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, complete) = wait_for(&engine, Duration::from_secs(55), |id, progress| {
        id == key && progress.finished
    });
    assert_eq!(complete.progress_bytes, payload.len() as u64);
    let output = client_dir.join("quota").join("stage24-real-enospc.bin");
    assert_eq!(std::fs::read(&output).unwrap(), payload);
    assert_eq!(std::fs::read(&seed_file).unwrap(), payload);
    engine.cancel(&key, false);
    let start = Instant::now();
    while engine.poll().0.iter().any(|(id, _)| id == &key) {
        assert!(start.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&output).unwrap(), payload);
    eprintln!("STAGE24_QUOTA_EXHAUSTION_RECOVERED=PASS bytes={} enlarged_isolated_tmpfs=true retry=true payload_exact=true", payload.len());
    drop(engine);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Opt-in public-network transfer of a small CC BY 3.0 subtitle from the
/// official WebTorrent Sintel test torrent. Never contact a Switch tracker
/// during QA or download unlicensed games. Verified 20,792-byte metadata
/// pins the selected subtitle index and prevents silent file substitution.
#[test]
#[ignore = "external peer availability required; CC BY 3.0 Sintel subtitle only"]
fn stage17_public_licensed_sintel_selected_subtitle() -> anyhow::Result<()> {
    use anyhow::Context;
    use sha2::Digest;
    use std::io::Read as _;
    let qa_root = Path::new("E:\\NSL_QA_20261120");
    anyhow::ensure!(qa_root.is_dir());
    let private = tempfile::Builder::new()
        .prefix("stage17-licensed-public-")
        .tempdir_in(qa_root)?;
    let started = Instant::now();
    let metadata_url = "https://webtorrent.io/torrents/sintel.torrent";
    let expected_srt_url = "https://webtorrent.io/torrents/Sintel/Sintel.en.srt";
    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(25))
        .build();
    let mut metadata = Vec::new();
    http.get(metadata_url)
        .call()
        .context("CC BY 3.0 source torrent metadata HTTP failure")?
        .into_reader()
        .take(128 * 1024)
        .read_to_end(&mut metadata)?;
    anyhow::ensure!(metadata.len() == 20_792);
    let fingerprint = sha2::Sha256::digest(&metadata)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    anyhow::ensure!(
        fingerprint == "4c8fdad0414b4767546a0f92fe3d660a66edc32471874e7a1cfa97120317b84a",
        "Creative Commons test torrent changed unexpectedly"
    );
    let magnet = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Sintel&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexplodie.org%3A6969&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969";
    let engine = TorrentEngine::new(private.path().join("downloads"))?;
    // Prove that the catalog's normal magnet-only preview also works over
    // public trackers: the official HTTPS .torrent above is a pinned
    // reference, NOT the source used by the actual picker/download.
    let request = engine.preview_files(magnet)?;
    let preview = request
        .receiver
        .recv_timeout(Duration::from_secs(45))
        .context("Public Creative Commons magnet metadata preview timeout")?
        .map_err(anyhow::Error::msg)?;
    drop(request);
    anyhow::ensure!(preview.files.len() == 11);
    let selected = preview
        .files
        .iter()
        .find(|file| file.path.ends_with("Sintel.en.srt"))
        .context("Public magnet metadata missing licensed subtitle")?;
    anyhow::ensure!(selected.index == 1 && selected.size == 1514);
    anyhow::ensure!(
        !private
            .path()
            .join("downloads")
            .join("sintel-subtitle")
            .exists(),
        "Metadata preview unexpectedly downloaded payload files"
    );
    let key = engine.start_download_selected(
        "Sintel Creative Commons subtitle (QA)",
        magnet,
        PathBuf::from("sintel-subtitle"),
        Some(preview.torrent_bytes),
        Some(vec![selected.index]),
    )?;
    let mut peer_seen = false;
    let mut seen_bytes = false;
    let mut final_notice = false;
    let mut last_status = String::new();
    while started.elapsed() < Duration::from_secs(85) {
        let (status, notices) = engine.poll();
        for (item_key, progress) in status {
            if item_key != key {
                continue;
            }
            if let Some(error) = progress.error {
                anyhow::bail!("Licensed public torrent failed: {error}");
            }
            peer_seen |=
                progress.active_seeders.unwrap_or(0) + progress.active_peers.unwrap_or(0) > 0;
            seen_bytes |= progress.progress_bytes > 0;
            last_status = format!(
                "state={} bytes={}/{} finished={} paused={}",
                progress.state,
                progress.progress_bytes,
                progress.total_bytes,
                progress.finished,
                progress.paused
            );
        }
        for notice in notices {
            if let EngineNotice::Download {
                key: item_key,
                result,
            } = notice
            {
                if item_key == key {
                    result.map_err(anyhow::Error::msg)?;
                    final_notice = true;
                }
            }
        }
        if final_notice {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::ensure!(
        final_notice,
        "Public CC-BY torrent not completed; last_status={last_status} peer_seen={peer_seen} bytes_seen={seen_bytes}"
    );
    let file = walkdir::WalkDir::new(private.path().join("downloads"))
        .into_iter()
        .filter_map(|entry| entry.ok())
        .find(|entry| entry.file_type().is_file() && entry.file_name() == "Sintel.en.srt")
        .context("Selected Sintel subtitle not found in isolated downloaded files")?;
    let contents = std::fs::read(file.path())?;
    anyhow::ensure!(contents.len() == 1514, "Unexpected licensed subtitle bytes");
    let mut authoritative = Vec::new();
    http.get(expected_srt_url)
        .call()
        .context("Official CC-BY subtitle HTTPS reference not available")?
        .into_reader()
        .take(16 * 1024)
        .read_to_end(&mut authoritative)?;
    anyhow::ensure!(
        contents == authoritative,
        "BitTorrent selected subtitle did not match the official HTTPS version"
    );
    println!(
        "STAGE17_PUBLIC_LICENSED_TORRENT_PASS download_bytes={} verified_https_equal=true live_peer_snapshot={} magnet_only_preview=true completion_notice=true elapsed_s={:.2}",
        contents.len(),
        peer_seen,
        started.elapsed().as_secs_f64(),
    );
    drop(engine);
    Ok(())
}

/// Full production batch pipeline using an authorized game fixture:
/// real BitTorrent pieces over TCP loopback, verified metadata preview,
/// selected-file queue, completion notice, crash-journal restoration,
/// and the native NSZ game/update merge. No external tracker, DHT, LAN
/// listener, original-data modification, or emulator process is involved.
#[test]
#[ignore = "opt-in private localhost-only real 346 MiB NSP BitTorrent -> NSZ merge"]
fn real_patch_loopback_torrent_to_restarted_batch_and_native_merge() -> anyhow::Result<()> {
    use anyhow::Context;
    use sha2::Digest;
    use std::io::Read as _;

    fn checksum(path: &Path) -> anyhow::Result<[u8; 32]> {
        let mut hasher = sha2::Sha256::new();
        let mut source = std::fs::File::open(path)?;
        let mut block = [0u8; 1024 * 1024];
        loop {
            let read = source.read(&mut block)?;
            if read == 0 {
                return Ok(hasher.finalize().into());
            }
            hasher.update(&block[..read]);
        }
    }
    let qa_root = PathBuf::from("E:\\NSL_QA_20261120");
    let original_base = qa_root.join("small").join("base.nsz");
    let original_patch = qa_root.join("small").join("patch.nsp");
    anyhow::ensure!(
        original_base.is_file() && original_patch.is_file(),
        "Stage 16 read-only real game fixtures are unavailable"
    );
    let qa = tempfile::Builder::new()
        .prefix("stage16-local-peer-real-")
        .tempdir_in(&qa_root)?;
    let seed_dir = qa.path().join("private-seeder");
    let game_dir = qa.path().join("private-game");
    let app_dir = qa.path().join("private-nsl");
    for directory in [&seed_dir, &game_dir, &app_dir] {
        std::fs::create_dir_all(directory)?;
    }
    let original_base_sha = checksum(&original_base)?;
    let original_patch_sha = checksum(&original_patch)?;
    let game = game_dir.join("base.nsz");
    let seed_file = seed_dir.join("patch.nsp");
    std::fs::copy(&original_base, &game)?;
    std::fs::copy(&original_patch, &seed_file)?;
    anyhow::ensure!(checksum(&game)? == original_base_sha);
    anyhow::ensure!(checksum(&seed_file)? == original_patch_sha);

    let began = Instant::now();
    let (seed_runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&seed_file, &seed_dir, 48 * 1024 * 1024);
    let mut app = crate::NslApp::with_base_dir(app_dir.clone())?;
    let engine = Arc::clone(
        app.torrent_engine
            .as_ref()
            .context("No NSL torrent engine")?,
    );
    let preview_request = engine.preview_files(&magnet)?;
    let preview = preview_request
        .receiver
        .recv_timeout(Duration::from_secs(45))
        .context("Local magnet metadata preview timeout")?
        .map_err(anyhow::Error::msg)?;
    drop(preview_request);
    anyhow::ensure!(preview.files.len() == 1);
    let selected = preview.files[0].clone();
    anyhow::ensure!(selected.size == std::fs::metadata(&original_patch)?.len());
    anyhow::ensure!(
        !app_dir.join("downloads").join("stage16-real").exists(),
        "Metadata-only preview wrote a payload"
    );
    let base_id = "0100F7301AAB6000".to_string();
    let wanted_version = 65536;
    app.update_batch = Some(crate::update_batch::UpdateBatch {
        kind: crate::update_batch::BatchKind::Update,
        rows: vec![crate::update_batch::UpdateBatchRow {
            library_id: 16,
            title_id: base_id.clone(),
            dlc_id: None,
            name: "Stage16 localhost-only Horror Tale 2 update".into(),
            installed_version: 0,
            latest_version: wanted_version,
            original_path: game.clone(),
            original_type: "nsz".into(),
            candidates: Vec::new(),
            next_candidate: 0,
            found: Some(crate::update_batch::UpdateBatchMatch {
                release: crate::catalog::CatalogItem {
                    title: "Stage16 isolated real NSP torrent".into(),
                    title_id: Some(base_id.clone()),
                    topic_id: "stage16-only".into(),
                    magnet,
                    ..Default::default()
                },
                file: selected.clone(),
                version: wanted_version,
                torrent_bytes: Arc::new(preview.torrent_bytes),
            }),
            checked: true,
            failure: None,
            job: None,
        }],
        running: false,
        cursor: 1,
        active: None,
        confirm_queue: false,
        preview_cache: Default::default(),
        preview_cache_bytes: 0,
    });
    // Invoke the real catalog-to-download queue code, not a synthetic Add.
    app.queue_missing_updates();
    let job = app
        .update_batch
        .as_ref()
        .context("No batch after queue")?
        .rows[0]
        .job
        .as_ref()
        .context("Catalog-to-batch queue rejected actual torrent metadata")?;
    let queue_key = job.key.clone();
    let downloaded = job.output.clone();
    anyhow::ensure!(job.expected_file_size == selected.size);
    let mut copied_bytes_seen = false;
    let mut peer_seen = false;
    let mut successful_notice = false;
    let ctx = eframe::egui::Context::default();
    let output = loop {
        anyhow::ensure!(
            began.elapsed() < Duration::from_secs(340),
            "Local real-torrent to native-merge E2E timed out"
        );
        let (snapshot, notices) = engine.poll();
        if let Some((_, live)) = snapshot.iter().find(|(key, _)| key == &queue_key) {
            anyhow::ensure!(
                live.error.is_none(),
                "Local BitTorrent peer error: {:?}",
                live.error
            );
            if live.progress_bytes > 0 && !live.finished {
                copied_bytes_seen = true;
                peer_seen |= live.active_seeders.unwrap_or(0) + live.active_peers.unwrap_or(0) > 0;
            }
        }
        app.torrent_progress = snapshot;
        for notice in notices {
            if let EngineNotice::Download { key, result } = notice {
                if key == queue_key {
                    anyhow::ensure!(
                        result.is_ok(),
                        "NSL torrent completion was unsuccessful: {result:?}"
                    );
                    anyhow::ensure!(copied_bytes_seen, "No live peer transfer recorded");
                    anyhow::ensure!(peer_seen, "No real loopback BitTorrent peer observed");
                    // The engine emits this notice only AFTER pausing and
                    // unlocking its completed file handles.
                    app.handle_missing_update_completion(&key, &result);
                    let completed = crate::update_manifest::load(&app_dir)?
                        .context("Completed torrent was not journaled")?;
                    anyhow::ensure!(completed.rows[0]
                        .job
                        .as_ref()
                        .is_some_and(|job| job.transfer_finished));
                    // Simulate an application crash/restart in the narrow
                    // window between successful transfer and native merge.
                    app.update_batch = Some(completed);
                    successful_notice = true;
                    println!(
                        "STAGE16_REAL_TORRENT_COMPLETED bytes={} real_peer=true crash_journal_restored=true elapsed_s={:.2}",
                        std::fs::metadata(&downloaded)?.len(),
                        began.elapsed().as_secs_f64()
                    );
                }
            }
        }
        if successful_notice {
            app.poll_missing_update_audit(&ctx);
            if let Some(job) = app
                .update_batch
                .as_ref()
                .and_then(|batch| batch.rows[0].job.as_ref())
            {
                match &job.status {
                    crate::update_batch::UpdateBatchJobStatus::Merged(path) => {
                        break path.clone();
                    }
                    crate::update_batch::UpdateBatchJobStatus::Failed(reason) => {
                        anyhow::bail!("Post-torrent NSZ native merge failed: {reason}");
                    }
                    _ => {}
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    anyhow::ensure!(output.is_file());
    anyhow::ensure!(output
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")));
    anyhow::ensure!(std::fs::metadata(&output)?.len() == 330_474_468);
    anyhow::ensure!(checksum(&downloaded)? == original_patch_sha);
    anyhow::ensure!(checksum(&game)? == original_base_sha);
    anyhow::ensure!(checksum(&original_patch)? == original_patch_sha);
    anyhow::ensure!(checksum(&original_base)? == original_base_sha);
    let post = crate::update_manifest::load(&app_dir)?.context("Merged journal missing")?;
    anyhow::ensure!(post.rows[0].job.as_ref().is_some_and(|job| {
        job.status == crate::update_batch::UpdateBatchJobStatus::Merged(output.clone())
    }));
    let source_hash = checksum(&output)?;
    println!(
        "STAGE16_REAL_LOOPBACK_TORRENT_NATIVE_MERGE_PASS bytes_downloaded={} nsz_bytes={} peer_seen=true journal_restored=true source_sha_unchanged=true output_sha256={} elapsed_s={:.2}",
        selected.size,
        std::fs::metadata(&output)?.len(),
        source_hash.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        began.elapsed().as_secs_f64()
    );
    drop(app);
    drop(engine);
    drop(seed_session);
    drop(seed_runtime);
    drop(tracker);
    Ok(())
}

#[test]
#[ignore = "localhost-only real torrent transfer with generated data (run explicitly)"]
fn transfer_pause_restart_resume_delete_preserves_other_files() {
    let root = tempfile::tempdir().unwrap();
    let seed_dir = root.path().join("seed");
    let client_dir = root.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let mut data = vec![0u8; 4 * 1024 * 1024];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(37).wrapping_add((i / 8192) as u32) as u8;
    }
    std::fs::write(seed_dir.join("payload.bin"), &data).unwrap();

    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&seed_dir.join("payload.bin"), &seed_dir, 512 * 1024);

    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download("local QA", &magnet, PathBuf::from("sample"))
        .unwrap();
    let (_, first) = wait_for(&engine, Duration::from_secs(30), |id, p| {
        id == key && p.progress_bytes > 0 && !p.finished
    });
    eprintln!("STAGE9_TORRENT_FIRST={}", first.progress_bytes);
    assert!(
        first.active_seeders.unwrap_or(0) + first.active_peers.unwrap_or(0) > 0,
        "a localhost transfer with received data must show at least one live peer"
    );
    engine.pause(&key);
    let (_, paused) = wait_for(&engine, Duration::from_secs(12), |id, p| {
        id == key && p.paused
    });
    assert!(paused.progress_bytes >= first.progress_bytes);
    assert_eq!((paused.active_seeders, paused.active_peers), (None, None));
    eprintln!("STAGE9_TORRENT_PAUSED={}", paused.progress_bytes);
    engine.resume(&key);
    let (_, resumed) = wait_for(&engine, Duration::from_secs(15), |id, p| {
        id == key && !p.paused && p.progress_bytes > paused.progress_bytes
    });
    assert!(resumed.progress_bytes < data.len() as u64);
    eprintln!("STAGE9_TORRENT_RESUMED={}", resumed.progress_bytes);

    // App restart mid-transfer: re-create the engine using only the temp root.
    drop(engine);
    let restored = TorrentEngine::new(client_dir.clone()).unwrap();
    let saved = client_dir
        .parent()
        .unwrap()
        .join("torrent-session")
        .join("session.json");
    eprintln!("STAGE9_TORRENT_SESSION_SAVED={}", saved.exists());
    let (restored_key, finished) = wait_for(&restored, Duration::from_secs(25), |id, p| {
        id.starts_with('r') && p.finished
    });
    assert_eq!(finished.total_bytes, data.len() as u64);
    assert_eq!(
        (finished.active_seeders, finished.active_peers),
        (None, None)
    );
    eprintln!("STAGE9_TORRENT_RESTORED={}", finished.progress_bytes);
    let output = client_dir.join("sample").join("payload.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);

    restored.cancel(&restored_key, false);
    let start = Instant::now();
    while restored.poll().0.iter().any(|(id, _)| id == &restored_key) {
        assert!(start.elapsed() < Duration::from_secs(12), "cancel hung");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read(&output).unwrap(),
        data,
        "cancel deleted output bytes"
    );

    let sentinel = client_dir.parent().unwrap().join("keep.txt");
    std::fs::write(&sentinel, b"unchanged").unwrap();
    let next = restored
        .start_download("delete QA", &magnet, PathBuf::from("delete-case"))
        .unwrap();
    eprintln!("STAGE9_TORRENT_DELETE_PHASE={next}");
    wait_for(&restored, Duration::from_secs(25), |id, p| {
        id == next && p.finished
    });
    let delete_file = client_dir.join("delete-case").join("payload.bin");
    assert_eq!(std::fs::read(&delete_file).unwrap(), data);
    restored.cancel(&next, true);
    let start = Instant::now();
    while restored.poll().0.iter().any(|(id, _)| id == &next) {
        assert!(
            start.elapsed() < Duration::from_secs(12),
            "explicit delete hung"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !delete_file.exists(),
        "explicit delete did not remove test file"
    );
    assert_eq!(
        std::fs::read(&output).unwrap(),
        data,
        "sibling torrent was deleted"
    );
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
    drop(restored);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Real byte transfers with a deterministic test-only terminal watcher fault.
/// Unlike a peer outage, retry must reattach NSL's completion observer.
#[test]
#[ignore = "loopback-only 6 MiB generated torrent, terminal watcher retry and cancellation"]
fn stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e() {
    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let data = (0..6 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(97).wrapping_add((i / 8192) as u32) as u8)
        .collect::<Vec<_>>();
    let seed_file = seed_dir.join("stage19.bin");
    std::fs::write(&seed_file, &data).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&seed_file, &seed_dir, 1024 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();

    let key = engine
        .start_download(
            "stage19 terminal failure",
            &magnet,
            PathBuf::from("recovery"),
        )
        .unwrap();
    let (_, before) = wait_for(&engine, Duration::from_secs(30), |id, p| {
        id == key && p.progress_bytes > 256 * 1024 && !p.finished
    });
    engine.inject_terminal_watcher_failure(&key).unwrap();
    let mut saw_failure_event = false;
    let started = Instant::now();
    let failure = loop {
        let (jobs, notices) = engine.poll();
        for notice in notices {
            if let EngineNotice::Download {
                key: id,
                result: Err(_),
            } = notice
            {
                if id == key {
                    saw_failure_event = true;
                }
            }
        }
        if saw_failure_event {
            if let Some((_, progress)) = jobs.iter().find(|(id, p)| *id == key && p.error.is_some())
            {
                break progress.clone();
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "terminal fault was not observed: {jobs:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(failure
        .error
        .as_deref()
        .unwrap()
        .contains("STAGE19_TEST_TERMINAL_WATCHER_FAILURE"));
    assert!(failure.progress_bytes >= before.progress_bytes);
    assert!(retryable_failed_transfer(&failure));
    eprintln!("STAGE19_TERMINAL_ERROR_BYTES={}", failure.progress_bytes);

    // Both commands are queued before completion; one must be rejected.
    engine.retry(&key);
    engine.retry(&key);
    let mut accepted = 0_usize;
    let mut rejected = 0_usize;
    let started = Instant::now();
    let completed = loop {
        let (jobs, notices) = engine.poll();
        for notice in notices {
            match notice {
                EngineNotice::Command {
                    key: id,
                    action: "retry",
                    result,
                } if id == key => {
                    if result.is_ok() {
                        accepted += 1;
                    } else {
                        rejected += 1;
                    }
                }
                EngineNotice::Download { key: id, result } if id == key => {
                    assert!(result.is_ok(), "recovered job failed: {result:?}");
                }
                _ => {}
            }
        }
        if let Some((_, progress)) = jobs.iter().find(|(id, p)| *id == key && p.finished) {
            if accepted + rejected >= 2 {
                break progress.clone();
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(65),
            "retry timed out: notices={accepted}/{rejected} jobs={jobs:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!((accepted, rejected), (1, 1));
    assert_eq!(completed.total_bytes, data.len() as u64);
    let output = client_dir.join("recovery").join("stage19.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);
    engine.cancel(&key, false);
    let started = Instant::now();
    while engine.poll().0.iter().any(|(id, _)| id == &key) {
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&output).unwrap(), data);

    // Fail another job then cancel immediately following retry. It must not
    // resurrect a removed row or destroy the previous completed payload.
    let next = engine
        .start_download("stage19 cancel race", &magnet, PathBuf::from("cancel-race"))
        .unwrap();
    wait_for(&engine, Duration::from_secs(25), |id, p| {
        id == next && p.progress_bytes >= 256 * 1024 && !p.finished
    });
    engine.inject_terminal_watcher_failure(&next).unwrap();
    let mut saw_failed_event = false;
    let started = Instant::now();
    loop {
        let (jobs, events) = engine.poll();
        saw_failed_event |= events
            .iter()
            .any(|e| matches!(e, EngineNotice::Download {key: id, result: Err(_)} if *id == next));
        if saw_failed_event && jobs.iter().any(|(id, p)| *id == next && p.error.is_some()) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "second terminal failure not observed"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    engine.retry(&next);
    engine.cancel(&next, false);
    let started = Instant::now();
    while engine.poll().0.iter().any(|(id, _)| id == &next) {
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "retry+cancel left a stuck queue row"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        engine.poll().0.iter().all(|(id, _)| id != &next),
        "cancelled retry resurrected"
    );
    assert_eq!(std::fs::read(&output).unwrap(), data);
    eprintln!("STAGE19_TERMINAL_RETRY_CANCEL_PASS bytes={} retry_ok=1 duplicate_rejected=1 cancelled_not_resurrected=true", data.len());
    drop(engine);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Failure in the actual librqbit Session::delete API must leave the live
/// transfer and its persisted resume state usable. The injected missing ID is
/// one-shot and only affects a generated localhost torrent.
#[test]
#[ignore = "localhost-only 6 MiB real transfer, failed session delete and retry"]
fn stage20_failed_session_delete_preserves_progress_restart_and_files() {
    let root = tempfile::tempdir().unwrap();
    let seed_dir = root.path().join("seed");
    let client_dir = root.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let payload = (0..6 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(101).wrapping_add((i / 4096) as u32) as u8)
        .collect::<Vec<_>>();
    let source = seed_dir.join("stage20.bin");
    std::fs::write(&source, &payload).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&source, &seed_dir, 768 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage20 delete failure",
            &magnet,
            PathBuf::from("preserved"),
        )
        .unwrap();
    let (_, first) = wait_for(&engine, Duration::from_secs(30), |id, progress| {
        id == key && progress.progress_bytes > 256 * 1024 && !progress.finished
    });
    engine.inject_session_delete_failure(&key).unwrap();
    engine.cancel(&key, false);
    let started = Instant::now();
    let mut rejected = false;
    while !rejected {
        let (jobs, notices) = engine.poll();
        for notice in notices {
            if let EngineNotice::Command {
                key: id,
                action: "remove",
                result: Err(error),
            } = notice
            {
                if id == key {
                    assert!(error.contains("torrent") || error.contains("토렌트"));
                    rejected = true;
                }
            }
        }
        assert!(
            jobs.iter()
                .any(|(id, progress)| id == &key && progress.error.is_none()),
            "a failed session delete removed the still-active torrent row"
        );
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "delete fault timeout"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, after) = wait_for(&engine, Duration::from_secs(20), |id, progress| {
        id == key && progress.progress_bytes > first.progress_bytes + 256 * 1024
    });
    eprintln!(
        "STAGE20_FAILED_DELETE_PRESERVED first={} continued={}",
        first.progress_bytes, after.progress_bytes
    );

    // Discard this engine and ensure the failed deletion did not corrupt its
    // on-disk resume state: the next isolated engine must adopt the transfer.
    drop(engine);
    let restored = TorrentEngine::new(client_dir.clone()).unwrap();
    let (restored_key, complete) = wait_for(&restored, Duration::from_secs(45), |id, progress| {
        id.starts_with('r') && progress.finished
    });
    assert_eq!(complete.progress_bytes, payload.len() as u64);
    let preserved = client_dir.join("preserved").join("stage20.bin");
    assert_eq!(std::fs::read(&preserved).unwrap(), payload);
    restored.cancel(&restored_key, false);
    let started = Instant::now();
    while restored.poll().0.iter().any(|(id, _)| id == &restored_key) {
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&preserved).unwrap(), payload);

    // The delete-with-files choice must also preserve data when the session
    // delete itself fails, and a second real deletion must remove only the
    // selected torrent, not the previously dismissed/sibling output.
    let sentinel = client_dir.parent().unwrap().join("keep.txt");
    std::fs::write(&sentinel, b"stage20-sentinel").unwrap();
    let next = restored
        .start_download(
            "stage20 file removal",
            &magnet,
            PathBuf::from("delete-only"),
        )
        .unwrap();
    wait_for(&restored, Duration::from_secs(25), |id, progress| {
        id == next && progress.progress_bytes > 256 * 1024 && !progress.finished
    });
    restored.inject_session_delete_failure(&next).unwrap();
    restored.cancel(&next, true);
    let started = Instant::now();
    let mut saw_error = false;
    while !saw_error {
        let (jobs, notices) = restored.poll();
        saw_error = notices.iter().any(|notice| {
            matches!(notice,
            EngineNotice::Command { key: id, action: "remove", result: Err(_) } if *id == next)
        });
        assert!(
            jobs.iter().any(|(id, _)| id == &next),
            "failed file delete dropped a live row"
        );
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, finished) = wait_for(&restored, Duration::from_secs(45), |id, p| {
        id == next && p.finished
    });
    assert_eq!(finished.total_bytes, payload.len() as u64);
    let output = client_dir.join("delete-only").join("stage20.bin");
    assert_eq!(std::fs::read(&output).unwrap(), payload);
    restored.cancel(&next, true);
    let started = Instant::now();
    while restored.poll().0.iter().any(|(id, _)| id == &next) {
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !output.exists(),
        "successful delete-with-files left its test file"
    );
    assert_eq!(std::fs::read(&preserved).unwrap(), payload);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"stage20-sentinel");
    eprintln!(
        "STAGE20_DELETE_FAILURE_RECOVERY_PASS bytes={} restart_resume=true files_preserved=true selective_delete=true",
        payload.len()
    );
    drop(restored);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// On Windows, an open handle without FILE_SHARE_DELETE prevents unlinking.
/// librqbit 9.1.0 ignores this per-file failure and returns Ok. The NSL
/// wrapper must detect the remaining file, report a partial cleanup, and
/// clear the now-removed session row without erasing unrelated outputs.
#[cfg(windows)]
#[test]
#[ignore = "Windows localhost-only genuine locked-file deletion failure"]
fn stage20_locked_file_delete_reports_partial_cleanup_without_stranded_session() {
    let root = tempfile::tempdir().unwrap();
    let seed_dir = root.path().join("seed");
    let client_dir = root.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let data = (0..2 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(73).wrapping_add((i / 1024) as u32) as u8)
        .collect::<Vec<_>>();
    let source = seed_dir.join("stage20-readonly.bin");
    std::fs::write(&source, &data).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&source, &seed_dir, 1024 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download("stage20 readonly", &magnet, PathBuf::from("readonly"))
        .unwrap();
    wait_for(&engine, Duration::from_secs(25), |id, progress| {
        id == key && progress.finished
    });
    let output = client_dir.join("readonly").join("stage20-readonly.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);
    let sentinel = client_dir.parent().unwrap().join("keep.txt");
    std::fs::write(&sentinel, b"unmodified").unwrap();
    // Permit existing readers/writers but deny FILE_SHARE_DELETE on the
    // disposable downloaded file while this handle stays open.
    use std::os::windows::fs::OpenOptionsExt;
    let no_delete_guard = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&output)
        .expect("failed to open the test-only Windows no-delete sharing guard");
    engine.cancel(&key, true);
    let start = Instant::now();
    let mut failed_delete = false;
    while !failed_delete {
        let (jobs, notices) = engine.poll();
        assert!(
            jobs.iter().any(|(id, _)| id == &key),
            "preflight failure removed the resumable download row"
        );
        for notice in notices {
            if let EngineNotice::Command {
                key: id,
                action: "remove",
                result,
            } = notice
            {
                if id == key {
                    let error = result.expect_err("locked file deletion unexpectedly succeeded");
                    assert!(
                        error.contains("잠금 해제 후 다시 삭제하세요"),
                        "unexpected cleanup result: {error}"
                    );
                    failed_delete = true;
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(12),
            "preflight delete refusal timed out"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&output).unwrap(), data);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"unmodified");
    // Release the real Windows deletion lock and retry, with no loss of
    // session metadata or completed pieces before the successful deletion.
    drop(no_delete_guard);
    engine.cancel(&key, true);
    let started = Instant::now();
    let mut removed = false;
    while !removed {
        let (jobs, notices) = engine.poll();
        for notice in notices {
            if let EngineNotice::Command {
                key: id,
                action: "remove",
                result,
            } = notice
            {
                if id == key {
                    result.expect("unlocked file should be removable");
                    removed = true;
                }
            }
        }
        if removed {
            assert!(jobs.iter().all(|(id, _)| id != &key));
        }
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !output.exists(),
        "unlocked selected torrent file survived deletion"
    );
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"unmodified");
    // A successfully removed session must not return after restart.
    drop(engine);
    let restarted = TorrentEngine::new(client_dir).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        restarted.poll().0.is_empty(),
        "deleted session resurrected after restart"
    );
    eprintln!(
        "STAGE20_REAL_FILE_DELETE_LOCK_PASS bytes={} retained_on_failure=true retry_deleted=true no_ghost_row=true sentinel_unchanged=true",
        data.len()
    );
    drop(restarted);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// librqbit can ignore failed session.json flushes after deleting an in-memory
/// torrent. Hold a genuine Windows no-delete lock on the JSON state to ensure
/// NSL checks the persistence path BEFORE dropping the resumable session.
#[cfg(windows)]
#[test]
#[ignore = "Windows localhost-only real locked persistence database preflight"]
fn stage20_locked_persistence_file_preserves_session_until_retry() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let original = (0..2 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(67).wrapping_add((i / 2048) as u32) as u8)
        .collect::<Vec<_>>();
    let source = seed_dir.join("stage20-session-lock.bin");
    std::fs::write(&source, &original).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&source, &seed_dir, 1024 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage20 persistence lock",
            &magnet,
            PathBuf::from("session-locked"),
        )
        .unwrap();
    wait_for(&engine, Duration::from_secs(25), |id, p| {
        id == key && p.finished
    });
    let payload = client_dir
        .join("session-locked")
        .join("stage20-session-lock.bin");
    assert_eq!(std::fs::read(&payload).unwrap(), original);
    let db = TorrentEngine::session_state_path(&client_dir).join("session.json");
    let db_before = std::fs::read(&db).unwrap();
    let no_delete_guard = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&db)
        .expect("failed to hold the test-only persistence JSON lock");
    engine.cancel(&key, false);
    let started = Instant::now();
    let mut rejected = false;
    while !rejected {
        let (jobs, events) = engine.poll();
        assert!(
            jobs.iter().any(|(id, p)| id == &key && p.finished),
            "failed persistence preflight hid a completed download"
        );
        for event in events {
            if let EngineNotice::Command {
                key: id,
                action: "remove",
                result,
            } = event
            {
                if id == key {
                    let error = result.expect_err("locked session.json was silently deleted");
                    assert!(error.contains("세션 기록 파일이 사용 중입니다"), "{error}");
                    rejected = true;
                }
            }
        }
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&db).unwrap(), db_before);
    assert_eq!(std::fs::read(&payload).unwrap(), original);
    drop(no_delete_guard);
    engine.cancel(&key, false);
    let started = Instant::now();
    let mut removed = false;
    while !removed {
        let (jobs, events) = engine.poll();
        for event in events {
            if let EngineNotice::Command {
                key: id,
                action: "remove",
                result,
            } = event
            {
                if id == key {
                    result.expect("unlocked persistence file should allow deletion");
                    removed = true;
                }
            }
        }
        if removed {
            assert!(jobs.iter().all(|(id, _)| id != &key));
        }
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(engine);
    let restored = TorrentEngine::new(client_dir).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        restored.poll().0.is_empty(),
        "stale torrent restored after session delete"
    );
    assert_eq!(std::fs::read(&payload).unwrap(), original);
    eprintln!(
        "STAGE20_SESSION_JSON_LOCK_PASS bytes={} failed_delete_preserved=true retry_clean=true no_ghost_session=true",
        original.len()
    );
    drop(restored);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// A true librqbit `ManagedTorrentState::Error`, not an NSL watcher stub:
/// Windows refuses access to the genuine *.bitv resume file during the
/// asynchronous initial integrity check of a persisted localhost transfer.
/// After releasing that OS lock, Retry must restart the real torrent from
/// already verified pieces and complete byte-for-byte.
#[cfg(windows)]
#[test]
#[ignore = "Windows localhost-only real resume-storage I/O failure and recovery"]
fn stage21_real_librqbit_error_from_locked_resume_storage_recovers() {
    use std::os::windows::fs::OpenOptionsExt;

    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(client_dir.parent().unwrap()).unwrap();
    std::fs::write(
        client_dir
            .parent()
            .unwrap()
            .join(".stage21-enable-test-fastresume"),
        b"test-only",
    )
    .unwrap();
    std::fs::create_dir_all(&seed_dir).unwrap();
    let payload = (0..6 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(131).wrapping_add((i / 4096) as u32) as u8)
        .collect::<Vec<_>>();
    let source = seed_dir.join("stage21-storage.bin");
    std::fs::write(&source, &payload).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&source, &seed_dir, 768 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage21 storage recovery",
            &magnet,
            PathBuf::from("storage-fault"),
        )
        .unwrap();
    let (_, partial) = wait_for(&engine, Duration::from_secs(30), |id, progress| {
        id == key && progress.progress_bytes >= 512 * 1024 && !progress.finished
    });
    let output = client_dir.join("storage-fault").join("stage21-storage.bin");
    assert!(output.exists());
    drop(engine);

    let state_dir = TorrentEngine::session_state_path(&client_dir);
    let bitv = std::fs::read_dir(&state_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "bitv"))
        .expect("persistent resume bitfield not present");
    let started = Instant::now();
    let exclusive_guard = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&bitv)
        {
            Ok(file) => break file,
            Err(error) => {
                assert!(
                    started.elapsed() < Duration::from_secs(12),
                    "resume file lock timeout: {error}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };

    let recovered_engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let started = Instant::now();
    let mut failed = None;
    while failed.is_none() {
        let (jobs, events) = recovered_engine.poll();
        for (id, progress) in jobs {
            if id.starts_with('r') && progress.error.is_some() {
                failed = Some((id, progress));
            }
        }
        for event in events {
            if let EngineNotice::Download {
                result: Err(error), ..
            } = event
            {
                eprintln!("STAGE21_REAL_LIBRQBIT_ERROR={error}");
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "locked bitfield did not reach true error state"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (restored_key, failure) = failed.unwrap();
    assert_eq!(failure.state, "오류");
    assert!(
        failure
            .error
            .as_deref()
            .is_some_and(|error| error.contains("loading have_pieces")
                || error.contains("bitfield")
                || error.contains("bitv")),
        "unexpected error type: {failure:?}"
    );
    eprintln!(
        "STAGE21_REAL_LIBRQBIT_ERROR_DETECTED partial={} error={:?}",
        partial.progress_bytes, failure.error
    );
    drop(exclusive_guard);

    recovered_engine.retry(&restored_key);
    let retry_start = Instant::now();
    let mut accepted = false;
    while !accepted {
        let (_, events) = recovered_engine.poll();
        for event in events {
            if let EngineNotice::Command {
                key: id,
                action: "retry",
                result,
            } = event
            {
                if id == restored_key {
                    result.expect("real librqbit storage fault did not accept retry");
                    accepted = true;
                }
            }
        }
        assert!(
            retry_start.elapsed() < Duration::from_secs(12),
            "no retry response after releasing storage lock"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, complete) = wait_for(
        &recovered_engine,
        Duration::from_secs(50),
        |id, progress| id == restored_key && progress.finished,
    );
    assert_eq!(complete.progress_bytes, payload.len() as u64);
    assert_eq!(std::fs::read(&output).unwrap(), payload);
    recovered_engine.cancel(&restored_key, false);
    let started = Instant::now();
    while recovered_engine
        .poll()
        .0
        .iter()
        .any(|(id, _)| id == &restored_key)
    {
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&output).unwrap(), payload);
    eprintln!("STAGE21_REAL_STORAGE_ERROR_RECOVERED bytes={} genuine_error=true retry=true kept_files=true", payload.len());
    drop(recovered_engine);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// librqbit 9.1.0 silently ignores a failed session.json rename after it has
/// already removed the torrent from its in-memory session. Force a *real*
/// Windows rename sharing violation between NSL's preflight and the delete,
/// then verify that a durable deletion intent prevents any ghost transfer on
/// the next launch. Payload bytes are never removed during recovery.
#[cfg(windows)]
#[test]
#[ignore = "Windows localhost-only persistence rename race and durable recovery"]
fn stage21_late_session_json_rename_failure_recovers_without_ghost_or_lost_files() {
    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let payload = (0..2 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(31).wrapping_add((i / 1024) as u32) as u8)
        .collect::<Vec<_>>();
    let source = seed_dir.join("stage21-rename.bin");
    std::fs::write(&source, &payload).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&source, &seed_dir, 1024 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage21 persistence race",
            &magnet,
            PathBuf::from("json-race"),
        )
        .unwrap();
    wait_for(&engine, Duration::from_secs(30), |id, p| {
        id == key && p.finished
    });
    let path = client_dir.join("json-race").join("stage21-rename.bin");
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    let persistence = TorrentEngine::session_state_path(&client_dir);
    let db = persistence.join("session.json");
    let started = Instant::now();
    // `wait_for` drains completion notices as well as progress. Observe the
    // persistent paused flag, not a previously consumed completion event.
    loop {
        let ready = std::fs::read(&db)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|json| {
                json["torrents"]
                    .as_object()
                    .and_then(|torrents| torrents.values().next())
                    .and_then(|entry| entry["is_paused"].as_bool())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    let before = std::fs::read(&db).unwrap();
    let before_json: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(before_json["torrents"].as_object().unwrap().len(), 1);

    engine.inject_late_session_json_rename_denial(&key).unwrap();
    engine.cancel(&key, false);
    let started = Instant::now();
    let mut failure_reported = false;
    while !failure_reported {
        let (jobs, notices) = engine.poll();
        for notice in notices {
            if let EngineNotice::Command {
                key: id,
                action: "remove",
                result,
            } = notice
            {
                if id == key {
                    let error = result.expect_err("rqbit silently ignored late JSON rename fault");
                    assert!(
                        error.contains("세션 기록 저장에 실패"),
                        "unexpected error: {error}"
                    );
                    failure_reported = true;
                }
            }
        }
        if failure_reported {
            assert!(jobs.iter().all(|(id, _)| id != &key));
        }
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    // This is an actual on-disk persistence failure: the serialized torrent
    // entry is still there and would otherwise return as a ghost on restart.
    assert_eq!(std::fs::read(&db).unwrap(), before);
    let pending = removal_journal::pending(&persistence).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    eprintln!(
        "STAGE21_LATE_JSON_FLUSH_FAULT_CONFIRMED bytes={} persisted_ghost=true journal=true",
        payload.len()
    );

    drop(engine);
    let restored = TorrentEngine::new(client_dir).unwrap();
    let started = Instant::now();
    loop {
        let journal_empty = removal_journal::pending(&persistence).unwrap().is_empty();
        let disk_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&db).unwrap()).unwrap();
        let disk_empty = disk_json["torrents"].as_object().unwrap().is_empty();
        let no_ghost_row = restored.poll().0.is_empty();
        if journal_empty && disk_empty && no_ghost_row {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "journal recovery did not clear persisted ghost"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    eprintln!("STAGE21_LATE_JSON_FLUSH_RECOVERED restart=true ghost=false journal_cleared=true payload_preserved=true");
    drop(restored);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Windows LockFileEx blocks writes by other file handles without changing
/// ACLs or truncating downloaded data. This forces librqbit's actual
/// FileOps::write_chunk -> pwrite_all -> fatal-errors receiver path, not a
/// synthetic watcher failure. Unlock, retry, checksum and preserve output.
#[cfg(windows)]
#[test]
#[ignore = "localhost-only real Windows locked payload write / fatal retry E2E"]
fn stage22_real_librqbit_live_payload_write_failure_recovers() {
    use fs2::FileExt as _;
    use std::os::windows::fs::OpenOptionsExt;

    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let payload = (0..8 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(79).wrapping_add((i / 8192) as u32) as u8)
        .collect::<Vec<_>>();
    let seed_file = seed_dir.join("stage22-write.bin");
    std::fs::write(&seed_file, &payload).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&seed_file, &seed_dir, 768 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage22 real payload fault",
            &magnet,
            PathBuf::from("locked-write"),
        )
        .unwrap();
    let (_, partial) = wait_for(&engine, Duration::from_secs(30), |id, progress| {
        id == key && progress.progress_bytes >= 256 * 1024 && !progress.finished
    });
    let output = client_dir.join("locked-write").join("stage22-write.bin");
    use std::fs::OpenOptions;
    let guard = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0x1 | 0x2 | 0x4)
        .open(&output)
        .unwrap();
    guard
        .try_lock_exclusive()
        .expect("could not acquire real Windows byte-range write denial");
    let started = Instant::now();
    let mut saw_actual_error = None;
    while saw_actual_error.is_none() {
        let (jobs, notices) = engine.poll();
        for (id, job) in jobs {
            if id == key && job.error.is_some() {
                saw_actual_error = job.error;
            }
        }
        for notice in notices {
            if let EngineNotice::Download {
                key: id,
                result: Err(error),
            } = notice
            {
                if id == key {
                    eprintln!("STAGE22_FATAL_WRITE_NOTICE={error}");
                }
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "real Windows byte-range lock did not trigger librqbit's fatal write state"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let message = saw_actual_error.unwrap();
    eprintln!(
        "STAGE22_FATAL_WRITE_CONFIRMED partial={} error={message}",
        partial.progress_bytes
    );
    assert!(
        message.contains("lock")
            || message.contains("write")
            || message.contains("Lock")
            || message.contains("chunk")
            || message.contains("os error 33"),
        "unexpected live fatal error: {message}"
    );
    fs2::FileExt::unlock(&guard).unwrap();
    drop(guard);

    engine.retry(&key);
    let retry_start = Instant::now();
    let mut accepted = false;
    while !accepted {
        let (_, notices) = engine.poll();
        for notice in notices {
            if let EngineNotice::Command {
                key: id,
                action: "retry",
                result,
            } = notice
            {
                if id == key {
                    result.expect("live pwrite_all failure was not retryable");
                    accepted = true;
                }
            }
        }
        assert!(retry_start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, completed) = wait_for(&engine, Duration::from_secs(50), |id, progress| {
        id == key && progress.finished
    });
    assert_eq!(completed.progress_bytes, payload.len() as u64);
    assert_eq!(std::fs::read(&output).unwrap(), payload);
    engine.cancel(&key, false);
    let started = Instant::now();
    while engine.poll().0.iter().any(|(id, _)| id == &key) {
        assert!(started.elapsed() < Duration::from_secs(12));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&output).unwrap(), payload);
    eprintln!(
        "STAGE22_REAL_PWRITE_FATAL_RECOVERED bytes={} retry=true exact_bytes=true payload_preserved=true",
        payload.len()
    );
    drop(engine);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Treat a durable deletion intent as if the process exited immediately
/// after journal fsync and before Session::delete. The first run supplies a
/// mismatched folder to prove unrelated torrents are never removed; the
/// second run supplies the exact identity and must clear the stale entry.
#[test]
#[ignore = "localhost-only simulated journal-before-delete crash boundary"]
fn stage22_journal_crash_boundary_rejects_wrong_folder_and_recovers_exact_match() {
    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let data = (0..2 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(53).wrapping_add((i / 2048) as u32) as u8)
        .collect::<Vec<_>>();
    let source = seed_dir.join("stage22-crash.bin");
    std::fs::write(&source, &data).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&source, &seed_dir, 1024 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage22 journal boundary",
            &magnet,
            PathBuf::from("retained"),
        )
        .unwrap();
    wait_for(&engine, Duration::from_secs(25), |id, job| {
        id == key && job.finished
    });
    let output = client_dir.join("retained").join("stage22-crash.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);
    let state = TorrentEngine::session_state_path(&client_dir);
    let db = state.join("session.json");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&db).unwrap()).unwrap();
    let torrents = json["torrents"].as_object().unwrap();
    assert_eq!(torrents.len(), 1);
    let (id_string, serialized) = torrents.iter().next().unwrap();
    let exact = removal_journal::RemovalIntent {
        id: id_string.parse::<usize>().unwrap(),
        info_hash: serialized["info_hash"].as_str().unwrap().to_owned(),
        output_folder: PathBuf::from(serialized["output_folder"].as_str().unwrap()),
    };
    assert_eq!(exact.output_folder, client_dir.join("retained"));
    let unrelated = removal_journal::RemovalIntent {
        output_folder: client_dir.join("not-this-torrent"),
        ..exact.clone()
    };
    removal_journal::begin_intent(&state, unrelated).unwrap();
    drop(engine);

    let first_restart = TorrentEngine::new(client_dir.clone()).unwrap();
    let (restored_key, _) = wait_for(&first_restart, Duration::from_secs(30), |id, job| {
        id.starts_with('r') && job.finished
    });
    assert!(removal_journal::pending(&state).unwrap().is_empty());
    assert_eq!(std::fs::read(&output).unwrap(), data);
    eprintln!("STAGE22_REUSED_ID_HASH_WRONG_FOLDER_GUARD=PASS key={restored_key}");

    // Simulate the precise fsynced-intent-to-crash boundary (without using
    // a real taskkill, which could interfere with another running process).
    removal_journal::begin_intent(&state, exact).unwrap();
    assert_eq!(removal_journal::pending(&state).unwrap().len(), 1);
    drop(first_restart);
    let final_restart = TorrentEngine::new(client_dir).unwrap();
    let started = Instant::now();
    loop {
        let no_intents = removal_journal::pending(&state).unwrap().is_empty();
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&db).unwrap()).unwrap();
        let no_session = persisted["torrents"].as_object().unwrap().is_empty();
        let no_row = final_restart.poll().0.is_empty();
        if no_intents && no_session && no_row {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "journal crash recovery timed out"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read(&output).unwrap(), data);
    eprintln!("STAGE22_JOURNAL_CRASH_BOUNDARY_RECOVERED bytes={} wrong_folder_protected=true exact_reconciled=true files_preserved=true", data.len());
    drop(final_restart);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Two simultaneous remove commands must not both mutate the same journal
/// and Session::delete. Exactly one succeeds, one safely rejects, output is
/// unchanged, and the next run has no stale session or pending intent.
#[test]
#[ignore = "localhost-only concurrent duplicate torrent removal"]
fn stage22_duplicate_remove_commands_serialized_without_ghost_or_data_loss() {
    let temp = tempfile::tempdir().unwrap();
    let seed_dir = temp.path().join("seed");
    let client_dir = temp.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let data = (0..2 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(97).wrapping_add((i / 2048) as u32) as u8)
        .collect::<Vec<_>>();
    let seed = seed_dir.join("stage22-duplicate.bin");
    std::fs::write(&seed, &data).unwrap();
    let (runtime, seed_session, _seed_handle, tracker, magnet) =
        local_file_seeder(&seed, &seed_dir, 1024 * 1024);
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download(
            "stage22 duplicate removal",
            &magnet,
            PathBuf::from("double"),
        )
        .unwrap();
    wait_for(&engine, Duration::from_secs(25), |id, job| {
        id == key && job.finished
    });
    let output = client_dir.join("double").join("stage22-duplicate.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);
    engine.cancel(&key, false);
    engine.cancel(&key, false);
    let mut success = 0;
    let mut rejected = 0;
    let started = Instant::now();
    while success + rejected < 2 {
        let (_, notices) = engine.poll();
        for notice in notices {
            if let EngineNotice::Command {
                key: id,
                action: "remove",
                result,
            } = notice
            {
                if id == key {
                    match result {
                        Ok(()) => success += 1,
                        Err(_) => rejected += 1,
                    }
                }
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "concurrent remove notices missing"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!((success, rejected), (1, 1));
    assert!(engine.poll().0.iter().all(|(id, _)| id != &key));
    assert_eq!(std::fs::read(&output).unwrap(), data);
    let state = TorrentEngine::session_state_path(&client_dir);
    assert!(removal_journal::pending(&state).unwrap().is_empty());
    drop(engine);
    let restarted = TorrentEngine::new(client_dir).unwrap();
    std::thread::sleep(Duration::from_millis(350));
    assert!(restarted.poll().0.is_empty());
    assert_eq!(std::fs::read(&output).unwrap(), data);
    eprintln!("STAGE22_DUPLICATE_CANCEL_SAFE bytes={} success=1 rejected=1 no_ghost=true payload_preserved=true", data.len());
    drop(restarted);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Longer loopback-only transfer with a real seeder interruption. The pause
/// models a disconnected/unavailable peer without changing any user network
/// adapters or touching any non-test torrent process or directory.
#[test]
#[ignore = "localhost-only 16 MiB transfer, seeder disconnect and resume (run explicitly)"]
fn larger_transfer_survives_local_seeder_outage_and_restores_exact_bytes() {
    let root = tempfile::tempdir().unwrap();
    let seed_dir = root.path().join("seed");
    let client_dir = root.path().join("client").join("downloads");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let data = (0..16 * 1024 * 1024)
        .map(|i| (i as u32).wrapping_mul(131).wrapping_add((i / 8192) as u32) as u8)
        .collect::<Vec<_>>();
    let file = seed_dir.join("outage-test.bin");
    std::fs::write(&file, &data).unwrap();
    let (runtime, seed_session, seed_handle, tracker, magnet) =
        local_file_seeder(&file, &seed_dir, 768 * 1024);

    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download("outage-only QA", &magnet, PathBuf::from("isolated-outage"))
        .unwrap();
    let started = Instant::now();
    let (_, first) = wait_for(&engine, Duration::from_secs(35), |id, progress| {
        id == key && progress.progress_bytes > 0 && !progress.finished
    });
    assert!(first.progress_bytes < data.len() as u64);
    runtime.block_on(seed_session.pause(&seed_handle)).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let (during_outage, _) = engine.poll();
    let current = during_outage
        .iter()
        .find(|(id, _)| id == &key)
        .map(|(_, status)| status)
        .expect("interrupted download should stay visible for retry");
    assert!(
        !current.finished,
        "outage should not mark an incomplete download as done"
    );
    assert!(
        current.error.is_none(),
        "temporary outage became fatal: {current:?}"
    );
    eprintln!(
        "TORRENT_PEER_OUTAGE first_bytes={} outage_bytes={} elapsed_s={:.2}",
        first.progress_bytes,
        current.progress_bytes,
        started.elapsed().as_secs_f64()
    );

    runtime
        .block_on(seed_session.unpause(&seed_handle))
        .unwrap();
    let (_, complete) = wait_for(&engine, Duration::from_secs(90), |id, progress| {
        id == key && progress.finished
    });
    assert_eq!(complete.total_bytes, data.len() as u64);
    assert_eq!(complete.progress_bytes, data.len() as u64);
    let output = client_dir.join("isolated-outage").join("outage-test.bin");
    assert_eq!(std::fs::read(&output).unwrap(), data);
    eprintln!(
        "TORRENT_OUTAGE_RECOVERY_PASS bytes={} elapsed_s={:.2}",
        data.len(),
        started.elapsed().as_secs_f64()
    );
    drop(engine);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}

/// Generated files + loopback tracker only. Verify that a nested torrent
/// preview does not download data until confirmation, and that only the
/// chosen file is transferred afterwards.
#[test]
#[ignore = "localhost-only nested torrent preview and selective transfer"]
fn nested_magnet_preview_downloads_only_selected_files() {
    let root = tempfile::tempdir().unwrap();
    let seed_dir = root.path().join("seeder");
    let client_dir = root.path().join("client").join("downloads");
    for relative in [
        "base/game.bin",
        "dlc/extra/content.bin",
        "dlc/extra/extra.txt",
    ] {
        let path = seed_dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data = (0..180 * 1024)
            .map(|i| (i as u32).wrapping_mul(17) as u8 ^ relative.len() as u8)
            .collect::<Vec<_>>();
        std::fs::write(path, data).unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (seed_session, peer, magnet) = runtime.block_on(async {
        let torrent = librqbit::create_torrent(
            &seed_dir,
            CreateTorrentOptions {
                piece_length: Some(32 * 1024),
                ..Default::default()
            },
            &BlockingSpawner::new(1),
        )
        .await
        .unwrap();
        let session = Session::new_with_opts(
            seed_dir.clone(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                listen: Some(ListenerOptions {
                    mode: ListenerMode::TcpOnly,
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(torrent.as_bytes().unwrap()),
                Some(AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(seed_dir.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .into_handle()
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while !handle.stats().finished {
            assert!(
                tokio::time::Instant::now() < deadline,
                "generated seeder did not verify all files: {:?}",
                handle.stats()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let peer = session.listen_addr().unwrap();
        let magnet = librqbit::Magnet::from_id20(torrent.info_hash(), vec![], None).to_string();
        (session, peer, magnet)
    });
    let tracker = LocalTracker::new(peer);
    let magnet = format!(
        "{}&tr={}",
        magnet,
        url::form_urlencoded::byte_serialize(tracker.url.as_bytes()).collect::<String>()
    );
    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let request = engine.preview_files(&magnet).unwrap();
    let preview = request
        .receiver
        .recv_timeout(Duration::from_secs(45))
        .expect("local metadata preview timeout")
        .expect("local metadata preview failure");
    assert_eq!(preview.files.len(), 3);
    assert!(
        !client_dir.join("selected").exists(),
        "preview wrote download files"
    );
    let wanted = preview
        .files
        .iter()
        .find(|file| file.path.ends_with("dlc/extra/content.bin"))
        .expect("nested torrent file missing")
        .clone();
    let ignored_files = preview
        .files
        .iter()
        .filter(|file| file.index != wanted.index)
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let picker = crate::torrent_file_picker::TorrentFilePicker::new(preview).unwrap();
    assert_eq!(picker.selected_count(), 3);
    let metadata = picker.preview.torrent_bytes.clone();
    drop(picker);
    drop(request);
    let key = engine
        .start_download_selected(
            "nested local test",
            &magnet,
            PathBuf::from("selected"),
            Some(metadata),
            Some(vec![wanted.index]),
        )
        .unwrap();
    wait_for(&engine, Duration::from_secs(35), |id, progress| {
        id == key && progress.finished
    });
    let output = client_dir.join("selected");
    assert_eq!(
        std::fs::read(output.join(&wanted.path)).unwrap(),
        std::fs::read(seed_dir.join(&wanted.path)).unwrap()
    );
    // BitTorrent transfers pieces, not arbitrary bytes: cross-file pieces
    // can legitimately create partial payloads in unchecked files. Keep
    // them to preserve verification of the selected file after restarting.
    for path in &ignored_files {
        let partial = output.join(path);
        if partial.is_file() {
            assert_ne!(
                std::fs::read(&partial).unwrap(),
                std::fs::read(seed_dir.join(path)).unwrap(),
                "an unchecked file was fully transferred"
            );
        }
    }
    eprintln!(
        "NESTED_TORRENT_SELECTIVE_TRANSFER=PASS selected_index={}",
        wanted.index
    );
    drop(engine);
    // The existing rqbit resume metadata retains the original only_files
    // selection, and the boundary-piece data keeps fast-resume valid.
    let restored = TorrentEngine::new(client_dir.clone()).unwrap();
    let (restored_key, restored_progress) =
        wait_for(&restored, Duration::from_secs(15), |id, p| {
            id.starts_with('r') && p.finished
        });
    assert!(restored_key.starts_with('r'));
    assert!(restored_progress.finished);
    assert_eq!(
        std::fs::read(output.join(&wanted.path)).unwrap(),
        std::fs::read(seed_dir.join(&wanted.path)).unwrap(),
    );
    drop(restored);
    drop(seed_session);
    drop(runtime);
    drop(tracker);
}
