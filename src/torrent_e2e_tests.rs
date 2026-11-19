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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (seed_session, peer, magnet) = runtime.block_on(async {
        let fixture = librqbit::create_torrent(
            &seed_dir.join("payload.bin"),
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
                ratelimits: LimitsConfig {
                    upload_bps: NonZeroU32::new(512 * 1024),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let torrent = session
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
        let start = tokio::time::Instant::now();
        while !torrent.stats().finished {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "seeder not ready"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let peer = session.listen_addr().expect("loopback peer address");
        let magnet = librqbit::Magnet::from_id20(fixture.info_hash(), vec![], None).to_string();
        (session, peer, magnet)
    });
    assert!(peer.ip().is_loopback());
    let tracker = LocalTracker::new(peer);
    let magnet = format!(
        "{}&tr={}",
        magnet,
        url::form_urlencoded::byte_serialize(tracker.url.as_bytes()).collect::<String>()
    );

    let engine = TorrentEngine::new(client_dir.clone()).unwrap();
    let key = engine
        .start_download("local QA", &magnet, PathBuf::from("sample"))
        .unwrap();
    let (_, first) = wait_for(&engine, Duration::from_secs(30), |id, p| {
        id == key && p.progress_bytes > 0 && !p.finished
    });
    eprintln!("STAGE9_TORRENT_FIRST={}", first.progress_bytes);
    engine.pause(&key);
    let (_, paused) = wait_for(&engine, Duration::from_secs(12), |id, p| {
        id == key && p.paused
    });
    assert!(paused.progress_bytes >= first.progress_bytes);
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
