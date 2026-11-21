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
