use super::*;

#[test]
fn catalog_refresh_moves_verified_state_to_ui_without_reloading_saved_json() {
    let (mut app, _private) = seeded_app();
    app.torrent_engine = None;
    let (sender, receiver) = std::sync::mpsc::channel();
    app.catalog_result = Some(receiver);
    sender
        .send(Ok(catalog::CatalogState {
            items: vec![catalog::CatalogItem {
                title: "Already current".into(),
                title_id: Some("0100D2C0115B6000".into()),
                magnet: "magnet:?xt=urn:btih:ABC".into(),
                ..Default::default()
            }],
            source_url: catalog::DEFAULT_CATALOG_URL.into(),
            updated_at: None,
            etag: Some("\"unit-304\"".into()),
            not_modified: true,
        }))
        .unwrap();
    // No catalog snapshot is saved in this test, so the UI must take the
    // already-parsed result directly from the background worker.
    assert!(catalog::load(&app.base_dir).is_none());
    run_app_ticks(&mut app, 2);
    assert_eq!(app.catalog_state.items.len(), 1);
    assert_eq!(app.catalog_state.items[0].title, "Already current");
    assert!(app.status.contains("이미 최신"), "status: {}", app.status);
    assert!(app.catalog_result.is_none());
}

#[test]
fn normalizes_windows_scan_paths_for_existing_database_keys() {
    #[cfg(windows)]
    assert_eq!(
        normalize_scan_path_text(r"e:/NSW\games/sample.nsp"),
        r"E:\NSW\games\sample.nsp"
    );

    #[cfg(not(windows))]
    assert_eq!(
        normalize_scan_path_text("/games/sample.nsp"),
        "/games/sample.nsp"
    );
}

#[test]
fn missing_updates_catalog_batch_only_offers_matching_title_ids() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut base = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    base.latest_version = 196_608;
    base.update_version = 65_536;
    let eligible = catalog::CatalogItem {
        title: "Alpha release".into(),
        title_id: Some(base.title_id.clone()),
        topic_id: "123456".into(),
        magnet: "magnet:?xt=urn:btih:ABCD".into(),
        ..Default::default()
    };
    let wrong = catalog::CatalogItem {
        title: "Different base".into(),
        title_id: Some("0100AABBCCDDE000".into()),
        topic_id: "123455".into(),
        magnet: "magnet:?xt=urn:btih:EFGH".into(),
        ..Default::default()
    };
    let batch = update_batch::new_update_batch(&[base.clone()], &[wrong, eligible.clone()], "");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].candidates, vec![1]);
    assert_eq!(
        batch.eligible(),
        0,
        "metadata verification must happen first"
    );
    let excluded = update_batch::new_update_batch(
        std::slice::from_ref(&base),
        std::slice::from_ref(&eligible),
        &base.title_id,
    );
    assert!(excluded.rows.is_empty());

    app.rows = vec![base];
    app.rows_generation += 1;
    app.folder_filter = "MISSING_UPDATES".into();
    app.kind_filter = "base".into();
    app.torrent_engine = None;
    app.update_batch = Some(batch);
    app.update_batch.as_mut().unwrap().running = false;
    let ctx = run_app_ticks(&mut app, 2);
    let rect = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_update_batch_toolbar"))
            .expect("missing-updates-only catalog toolbar not drawn")
    });
    assert!(rect.width() > 100.0 && rect.height() > 0.0);
}

#[test]
fn missing_dlc_audit_is_per_title_id_and_toolbar_is_visible() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut base = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    let number = u64::from_str_radix(&base.title_id, 16).unwrap() & !0x1fff;
    let id1 = format!("{:016X}", number | 0x1001);
    let id2 = format!("{:016X}", number | 0x1002);
    base.missing_dlcs = vec![
        (id1.clone(), "Extra One".into()),
        (id2.clone(), "Extra Two".into()),
    ];
    let matching = catalog::CatalogItem {
        title: "Extra DLC collection".into(),
        title_id: Some(id1.clone()),
        topic_id: "12345".into(),
        magnet: "magnet:?xt=urn:btih:ABCD".into(),
        ..Default::default()
    };
    let batch = update_batch::new_dlc_batch(std::slice::from_ref(&base), &[matching], &id2);
    assert_eq!(batch.kind, update_batch::BatchKind::Dlc);
    assert_eq!(batch.rows.len(), 1, "excluded DLC is skipped independently");
    assert_eq!(batch.rows[0].dlc_id.as_deref(), Some(id1.as_str()));
    assert_eq!(batch.rows[0].candidates, vec![0]);
    app.rows = vec![base];
    app.rows_generation += 1;
    app.folder_filter = "MISSING_DLCS".into();
    app.kind_filter = "base".into();
    app.update_batch = Some(batch);
    app.update_batch.as_mut().unwrap().running = false;
    let ctx = run_app_ticks(&mut app, 2);
    let rect = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_update_batch_toolbar"))
    });
    assert!(rect.is_some_and(|rect| rect.width() > 100.0 && rect.height() > 0.0));
}

#[test]
fn two_missing_dlcs_reuse_one_catalog_torrent_preview() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut game = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    let base = u64::from_str_radix(&game.title_id, 16).unwrap() & !0x1fff;
    let id1 = format!("{:016X}", base | 0x1001);
    let id2 = format!("{:016X}", base | 0x1002);
    game.missing_dlcs = vec![(id1.clone(), "One".into()), (id2.clone(), "Two".into())];
    let catalog = catalog::CatalogItem {
        title: "DLC bundle".into(),
        title_id: Some(game.title_id.clone()),
        topic_id: "123".into(),
        magnet: "magnet:?xt=urn:btih:ABCD".into(),
        ..Default::default()
    };
    let mut batch = update_batch::new_dlc_batch(&[game], std::slice::from_ref(&catalog), "");
    let (request, sender, _cancelled) = torrent::synthetic_preview_request();
    batch.rows[0].next_candidate = 1;
    batch.active = Some((0, request));
    sender
        .send(Ok(torrent::TorrentPreview {
            files: vec![
                torrent::TorrentFileEntry {
                    index: 0,
                    path: format!("DLC [{id1}][v0].nsp").into(),
                    size: 120,
                },
                torrent::TorrentFileEntry {
                    index: 1,
                    path: format!("DLC [{id2}][v0].nsp").into(),
                    size: 180,
                },
            ],
            torrent_bytes: vec![11, 22, 33],
        }))
        .unwrap();
    batch.tick(&[catalog], None);
    assert!(!batch.running);
    assert_eq!(batch.eligible(), 2);
    assert_eq!(batch.preview_cache.len(), 1);
    let first = &batch.rows[0].found.as_ref().unwrap().torrent_bytes;
    let second = &batch.rows[1].found.as_ref().unwrap().torrent_bytes;
    assert!(
        Arc::ptr_eq(first, second),
        "the preview payload should be shared"
    );
    assert_eq!(
        app::update_batch::grouped_batch_downloads(&batch),
        vec![vec![0, 1]],
        "two DLCs in the same torrent must be one selected-files transfer"
    );
    batch.rows[1].found.as_mut().unwrap().release.magnet =
        "magnet:?xt=urn:btih:ABCD&tr=udp://different-tracker".into();
    assert_eq!(
        app::update_batch::grouped_batch_downloads(&batch),
        vec![vec![0, 1]],
        "same infohash with different tracker URLs must still be grouped"
    );
    batch.rows[1].found.as_mut().unwrap().release.magnet = "magnet:?xt=urn:btih:EFGH".into();
    assert_eq!(
        app::update_batch::grouped_batch_downloads(&batch),
        vec![vec![0], vec![1]],
        "different torrent hashes must not share a transfer"
    );
    batch.rows[1].found.as_mut().unwrap().release.magnet = "magnet:?xt=urn:btih:ABCD".into();
    batch.rows[1].checked = false;
    assert_eq!(
        app::update_batch::grouped_batch_downloads(&batch),
        vec![vec![0]],
        "unchecked DLC must not be downloaded"
    );
    for row in &mut batch.rows {
        row.job = Some(update_batch::UpdateBatchJob {
            key: "t-group".into(),
            directory: app.base_dir.join("downloads").join("batch-group"),
            output: app
                .base_dir
                .join("downloads")
                .join("batch-group")
                .join("dlc.nsp"),
            expected_version: 0,
            expected_file_size: 120,
            original_size: 500,
            transfer_finished: false,
            recovering_since: None,
            status: update_batch::UpdateBatchJobStatus::Queued,
            verifying: None,
            merge_control: None,
        });
    }
    app.update_batch = Some(batch);
    app.handle_missing_update_completion("t-group", &Ok(()));
    for row in &app.update_batch.as_ref().unwrap().rows {
        let job = row.job.as_ref().unwrap();
        assert!(
            job.transfer_finished,
            "every DLC sharing one torrent must complete"
        );
        assert_eq!(job.status, update_batch::UpdateBatchJobStatus::Queued);
    }
}

#[test]
fn dlc_merges_wait_for_prior_package_and_use_latest_published_result() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut game = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    let base = u64::from_str_radix(&game.title_id, 16).unwrap() & !0x1fff;
    game.missing_dlcs = vec![
        (format!("{:016X}", base | 0x1001), "One".into()),
        (format!("{:016X}", base | 0x1002), "Two".into()),
    ];
    let mut batch = update_batch::new_dlc_batch(&[game], &[], "");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(
        app::update_batch::cumulative_dlc_input(&batch, 1),
        Some(None)
    );
    let prior_output = app.base_dir.join("[NSL DLC] prior.nsz");
    batch.rows[0].job = Some(update_batch::UpdateBatchJob {
        key: "t5".into(),
        directory: app.base_dir.join("downloads").join("safe"),
        output: app.base_dir.join("downloads").join("safe").join("dlc.nsp"),
        expected_version: 0,
        expected_file_size: 50,
        original_size: 100,
        transfer_finished: true,
        recovering_since: None,
        status: update_batch::UpdateBatchJobStatus::Queued,
        verifying: None,
        merge_control: None,
    });
    assert_eq!(app::update_batch::cumulative_dlc_input(&batch, 1), None);
    batch.rows[0].job.as_mut().unwrap().status =
        update_batch::UpdateBatchJobStatus::Failed("no input".into());
    assert_eq!(app::update_batch::cumulative_dlc_input(&batch, 1), None);
    batch.rows[0].job.as_mut().unwrap().status =
        update_batch::UpdateBatchJobStatus::Merged(prior_output.clone());
    assert_eq!(
        app::update_batch::cumulative_dlc_input(&batch, 1),
        Some(Some(prior_output))
    );
}

#[test]
fn restored_grouped_dlcs_reconnect_by_folder_and_resume_after_download_error() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut game = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    let base = u64::from_str_radix(&game.title_id, 16).unwrap() & !0x1fff;
    game.missing_dlcs = (1..=2)
        .map(|n| {
            (
                format!("{:016X}", base | (0x1000 + n)),
                format!("Extra DLC {n}"),
            )
        })
        .collect();
    let mut batch = update_batch::new_dlc_batch(&[game], &[], "");
    batch.running = false;
    let folder = app
        .base_dir
        .join("downloads")
        .join("stage15-grouped-recovery");
    for (index, row) in batch.rows.iter_mut().enumerate() {
        row.job = Some(update_batch::UpdateBatchJob {
            key: "t1".into(),
            directory: folder.clone(),
            output: folder.join(format!("dlc{index}.nsp")),
            expected_version: 0,
            expected_file_size: 1024,
            original_size: 4096,
            transfer_finished: false,
            recovering_since: None,
            status: update_batch::UpdateBatchJobStatus::Failed("peer disconnected".into()),
            verifying: None,
            merge_control: None,
        });
    }
    update_manifest::save(&app.base_dir, &batch).unwrap();
    app.update_batch = update_manifest::load(&app.base_dir).unwrap();
    app.torrent_engine = None;
    app.torrent_progress = vec![
        (
            "t1".into(),
            torrent::TorrentProgress {
                output_folder: Some(app.base_dir.join("downloads").join("unrelated")),
                finished: true,
                ..Default::default()
            },
        ),
        (
            "r54".into(),
            torrent::TorrentProgress {
                output_folder: Some(folder.clone()),
                state: "downloading".into(),
                ..Default::default()
            },
        ),
    ];
    app.poll_missing_update_audit(&ctx);
    for row in &app.update_batch.as_ref().unwrap().rows {
        let job = row.job.as_ref().unwrap();
        assert_eq!(job.key, "r54", "old t1 must not bind unrelated torrent");
        assert_eq!(job.status, update_batch::UpdateBatchJobStatus::Downloading);
        assert!(!job.transfer_finished);
        assert!(job.verifying.is_none());
    }
    let restored = update_manifest::load(&app.base_dir).unwrap().unwrap();
    assert!(restored
        .rows
        .iter()
        .all(|row| row.job.as_ref().unwrap().key == "r54"));
    app.torrent_progress[1].1.paused = true;
    app.poll_missing_update_audit(&ctx);
    assert!(app.update_batch.as_ref().unwrap().rows.iter().all(|row| {
        row.job.as_ref().unwrap().status == update_batch::UpdateBatchJobStatus::Queued
    }));
    app.torrent_progress[1].1.paused = false;
    app.torrent_progress[1].1.error = Some("temporary failure".into());
    app.poll_missing_update_audit(&ctx);
    assert!(app.update_batch.as_ref().unwrap().rows.iter().all(|row| {
        matches!(
            row.job.as_ref().unwrap().status,
            update_batch::UpdateBatchJobStatus::Failed(_)
        )
    }));
    app.torrent_progress[1].1.error = None;
    app.poll_missing_update_audit(&ctx);
    assert!(app.update_batch.as_ref().unwrap().rows.iter().all(|row| {
        row.job.as_ref().unwrap().status == update_batch::UpdateBatchJobStatus::Downloading
    }));
    // rqbit publishes 100% BEFORE its watcher has paused the torrent and
    // released Windows file handles. Never run native verification yet.
    app.torrent_progress[1].1.finished = true;
    app.poll_missing_update_audit(&ctx);
    assert!(app.update_batch.as_ref().unwrap().rows.iter().all(|row| {
        let job = row.job.as_ref().unwrap();
        !job.transfer_finished && job.verifying.is_none()
    }));
    app.handle_missing_update_completion("r54", &Ok(()));
    assert!(app.update_batch.as_ref().unwrap().rows.iter().all(|row| {
        let job = row.job.as_ref().unwrap();
        job.transfer_finished && job.status == update_batch::UpdateBatchJobStatus::Queued
    }));
    app.poll_missing_update_audit(&ctx);
    let jobs = app
        .update_batch
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .filter_map(|row| row.job.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        jobs.iter().filter(|job| job.verifying.is_some()).count(),
        1,
        "two DLCs must not run simultaneous native merges"
    );
    assert_eq!(
        jobs.iter()
            .filter(|job| job.status == update_batch::UpdateBatchJobStatus::Queued)
            .count(),
        1
    );
}

#[test]
fn fully_merged_dlc_chain_preserves_only_final_file_and_crash_safe_journal() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut game = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    let base = u64::from_str_radix(&game.title_id, 16).unwrap() & !0x1fff;
    game.missing_dlcs = (1..=4)
        .map(|n| (format!("{:016X}", base | (0x1000 + n)), format!("DLC {n}")))
        .collect();
    let mut batch = update_batch::new_dlc_batch(&[game], &[], "");
    assert_eq!(batch.rows.len(), 4);
    batch.running = false;
    let original = app.base_dir.join("original.nsz");
    let unrelated = app.base_dir.join("user-owned.nsz");
    std::fs::write(&original, b"Original game is never deleted").unwrap();
    std::fs::write(&unrelated, b"User file is never deleted").unwrap();
    let folder = app.base_dir.join("downloads").join("stage15-merge-final");
    let mut published = Vec::new();
    for (index, row) in batch.rows.iter_mut().enumerate() {
        row.original_path = original.clone();
        if index == 3 {
            row.checked = false;
            continue;
        }
        let path = update_merge::expected_dlc_output(
            &original,
            &row.title_id,
            row.dlc_id.as_deref().unwrap(),
            0,
        )
        .unwrap();
        std::fs::write(&path, vec![(index + 1) as u8; 128]).unwrap();
        published.push(path.clone());
        row.job = Some(update_batch::UpdateBatchJob {
            key: format!("t{index}"),
            directory: folder.clone(),
            output: folder.join(format!("dlc{index}.nsp")),
            expected_version: 0,
            expected_file_size: 1024,
            original_size: 100,
            transfer_finished: true,
            recovering_since: None,
            status: update_batch::UpdateBatchJobStatus::Merged(path),
            verifying: None,
            merge_control: None,
        });
    }

    // Even one unresolved selected DLC prevents cleanup of the preceding
    // successful outputs, which may still be required as restart inputs.
    batch.rows[2].job.as_mut().unwrap().status =
        update_batch::UpdateBatchJobStatus::Failed("retry later".into());
    assert_eq!(
        app::update_batch::consolidate_finished_dlc_outputs(&app.base_dir, &mut batch).unwrap(),
        0
    );
    assert!(published.iter().all(|path| path.is_file()));
    batch.rows[2].job.as_mut().unwrap().status =
        update_batch::UpdateBatchJobStatus::Merged(published[2].clone());

    // A forged path from a persisted journal must never authorize removal of
    // a user-owned file with an unrelated name.
    batch.rows[0].job.as_mut().unwrap().status =
        update_batch::UpdateBatchJobStatus::Merged(unrelated.clone());
    assert!(
        app::update_batch::consolidate_finished_dlc_outputs(&app.base_dir, &mut batch).is_err()
    );
    assert!(unrelated.is_file() && published.iter().all(|path| path.is_file()));
    batch.rows[0].job.as_mut().unwrap().status =
        update_batch::UpdateBatchJobStatus::Merged(published[0].clone());

    // A durable journal is mandatory before destructive cleanup. Simulate a
    // journal write failure; statuses and all data must roll back unchanged.
    let journal = app.base_dir.join("batch-updates.json");
    std::fs::create_dir(&journal).unwrap();
    assert!(
        app::update_batch::consolidate_finished_dlc_outputs(&app.base_dir, &mut batch).is_err()
    );
    assert!(published.iter().all(|path| path.is_file()));
    assert_eq!(
        batch.rows[0].job.as_ref().unwrap().status,
        update_batch::UpdateBatchJobStatus::Merged(published[0].clone())
    );
    std::fs::remove_dir(&journal).unwrap();

    assert_eq!(
        app::update_batch::consolidate_finished_dlc_outputs(&app.base_dir, &mut batch).unwrap(),
        2,
        "only NSL-owned intermediate results may be removed"
    );
    assert!(!published[0].exists() && !published[1].exists());
    assert!(published[2].is_file() && original.is_file() && unrelated.is_file());
    let restored = update_manifest::load(&app.base_dir).unwrap().unwrap();
    for row in restored.rows {
        assert_eq!(
            row.job.unwrap().status,
            update_batch::UpdateBatchJobStatus::Merged(published[2].clone())
        );
    }
    assert_eq!(
        app::update_batch::consolidate_finished_dlc_outputs(&app.base_dir, &mut batch).unwrap(),
        0,
        "cleanup must be idempotent"
    );
    assert_eq!(
        app::update_batch::cumulative_dlc_input(&batch, 3),
        Some(Some(published[2].clone())),
        "new DLC additions must continue from the retained final result"
    );
}

/// Opt-in end-to-end handoff with independently verified real game content.
/// The network transfer itself is simulated by placing the authorized NSP in
/// an isolated download directory and delivering the actual completion event.
/// The production crash journal, patch verification and merge worker run.
#[test]
#[ignore = "requires read-only E:\\NSL_QA_20261120\\small real game fixtures"]
fn isolated_real_finished_transfer_restart_runs_native_nsz_merge() -> anyhow::Result<()> {
    use anyhow::Context;
    use sha2::Digest;
    use std::io::Read;
    use std::path::Path;
    fn sha256(path: &Path) -> anyhow::Result<[u8; 32]> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = sha2::Sha256::new();
        let mut bytes = [0u8; 1024 * 1024];
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                return Ok(hasher.finalize().into());
            }
            hasher.update(&bytes[..count]);
        }
    }
    let fixtures = PathBuf::from("E:\\NSL_QA_20261120\\small");
    let qa_root = PathBuf::from("E:\\NSL_QA_20261120");
    let base_source = fixtures.join("base.nsz");
    let patch_source = fixtures.join("patch.nsp");
    anyhow::ensure!(base_source.is_file() && patch_source.is_file());
    let private = tempfile::Builder::new()
        .prefix("stage15-batch-e2e-")
        .tempdir_in(&qa_root)?;
    let base = private.path().join("private_base.nsz");
    std::fs::copy(&base_source, &base)?;
    let base_hash = sha256(&base)?;
    anyhow::ensure!(base_hash == sha256(&base_source)?);
    let (mut app, _app_tmp) = seeded_app();
    app.torrent_engine = None;
    let folder = app
        .base_dir
        .join("downloads")
        .join("stage15-real-downloaded");
    std::fs::create_dir_all(&folder)?;
    let downloaded = folder.join("real_patch.nsp");
    std::fs::copy(&patch_source, &downloaded)?;
    let patch_hash = sha256(&downloaded)?;
    anyhow::ensure!(patch_hash == sha256(&patch_source)?);
    let base_id = "0100F7301AAB6000".to_string();
    let wanted = 65536;
    let batch = update_batch::UpdateBatch {
        kind: update_batch::BatchKind::Update,
        rows: vec![update_batch::UpdateBatchRow {
            library_id: 1,
            title_id: base_id.clone(),
            dlc_id: None,
            name: "Horror Tale 2 isolated Stage 15".into(),
            installed_version: 0,
            latest_version: wanted,
            original_path: base.clone(),
            original_type: "nsz".into(),
            candidates: Vec::new(),
            next_candidate: 0,
            found: None,
            checked: true,
            failure: None,
            job: Some(update_batch::UpdateBatchJob {
                key: "t-local-finished".into(),
                directory: folder,
                output: downloaded.clone(),
                expected_version: wanted,
                expected_file_size: std::fs::metadata(&downloaded)?.len(),
                original_size: std::fs::metadata(&base)?.len(),
                transfer_finished: false,
                recovering_since: None,
                status: update_batch::UpdateBatchJobStatus::Queued,
                verifying: None,
                merge_control: None,
            }),
        }],
        running: false,
        cursor: 1,
        active: None,
        confirm_queue: false,
        preview_cache: Default::default(),
    };
    app.update_batch = Some(batch);
    // Actual NSL completion handler persists "all bytes transferred".
    app.handle_missing_update_completion("t-local-finished", &Ok(()));
    let saved = update_manifest::load(&app.base_dir)?.context("missing completion journal")?;
    anyhow::ensure!(saved.rows[0].job.as_ref().unwrap().transfer_finished);
    // Simulate a crash/restart after the transfer, before the native worker.
    app.update_batch = Some(saved);
    let ctx = egui::Context::default();
    let started = std::time::Instant::now();
    let output = loop {
        app.poll_missing_update_audit(&ctx);
        let job = app.update_batch.as_ref().unwrap().rows[0]
            .job
            .as_ref()
            .unwrap();
        match &job.status {
            update_batch::UpdateBatchJobStatus::Merged(path) => break path.clone(),
            update_batch::UpdateBatchJobStatus::Failed(reason) => {
                anyhow::bail!("Stage 15 actual merge failed: {reason}")
            }
            _ => {
                anyhow::ensure!(
                    started.elapsed() < std::time::Duration::from_secs(240),
                    "Stage 15 real batch merge timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };
    anyhow::ensure!(output.is_file() && output.parent() == base.parent());
    anyhow::ensure!(output
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")));
    let keys = scanner::prod_keys().context("Stage 15 read-only prod.keys missing")?;
    let metadata_work = private.path().join("metadata");
    std::fs::create_dir(&metadata_work)?;
    let (_entries, cnmts, _legacy) = nsrp_core::ops::cnmt::discover_game_metadata(
        &output,
        &metadata_work,
        &private.path().join("external-tool-disabled.exe"),
        &keys,
        &std::sync::atomic::AtomicBool::new(false),
    )?;
    anyhow::ensure!(cnmts.iter().any(|item| {
        item.meta_type == nsrp_core::ops::cnmt::ContentMetaType::Base && item.title_id == base_id
    }));
    anyhow::ensure!(cnmts.iter().any(|item| {
        item.meta_type == nsrp_core::ops::cnmt::ContentMetaType::Update
            && item.title_id == "0100F7301AAB6800"
            && item.version >= wanted as u32
    }));
    anyhow::ensure!(sha256(&base)? == base_hash && sha256(&downloaded)? == patch_hash);
    let journal = update_manifest::load(&app.base_dir)?.context("missing final journal")?;
    anyhow::ensure!(
        journal.rows[0].job.as_ref().unwrap().status
            == update_batch::UpdateBatchJobStatus::Merged(output.clone())
    );
    println!(
        "STAGE15_OFFLINE_BATCH_E2E_PASS native_nsz={} bytes={} cnmts={} transfer_complete_journal_recovered=true sources_unchanged=true elapsed_seconds={:.2}",
        output.display(),
        std::fs::metadata(&output)?.len(),
        cnmts.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[test]
fn restored_batch_rebinds_new_torrent_key_by_output_folder_without_starting_merge() {
    let (mut app, dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.torrent_engine = None;
    let folder = app.base_dir.join("downloads").join("batch-restore-QA");
    let output = folder.join("game/update.nsz");
    let batch = update_batch::UpdateBatch {
        kind: update_batch::BatchKind::Update,
        rows: vec![update_batch::UpdateBatchRow {
            library_id: 25,
            title_id: "0100D2C0115B6000".into(),
            dlc_id: None,
            name: "QA title".into(),
            installed_version: 65536,
            latest_version: 196608,
            original_path: dir.path().join("original.nsz"),
            original_type: "nsz".into(),
            candidates: Vec::new(),
            next_candidate: 0,
            found: None,
            checked: false,
            failure: None,
            job: Some(update_batch::UpdateBatchJob {
                key: "t1".into(),
                directory: folder.clone(),
                output,
                expected_version: 196608,
                expected_file_size: 8192,
                original_size: 123456,
                transfer_finished: false,
                recovering_since: None,
                status: update_batch::UpdateBatchJobStatus::Queued,
                verifying: None,
                merge_control: None,
            }),
        }],
        running: false,
        cursor: 1,
        active: None,
        confirm_queue: false,
        preview_cache: Default::default(),
    };
    update_manifest::save(&app.base_dir, &batch).unwrap();
    app.update_batch = update_manifest::load(&app.base_dir).unwrap();
    app.torrent_progress = vec![(
        "r42".into(),
        torrent::TorrentProgress {
            output_folder: Some(folder),
            state: "downloading".into(),
            ..Default::default()
        },
    )];

    app.poll_missing_update_audit(&ctx);
    let job = app.update_batch.as_ref().unwrap().rows[0]
        .job
        .as_ref()
        .unwrap();
    assert_eq!(
        job.key, "r42",
        "restored librqbit keys replace original tNN keys"
    );
    assert_eq!(job.status, update_batch::UpdateBatchJobStatus::Downloading);
    assert!(!job.transfer_finished);
    assert!(
        job.verifying.is_none(),
        "partial transfer must never trigger package merge"
    );
    let saved = update_manifest::load(&app.base_dir).unwrap().unwrap();
    assert_eq!(saved.rows[0].job.as_ref().unwrap().key, "r42");

    // Simulate an active merge without touching any user packages. The
    // footer must reflect native worker state rather than a stuck spinner.
    let (sender, receiver) = mpsc::channel::<std::result::Result<Option<PathBuf>, String>>();
    let control = Arc::new(update_merge::MergeControl::default());
    control.stage("업데이트 병합 검증");
    let job = app.update_batch.as_mut().unwrap().rows[0]
        .job
        .as_mut()
        .unwrap();
    job.status = update_batch::UpdateBatchJobStatus::Verifying;
    job.verifying = Some(receiver);
    job.merge_control = Some(Arc::clone(&control));
    let views = app.active_progress_views();
    let active = views
        .iter()
        .find(|view| view.label == "업데이트 병합 검증")
        .unwrap();
    assert!(active.fraction.is_none());
    control.request_cancel();
    assert!(app.active_progress_views().iter().any(|view| {
        view.label == "업데이트 병합 중단 처리 중" && view.fraction.is_none()
    }));
    drop(sender);
}

#[test]
fn cached_scan_reuse_does_not_depend_on_nacp_names() {
    let path = normalize_scan_path_text(r"E:\NSW\sample.nsp");
    let mut records = HashMap::new();
    records.insert(
        path.clone(),
        database::FileRecord {
            path: path.clone(),
            filename: "sample.nsp".into(),
            size: 100,
            mtime: Some(1.0),
            file_type: "nsp".into(),
            scan_folder: r"E:\NSW".into(),
            fast_hash: "cached-hash".into(),
            nacp_ko: String::new(),
            nacp_en: String::new(),
            nacp_ja: String::new(),
            contents: vec![scanner::Content {
                title_id: Some("0100D2C0115B6000".into()),
                version: 0,
                content_type: "base".into(),
                source: "test".into(),
            }],
        },
    );

    assert!(cached_record_for_path(&records, &path).is_some());
    records.get_mut(&path).unwrap().fast_hash.clear();
    assert!(cached_record_for_path(&records, &path).is_none());
}

/// Drives the real egui `update()` loop headlessly so the rendered screens,
/// filter paths, and tab transitions are exercised without a window.
fn run_app_ticks(app: &mut NslApp, ticks: usize) -> egui::Context {
    let ctx = egui::Context::default();
    install_korean_font(&ctx);
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1456.0, 900.0),
        )),
        ..Default::default()
    };
    for _ in 0..ticks {
        ctx.begin_pass(input.take());
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        };
    }
    ctx
}

fn seeded_app() -> (NslApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    std::fs::create_dir_all(base.join("icons")).unwrap();
    std::fs::create_dir_all(base.join("screenshots")).unwrap();
    let db = database::Database::open(base.join("library.db")).unwrap();
    db.init().unwrap();
    db.upsert_files(&[
        database::FileRecord {
            path: "E:\\NSW\\Alpha [0100D2C0115B6000][v0][Base].nsz".into(),
            filename: "Alpha [0100D2C0115B6000][v0][Base].nsz".into(),
            size: 4_000_000,
            mtime: Some(1.0),
            file_type: "nsz".into(),
            scan_folder: "E:\\NSW".into(),
            fast_hash: "hash-alpha".into(),
            nacp_ko: "알파".into(),
            nacp_en: "Alpha".into(),
            nacp_ja: String::new(),
            contents: vec![scanner::Content {
                title_id: Some("0100D2C0115B6000".into()),
                version: 0,
                content_type: "base".into(),
                source: "header_cnmt".into(),
            }],
        },
        database::FileRecord {
            path: "E:\\NSW\\Beta [0100D2C0115B7001][v0][DLC].nsz".into(),
            filename: "Beta [0100D2C0115B7001][v0][DLC].nsz".into(),
            size: 1_000_000,
            mtime: Some(2.0),
            file_type: "nsz".into(),
            scan_folder: "E:\\NSW".into(),
            fast_hash: "hash-beta".into(),
            nacp_ko: "베타 DLC".into(),
            nacp_en: String::new(),
            nacp_ja: String::new(),
            contents: vec![scanner::Content {
                title_id: Some("0100D2C0115B7001".into()),
                version: 0,
                content_type: "dlc".into(),
                source: "header_cnmt".into(),
            }],
        },
    ])
    .unwrap();
    let mut config = database::Config {
        scan_folders: vec!["E:\\NSW".into()],
        ..Default::default()
    };
    config
        .settings
        .insert("name_mode".into(), serde_json::Value::String("db".into()));
    config.settings.insert(
        "titledb_source".into(),
        serde_json::Value::String("custom".into()),
    );
    database::save_config(&base.join("config.json"), &config).unwrap();

    let app = NslApp::with_base_dir(base).unwrap();
    (app, dir)
}

fn wait_for_rows(app: &mut NslApp, ctx: &egui::Context) {
    for _ in 0..200 {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        });
        app.ui_frame(ctx);
        let _ = ctx.end_pass();
        if !app.rows.is_empty() && app.reload_result.is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("library rows did not load");
}

#[test]
fn library_store_and_screenshots_paint_text_before_lazy_image_uploads() {
    let (mut app, _dir) = seeded_app();
    app.torrent_engine = None;
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let base = app.rows.iter().find(|r| r.has_base).unwrap().clone();
    let icon = image::RgbaImage::from_pixel(320, 160, image::Rgba([60, 90, 120, 255]));
    icon.save(app.icon_dir.join(format!("{}.png", base.title_id)))
        .unwrap();
    let screenshot_dir = app.screenshot_dir.join(&base.title_id);
    std::fs::create_dir_all(&screenshot_dir).unwrap();
    icon.save(screenshot_dir.join("first.png")).unwrap();
    std::fs::create_dir_all(&app.catalog_cover_dir).unwrap();
    image::RgbImage::from_pixel(320, 160, image::Rgb([60, 90, 120]))
        .save_with_format(
            app.catalog_cover_dir.join("sample-cover.jpg"),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    app.catalog_state.items = vec![catalog::CatalogItem {
        title: "Text is visible before the store cover".into(),
        topic_id: "sample-cover".into(),
        ..Default::default()
    }];
    app.selected = Some(base.id);
    app.detail_tab = 0;
    let mut budget = 1;
    app.ensure_icon_texture(&ctx, &base.title_id, &mut budget);
    assert!(
        !app.icon_textures.contains_key(&base.title_id),
        "icon decoded on GUI thread"
    );
    let render = |app: &mut NslApp| {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    };
    app.folder_filter = "CATALOG".into();
    render(&mut app);
    assert!(
        ctx.data_mut(
            |data| data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
        )
        .is_some(),
        "store text row failed to render"
    );
    assert!(
        app.screenshot_textures.is_empty(),
        "background tab decoded screenshots"
    );
    for _ in 0..90 {
        app.poll_loaded_images(&ctx);
        if app.icon_textures.contains_key(&base.title_id)
            && app.catalog_cover_textures.contains_key("sample-cover")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(app.icon_textures.contains_key(&base.title_id));
    assert!(app.catalog_cover_textures.contains_key("sample-cover"));
    assert!(app.screenshot_textures.is_empty());
    app.folder_filter.clear();
    app.detail_tab = 2;
    for _ in 0..90 {
        render(&mut app);
        if !app.screenshot_textures.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        app.screenshot_textures.len(),
        1,
        "visible screenshot not decoded"
    );
    assert!(app.icon_textures.len() <= ICON_CACHE_MAX);
    assert!(app.catalog_cover_textures.len() <= STORE_COVER_CACHE_MAX);
}

#[test]
fn media_plan_collects_both_types_once_and_keeps_valid_duplicate_metadata() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut row = app.rows.iter().find(|r| r.has_base).unwrap().clone();
    row.title_id = "0100D2C0115B6000".into();
    row.icon_url.clear();
    row.screenshots = "[]".into();
    row.path = "E:\\sample.nsp".into();
    let mut duplicate = row.clone();
    duplicate.icon_url = "https://example.test/icon.png".into();
    duplicate.screenshots = "[\"https://example.test/image.jpg\"]".into();
    duplicate.path.clear();
    let rows = [row.clone(), duplicate.clone()];
    let full = media_collection_plan(&rows, None, MediaSelection::Both);
    assert_eq!(full.total(), 2);
    assert_eq!(full.icons.len(), 1);
    assert_eq!(full.screenshots.len(), 1);
    assert_eq!(full.icons[0].1, duplicate.icon_url);
    assert_eq!(full.icons[0].2, row.path);
    assert_eq!(full.screenshots[0].1, duplicate.screenshots);
    assert_eq!(
        media_collection_plan(&rows, None, MediaSelection::Icons).total(),
        1
    );
    assert_eq!(
        media_collection_plan(&rows, None, MediaSelection::Screenshots).total(),
        1
    );
    let one = media_collection_plan(&rows, Some(&row), MediaSelection::Both);
    assert_eq!(one.total(), 1);
    assert_eq!(one.screenshots.len(), 0);
}

#[test]
fn automatic_prefetch_combines_icons_and_screenshots_after_reload() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut row = app.rows.iter().find(|r| r.has_base).unwrap().clone();
    row.screenshots = "[\"https://example.test/one.jpg\"]".into();
    app.rows = vec![row.clone()];

    std::fs::create_dir_all(&app.icon_dir).unwrap();
    std::fs::create_dir_all(app.screenshot_dir.join(&row.title_id)).unwrap();
    let image_data = |format| {
        let image = image::DynamicImage::new_rgb8(2, 2);
        let mut buffer = std::io::Cursor::new(Vec::new());
        image.write_to(&mut buffer, format).unwrap();
        buffer.into_inner()
    };
    std::fs::write(
        app.icon_dir.join(format!("{}.png", row.title_id)),
        image_data(image::ImageFormat::Png),
    )
    .unwrap();
    std::fs::write(
        app.screenshot_dir.join(&row.title_id).join("001.jpg"),
        image_data(image::ImageFormat::Jpeg),
    )
    .unwrap();

    app.pending_icon_prefetch = true;
    app.pending_screenshot_prefetch = true;
    let (_tx, pending_reload) = mpsc::channel();
    app.reload_result = Some(pending_reload);
    app.start_pending_media();
    assert!(app.media_result.is_none(), "must wait for new DB rows");
    assert!(app.pending_icon_prefetch && app.pending_screenshot_prefetch);
    app.reload_result = None;
    app.start_pending_media();
    assert!(!app.pending_icon_prefetch && !app.pending_screenshot_prefetch);
    let outcome = app
        .media_result
        .as_ref()
        .expect("one combined worker")
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(outcome.icons, 0);
    assert_eq!(outcome.screenshots, 0);
    assert_eq!(outcome.failed, 0);
    assert_eq!(outcome.skipped, 2);
    assert_eq!(app.media_progress.as_ref().unwrap().snapshot(), (2, 2, 0));
    assert_eq!(app.media_progress.as_ref().unwrap().stage_label(), "완료");
}

#[test]
fn disconnected_background_worker_returns_error_instead_of_staying_busy() {
    let (tx, rx) = mpsc::channel::<std::result::Result<usize, String>>();
    let worker = Some(rx);
    assert!(poll_background(&worker, "테스트").is_none());
    drop(tx);
    assert!(poll_background(&worker, "테스트").unwrap().is_err());
}

#[test]
fn queued_reload_runs_after_inflight_reload_finishes() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let (tx, rx) = mpsc::channel::<std::result::Result<Vec<library::GameFile>, String>>();
    app.reload_result = Some(rx);
    app.start_reload();
    assert!(app.reload_dirty);
    tx.send(Ok(app.rows.clone())).unwrap();
    run_app_ticks(&mut app, 1);
    assert!(!app.reload_dirty);
    assert!(app.reload_result.is_some(), "new reload was dropped");
    wait_for_rows(&mut app, &ctx);
}

#[test]
fn cancelled_scan_never_schedules_media_collection() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.config.settings.insert("scan_icon".into(), "1".into());
    app.config
        .settings
        .insert("scan_screenshot".into(), "1".into());
    let (tx, rx) = mpsc::channel::<std::result::Result<ScanOutcome, String>>();
    app.scan_result = Some(rx);
    tx.send(Ok((0, 0, true, 0))).unwrap();
    run_app_ticks(&mut app, 1);
    assert!(!app.pending_icon_prefetch);
    assert!(!app.pending_screenshot_prefetch);
    assert!(app.media_result.is_none());
}

#[test]
fn schema_bump_forces_one_rescan() {
    // Records written before the version column cannot be upgraded by a
    // cache hit, so a schema mismatch must force a full re-analysis.
    let mut config = database::Config::default();
    assert_ne!(
        config
            .settings
            .get("scan_schema")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        SCAN_SCHEMA_VERSION,
        "a fresh config must re-analyze once"
    );
    config.settings.insert(
        "scan_schema".into(),
        serde_json::Value::String(SCAN_SCHEMA_VERSION.into()),
    );
    assert_eq!(
        config
            .settings
            .get("scan_schema")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        SCAN_SCHEMA_VERSION,
        "after the first scan the cache is trusted again"
    );
}

#[test]
fn incremental_scan_reanalyzes_unchanged_files_on_schema_change() {
    let (mut app, dir) = seeded_app();
    let folder = dir.path().join("synthetic-games");
    std::fs::create_dir_all(&folder).unwrap();
    let filename = "Test [0100D2C0115B6000][v123].nsp";
    let path = folder.join(filename);
    std::fs::write(&path, vec![0u8; 4096]).unwrap();
    let normalized = normalize_scan_path(&path);
    let metadata = std::fs::metadata(&path).unwrap();
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());
    app.db
        .upsert_files(&[database::FileRecord {
            path: normalized.clone(),
            filename: filename.into(),
            size: metadata.len() as i64,
            mtime,
            file_type: "nsp".into(),
            scan_folder: normalize_scan_path(&folder),
            fast_hash: scanner::fast_hash(&path).unwrap(),
            nacp_ko: String::new(),
            nacp_en: String::new(),
            nacp_ja: String::new(),
            contents: vec![scanner::Content {
                title_id: Some("0100D2C0115B6000".into()),
                version: 999,
                content_type: "base".into(),
                source: "old_schema".into(),
            }],
        }])
        .unwrap();
    app.folders = vec![normalize_scan_path(&folder)];
    app.config.settings.insert("scan_schema".into(), "0".into());
    app.headless_mode = true;
    app.start_scan(true);
    let ctx = egui::Context::default();
    assert!(run_until_idle(&mut app, &ctx));
    let contents = app.db.file_contents(&normalized).unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(
        contents[0].version, 123,
        "unchanged metadata must not bypass schema upgrade"
    );
}

#[test]
fn incremental_scan_detects_same_size_rom_rewrite_within_one_second() {
    // Replacing a ROM without changing its size, less than one second after
    // its previous mtime, used to bypass both the metadata scan and fast hash.
    // Everything here lives in the seeded app's private temporary folder.
    let (mut app, dir) = seeded_app();
    let folder = dir.path().join("rapid-rewrite-games");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("rapid-rewrite.nsp");
    let normalized = normalize_scan_path(&path);
    let named = normalize_scan_path(&folder);
    let fixture = |id: &str| {
        let entry_name = b"test.cnmt.xml\0";
        let xml = format!("<TitleId>{id}</TitleId>");
        let mut data = Vec::new();
        data.extend_from_slice(b"PFS0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(entry_name.len() as u32).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&(xml.len() as u64).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(entry_name);
        data.extend_from_slice(xml.as_bytes());
        data
    };
    let initial = fixture("0100D2C0115B6000");
    let replacement = fixture("0100D2C0115B9000");
    assert_eq!(initial.len(), replacement.len());
    std::fs::write(&path, &initial).unwrap();
    let initial_time = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(1_750_000_000)
        + std::time::Duration::from_millis(125);
    let later_time = initial_time + std::time::Duration::from_millis(250);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(initial_time))
        .unwrap();

    app.db.clear_library_data().unwrap();
    app.folders = vec![named.clone()];
    app.config.scan_folders = app.folders.clone();
    app.config.settings.insert(
        "scan_schema".into(),
        serde_json::Value::String(SCAN_SCHEMA_VERSION.into()),
    );
    app.headless_mode = true;
    let ctx = egui::Context::default();
    app.start_scan(false);
    assert!(run_until_idle(&mut app, &ctx));
    let before = app.db.file_contents(&normalized).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].title_id.as_deref(), Some("0100D2C0115B6000"));
    let before_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

    std::fs::write(&path, &replacement).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(later_time))
        .unwrap();
    let after_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let difference = after_mtime.duration_since(before_mtime).unwrap();
    assert!(difference > std::time::Duration::ZERO);
    assert!(difference < std::time::Duration::from_secs(1));
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        initial.len() as u64
    );

    app.start_scan(true);
    assert!(run_until_idle(&mut app, &ctx));
    let after = app.db.file_contents(&normalized).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].title_id.as_deref(),
        Some("0100D2C0115B9000"),
        "incremental scan must re-read ROM content after a subsecond mtime change"
    );
    assert_eq!(std::fs::read(&path).unwrap(), replacement);
}

#[test]
fn missing_dlc_exclusion_invalidates_stats_and_list_cache() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let id = "0100D2C0115B7001";
    let base = app.rows.iter_mut().find(|row| row.has_base).unwrap();
    base.missing_dlcs = vec![(id.into(), "DLC".into())];
    app.rows_generation += 1;
    app.folder_filter = "MISSING_DLCS".into();
    app.refresh_library_stats();
    assert_eq!(app.cached_missing_dlcs, 1);
    assert_eq!(app.library_rows_cached().len(), 1);
    app.config
        .settings
        .insert("exception_dlc".into(), id.into());
    app.refresh_library_stats();
    assert_eq!(app.cached_missing_dlcs, 0);
    assert_eq!(app.library_rows_cached().len(), 0);
}

#[test]
fn python_style_toolbar_buttons_remain_bordered_and_clickable() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let (stroke_width, hover_color, background) = ctx.data_mut(|data| {
        data.get_temp::<(f32, egui::Color32, egui::Color32)>(egui::Id::new(
            "nsl_test_toolbar_button_style",
        ))
        .unwrap()
    });
    assert!(stroke_width >= 1.0, "toolbar action border is invisible");
    assert_ne!(
        background,
        egui::Color32::TRANSPARENT,
        "toolbar action background is transparent"
    );
    assert_eq!(
        hover_color,
        ctx.style().visuals.hyperlink_color,
        "toolbar hover border no longer matches the theme accent"
    );

    let rect = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_view_button_rect"))
            .unwrap()
    });
    let at = rect.center();
    let before = app.view_mode.clone();
    for pressed in [true, false] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            events: vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    }
    assert_ne!(
        app.view_mode, before,
        "outlined view button no longer toggles list/grid"
    );
}

#[test]
fn store_sidebar_filters_real_genre_and_format_without_guessing_dlc() {
    let action_nsz = catalog::CatalogItem {
        title: "Alpha [NSZ] + Update + DLC".into(),
        title_id: Some("0100D2C0115B6000".into()),
        genre: "Action, Adventure".into(),
        image_format: ".NSZ (compressed)".into(),
        ..Default::default()
    };
    let puzzle_xci = catalog::CatalogItem {
        title: "Beta [XCI]".into(),
        title_id: Some("0100D2C0115B7000".into()),
        genre: "Puzzle".into(),
        image_format: "XCI".into(),
        ..Default::default()
    };
    assert!(catalog_matches_filters(&action_nsz, "", "all", "all"));
    assert!(catalog_matches_filters(
        &action_nsz,
        "alpha",
        "Action",
        "NSZ"
    ));
    assert!(catalog_matches_filters(
        &action_nsz,
        "0100d2c0115b6",
        "Adventure",
        "NSZ"
    ));
    assert!(!catalog_matches_filters(
        &action_nsz,
        "beta",
        "Action",
        "NSZ"
    ));
    assert!(!catalog_matches_filters(&action_nsz, "", "Puzzle", "NSZ"));
    assert!(!catalog_matches_filters(&action_nsz, "", "Action", "NSP"));
    assert!(catalog_matches_filters(
        &puzzle_xci,
        "beta",
        "Puzzle",
        "XCI"
    ));
    assert!(!catalog_matches_filters(&puzzle_xci, "", "Action", "XCI"));
}

#[test]
fn store_language_filter_does_not_assume_generic_multi_means_korean() {
    let typical = "Russian, English [RUS / ENG / Multi 10]";
    assert!(catalog_matches_language(typical, "all"));
    assert!(catalog_matches_language(typical, "ru"));
    assert!(catalog_matches_language(typical, "en"));
    assert!(!catalog_matches_language(typical, "ko"));
    assert!(!catalog_matches_language(typical, "ja"));
    assert!(catalog_matches_language(
        "Korean, English [KOR / ENG]",
        "ko"
    ));
    assert!(catalog_matches_language("Japanese [JAP]", "ja"));
    assert!(catalog_matches_language("Chinese [CHN]", "zh"));
    assert!(catalog_matches_language("Spanish [ESP]", "es"));
    assert!(catalog_matches_language("No text", "none"));
    assert!(!catalog_matches_language("English [ENG]", "none"));
    assert!(!catalog_matches_language("English [ENG]", "ko"));
    assert!(!catalog_matches_language("Korean", "unknown"));
    let item = catalog::CatalogItem {
        interface_lang: "English [ENG]".into(),
        ..Default::default()
    };
    let db = database::CatalogTitleDetails {
        languages: "Korean, English".into(),
        ..Default::default()
    };
    assert!(!catalog_item_matches_language(&item, None, "ko"));
    assert!(catalog_item_matches_language(&item, Some(&db), "ko"));
    assert!(!catalog_item_matches_language(&item, Some(&db), "ja"));
}

#[test]
fn store_title_follows_all_three_library_name_priority_modes() {
    let (app, _dir) = seeded_app();
    app.db
        .load_titles(&[titledb::Entry {
            title_id: "0100D2C0115B6000".into(),
            name: "DB preferred title".into(),
            description: "DB description".into(),
            ..Default::default()
        }])
        .unwrap();
    let item = catalog::CatalogItem {
        title: "Original Store [NSZ] + DLC".into(),
        title_id: Some("0100D2C0115B6000".into()),
        ..Default::default()
    };
    let ids = HashSet::from(["0100D2C0115B6000".to_owned()]);
    let metadata = app.db.catalog_title_details(&ids).unwrap();
    let details = metadata.get("0100D2C0115B6000");
    for (mode, expected) in [
        ("db", "DB preferred title"),
        ("ko_en_db", "알파"),
        ("ko_ja_db", "알파"),
    ] {
        let rows = app.db.files(mode).unwrap();
        let owned = catalog_owned_summaries(&rows);
        assert_eq!(
            catalog_preferred_title(&item, details, owned.get("0100D2C0115B6000")),
            expected,
            "Store and Library disagree under name_mode={mode}"
        );
    }
    assert_eq!(
        catalog_preferred_title(&item, details, None),
        "DB preferred title",
        "Unowned games should use title DB when available"
    );
    assert_eq!(catalog_preferred_title(&item, None, None), item.title);
    assert_eq!(
        catalog_base_id("0100D2C0115B6800").as_deref(),
        Some("0100D2C0115B6000")
    );
}

#[test]
fn store_sidebar_language_filter_is_combined_with_genre_and_format() {
    let (mut app, _dir) = seeded_app();
    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = vec![
        catalog::CatalogItem {
            title: "Alpha [NSZ]".into(),
            topic_id: "a".into(),
            interface_lang: "Korean, English [KOR / ENG]".into(),
            genre: "Action".into(),
            image_format: "NSZ".into(),
            ..Default::default()
        },
        catalog::CatalogItem {
            title: "Beta [NSZ]".into(),
            topic_id: "b".into(),
            interface_lang: "Russian, English [RUS / ENG / Multi 10]".into(),
            genre: "Action".into(),
            image_format: "NSZ".into(),
            ..Default::default()
        },
        catalog::CatalogItem {
            title: "Gamma [XCI]".into(),
            topic_id: "c".into(),
            interface_lang: "Japanese [JAP]".into(),
            genre: "Puzzle".into(),
            image_format: "XCI".into(),
            ..Default::default()
        },
    ];
    app.catalog_genre_filter = "Action".into();
    app.catalog_format_filter = "NSZ".into();
    app.catalog_language_filter = "ko".into();
    run_app_ticks(&mut app, 3);
    assert_eq!(app.catalog_matching_indices, vec![0]);
    app.catalog_language_filter = "ru".into();
    run_app_ticks(&mut app, 2);
    assert_eq!(app.catalog_matching_indices, vec![1]);
    app.catalog_genre_filter = "Puzzle".into();
    app.catalog_format_filter = "XCI".into();
    app.catalog_language_filter = "ja".into();
    run_app_ticks(&mut app, 2);
    assert_eq!(app.catalog_matching_indices, vec![2]);
    app.catalog_language_filter = "ko".into();
    run_app_ticks(&mut app, 2);
    assert!(app.catalog_matching_indices.is_empty());
}

#[test]
fn store_selected_detail_and_language_filter_fit_sidebar_at_three_widths() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = vec![catalog::CatalogItem {
        title: "Original long name with update and DLC [NSZ]".into(),
        topic_id: "ui-detail-one".into(),
        title_id: Some("0100D2C0115B6000".into()),
        year: "2026, November".into(),
        developer: "An independent developer".into(),
        publisher: "A publishing studio".into(),
        genre: "Action".into(),
        interface_lang: "Korean, English [KOR / ENG]".into(),
        image_format: "NSZ".into(),
        size: "3.21 GB".into(),
        url: "https://example.org/switch-game-post".into(),
        ..Default::default()
    }];
    app.catalog_title_details.insert(
        "0100D2C0115B6000".into(),
        database::CatalogTitleDetails {
            name: "Localized catalog name".into(),
            screenshots: "[\"https://example.org/one.jpg\"]".into(),
            release_date: "20261119".into(),
            languages: "Korean, English".into(),
            version: 131072,
            ..Default::default()
        },
    );
    app.catalog_title_metadata_loaded = true;
    app.catalog_selected = Some("ui-detail-one".into());
    app.catalog_language_filter = "ko".into();
    app.catalog_search_cache = None;
    for width in [720.0, 980.0, 1456.0] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 750.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        let (sidebar, language, title, result_count, reset, refresh, download, heading, post) = ctx
            .data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_sidebar_rect"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_language_filter"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_detail_title"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_result_count"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_reset_button"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_refresh_button"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new(
                        "nsl_test_store_game_download_button",
                    ))
                    .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_game_info_heading"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new(
                        "nsl_test_store_original_post_button",
                    ))
                    .unwrap(),
                )
            });
        assert!(
                language.left() >= sidebar.left()
                    && language.right() <= sidebar.right()
                    && title.left() >= sidebar.left()
                    && title.right() <= sidebar.right(),
                "Store sidebar details/control spill outside panel at {width}: panel={sidebar:?}, language={language:?}, title={title:?}"
            );
        assert!(
                result_count.bottom() < reset.top()
                    && reset.bottom() < refresh.top()
                    && refresh.bottom() < download.top()
                    && download.bottom() < heading.top()
                    && heading.bottom() < title.top()
                    && title.bottom() < post.top(),
                "Store order result -> reset -> refresh -> download -> game info -> external post failed at {width}: {result_count:?} {reset:?} {refresh:?} {download:?} {heading:?} {title:?} {post:?}"
            );
        for rect in [result_count, reset, refresh, download, heading, post] {
            assert!(
                    rect.left() >= sidebar.left() + 12.0 && rect.right() <= sidebar.right() - 12.0,
                    "Store content overflows padded sidebar at {width}: rect={rect:?}, sidebar={sidebar:?}"
                );
        }
        assert_eq!(app.catalog_matching_indices, vec![0]);
    }
}

/// Clicks the same coordinate using real egui pointer events (press and
/// release), so selection cannot pass merely from matching cell bounds.
fn click_cell(ctx: &egui::Context, app: &mut NslApp, pos: egui::Pos2, width: f32, height: f32) {
    // Separate successive independent tests by more than egui's
    // double-click interval so QA never accidentally launches a game.
    let click_time = ctx.input(|input| input.time) + 1.0;
    for (phase, pressed) in [true, false].into_iter().enumerate() {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            time: Some(click_time + 0.03 * phase as f64),
            events: vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        });
        app.ui_frame(ctx);
        let _ = ctx.end_pass();
    }
}

fn draw_card_test_frames(ctx: &egui::Context, app: &mut NslApp, count: usize) {
    draw_card_test_frames_at(ctx, app, count, 1456.0, 900.0);
}

fn draw_card_test_frames_at(
    ctx: &egui::Context,
    app: &mut NslApp,
    count: usize,
    width: f32,
    height: f32,
) {
    for _ in 0..count {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            ..Default::default()
        });
        app.ui_frame(ctx);
        let _ = ctx.end_pass();
    }
}

#[test]
fn store_card_background_selects_and_download_button_stays_independent() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = vec![catalog::CatalogItem {
        title: "Entire row target".into(),
        topic_id: "select-whole-card".into(),
        cover: String::new(),
        ..Default::default()
    }];
    app.catalog_search_cache = None;
    app.catalog_title_metadata_loaded = true;
    app.torrent_engine = None; // never launch real downloads in UI QA.
    draw_card_test_frames(&ctx, &mut app, 3);
    let row = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
            .expect("first store row not rendered")
    });
    let download = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_download_button"))
            .expect("store download button not rendered")
    });
    assert!(row.width() > 200.0 && download.left() > row.center().x);
    let empty_padding = row.min + egui::vec2(3.0, 3.0);
    click_cell(&ctx, &mut app, empty_padding, 1456.0, 900.0);
    assert_eq!(app.catalog_selected.as_deref(), Some("select-whole-card"));
    assert!(app.torrent_progress.is_empty());

    // An explicit action button cannot be swallowed by row selection.
    app.catalog_selected = None;
    app.status.clear();
    click_cell(&ctx, &mut app, download.center(), 1456.0, 900.0);
    assert_eq!(
        app.status, "토렌트 엔진을 사용할 수 없습니다",
        "download button click was intercepted by the card"
    );
    assert!(
        app.catalog_selected.is_none(),
        "clicking the download action should not change row selection"
    );
}

#[test]
fn download_row_background_selects_without_torrent_actions() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None;
    app.torrent_progress = vec![
        (
            "download-a".into(),
            torrent::TorrentProgress {
                name: "First game".into(),
                state: "일시정지".into(),
                paused: true,
                ..Default::default()
            },
        ),
        (
            "download-b".into(),
            torrent::TorrentProgress {
                name: "Second game".into(),
                state: "완료".into(),
                finished: true,
                ..Default::default()
            },
        ),
    ];
    draw_card_test_frames(&ctx, &mut app, 3);
    let row = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_first_row"))
            .expect("first download row missing")
    });
    click_cell(
        &ctx,
        &mut app,
        row.min + egui::vec2(3.0, 3.0),
        1456.0,
        900.0,
    );
    assert_eq!(app.download_selected.as_deref(), Some("download-a"));
    assert_eq!(app.torrent_progress.len(), 2);
    assert!(!app.torrent_progress[0].1.finished);
    assert!(app.torrent_progress[0].1.paused);
}

#[test]
fn grid_card_padding_selects_entire_cell_without_clicking_text_or_icon() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.view_mode = "grid".into();
    app.selected = None;
    draw_card_test_frames(&ctx, &mut app, 3);
    let card = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_first_card"))
            .expect("first grid card missing")
    });
    let hit = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_first_card_hit_rect"))
            .expect("first grid hit region missing")
    });
    assert!(
        hit.contains(card.min + egui::vec2(3.0, 3.0)),
        "grid background region misses cell padding: hit={hit:?}, card={card:?}"
    );
    assert!(card.width() >= 120.0 && card.height() >= 120.0);
    click_cell(
        &ctx,
        &mut app,
        card.min + egui::vec2(3.0, 3.0),
        1456.0,
        900.0,
    );
    assert!(app.selected.is_some(), "grid card padding did not select");
    draw_card_test_frames(&ctx, &mut app, 1);
    let selected = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_selected_grid_rect"))
            .expect("grid selection highlight was not painted")
    });
    assert!(
        selected.contains(card.min + egui::vec2(3.0, 3.0))
            && (selected.min - card.min).length() < 3.0
            && (selected.size() - card.size()).length() < 20.0,
        "grid selection background does not cover the clicked card: {selected:?} versus {card:?}"
    );
}

#[test]
fn grid_card_clicks_on_artwork_title_edges_at_three_window_widths() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.view_mode = "grid".into();
    for (width, height) in [(1456.0, 900.0), (980.0, 760.0), (720.0, 690.0)] {
        for region in ["artwork", "title", "right margin", "bottom margin"] {
            // A real deselection closes the library detail sidebar and
            // changes the grid width. Render that change BEFORE deriving
            // the next click point, as users see the resized card.
            app.selected = None;
            draw_card_test_frames_at(&ctx, &mut app, 3, width, height);
            let rect = ctx.data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_first_card"))
                    .expect("grid first card missing")
            });
            assert!(rect.width() > 120.0 && rect.bottom() < height);
            let point = match region {
                "artwork" => rect.left_top() + egui::vec2(rect.width() / 2.0, 35.0),
                "title" => rect.left_bottom() + egui::vec2(rect.width() / 2.0, -25.0),
                "right margin" => rect.right_center() - egui::vec2(4.0, 0.0),
                _ => rect.right_bottom() - egui::vec2(6.0, 4.0),
            };
            click_cell(&ctx, &mut app, point, width, height);
            assert!(
                app.selected.is_some(),
                "grid {region} did not select at {width}x{height}: card={rect:?}, point={point:?}"
            );
        }
    }
}

#[test]
fn store_card_cover_name_metadata_and_padding_select_across_window_widths() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = vec![catalog::CatalogItem {
        title: "Whole store card".into(),
        topic_id: "whole-store-row".into(),
        title_id: Some("0100D2C0115B6000".into()),
        image_format: "NSZ".into(),
        size: "16 MB".into(),
        cover: String::new(),
        ..Default::default()
    }];
    app.catalog_title_metadata_loaded = true;
    app.catalog_search_cache = None;
    app.torrent_engine = None;
    for (width, height) in [(1456.0, 900.0), (980.0, 760.0), (720.0, 690.0)] {
        for region in ["cover", "name", "metadata", "bottom padding"] {
            app.catalog_selected = None;
            draw_card_test_frames_at(&ctx, &mut app, 3, width, height);
            let rect = ctx.data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
                    .expect("store first row missing")
            });
            let point = match region {
                "cover" => rect.left_top() + egui::vec2(30.0, 30.0),
                "name" => rect.left_top() + egui::vec2(98.0, 16.0),
                "metadata" => rect.left_top() + egui::vec2(98.0, 37.0),
                _ => rect.left_bottom() + egui::vec2(4.0, -4.0),
            };
            assert!(rect.contains(point), "invalid store target {point:?}");
            click_cell(&ctx, &mut app, point, width, height);
            assert_eq!(
                app.catalog_selected.as_deref(),
                Some("whole-store-row"),
                "store {region} did not select at {width}: {point:?} in {rect:?}"
            );
        }
    }
}

#[test]
fn download_card_name_progress_state_and_padding_select_across_widths() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None; // No file or network activity in this QA.
    app.torrent_progress = vec![(
        "download-full-cell".into(),
        torrent::TorrentProgress {
            name: "Example download title".into(),
            state: "다운로드 중".into(),
            total_bytes: 1_000_000,
            progress_bytes: 250_000,
            download_speed_bps: 4_000,
            ..Default::default()
        },
    )];
    for (width, height) in [(1456.0, 900.0), (980.0, 760.0), (720.0, 690.0)] {
        for region in ["name", "progress", "status", "bottom padding"] {
            app.download_selected = None;
            draw_card_test_frames_at(&ctx, &mut app, 3, width, height);
            let rect = ctx.data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_first_row"))
                    .expect("download first row missing")
            });
            let point = match region {
                "name" => rect.left_top() + egui::vec2(40.0, 23.0),
                "progress" => rect.center(),
                "status" => rect.left_bottom() + egui::vec2(38.0, -23.0),
                _ => rect.left_bottom() + egui::vec2(3.0, -3.0),
            };
            assert!(rect.contains(point), "invalid download point {point:?}");
            click_cell(&ctx, &mut app, point, width, height);
            assert_eq!(
                app.download_selected.as_deref(),
                Some("download-full-cell"),
                "download {region} did not select at {width}: {point:?} in {rect:?}"
            );
        }
    }
}

#[test]
fn download_name_starts_at_card_left_and_peer_counts_do_not_overlap_actions() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None;
    for label in ["Short", &"Long download title ".repeat(35)] {
        let progress = torrent::TorrentProgress {
            name: label.into(),
            state: "다운로드".into(),
            active_seeders: Some(3),
            active_peers: Some(5),
            total_bytes: 20_000,
            progress_bytes: 2_000,
            ..Default::default()
        };
        assert_eq!(download_connection_label(&progress), "시드 3 · 피어 5");
        app.torrent_progress = vec![("stage20-geometry".into(), progress)];
        for (width, height) in [(720.0, 720.0), (980.0, 760.0), (1456.0, 900.0)] {
            draw_card_test_frames_at(&ctx, &mut app, 3, width, height);
            let (row, name, delete, connections) = ctx.data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_first_row"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new((
                        "nsl_test_download_name",
                        "stage20-geometry",
                    )))
                    .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new((
                        "nsl_test_download_delete_button",
                        "stage20-geometry",
                    )))
                    .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new((
                        "nsl_test_download_connections",
                        "stage20-geometry",
                    )))
                    .unwrap(),
                )
            });
            assert!(
                name.left() >= row.left() && name.left() - row.left() < 34.0,
                "name centered instead of left aligned: {label} {width}: {row:?} {name:?}"
            );
            assert!(
                name.right() <= row.right() && connections.right() <= row.right(),
                "card contents overflow: {width}: {row:?} {name:?} {connections:?}"
            );
            assert!(
                !name.intersects(delete),
                "name overlaps delete button: {width}: {name:?} {delete:?}"
            );
        }
    }

    let mut progress = torrent::TorrentProgress {
        active_seeders: Some(0),
        active_peers: Some(0),
        ..Default::default()
    };
    assert_eq!(download_connection_label(&progress), "시드 0 · 피어 0");
    progress.active_peers = None;
    assert_eq!(download_connection_label(&progress), "시드 — · 피어 —");
    progress.active_seeders = None;
    assert_eq!(download_connection_label(&progress), "시드 — · 피어 —");
}

#[test]
fn download_delete_button_opens_dialog_for_all_states_and_respects_file_choice() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None; // A real download can never run during this UI QA.
    for (state, paused, finished, error) in [
        ("다운로드", false, false, None),
        ("일시정지", true, false, None),
        ("완료", false, true, None),
        ("오류", false, false, Some("테스트 오류".to_string())),
    ] {
        app.torrent_progress = vec![(
            "stage20-delete".into(),
            torrent::TorrentProgress {
                name: format!("{state} 테스트"),
                state: state.into(),
                paused,
                finished,
                error,
                total_bytes: 100,
                progress_bytes: if finished { 100 } else { 10 },
                ..Default::default()
            },
        )];
        for (setting, first_option) in [("0", false), ("1", true)] {
            app.config.settings.insert(
                "download_delete_files".into(),
                serde_json::Value::String(setting.into()),
            );
            app.download_delete_confirm = None;
            draw_card_test_frames(&ctx, &mut app, 3);
            let delete = ctx.data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new((
                    "nsl_test_download_delete_button",
                    "stage20-delete",
                )))
                .expect("all download states must show the same 삭제 button")
            });
            click_cell(&ctx, &mut app, delete.center(), 1456.0, 900.0);
            assert_eq!(
                app.download_delete_confirm
                    .as_ref()
                    .map(|(key, _)| key.as_str()),
                Some("stage20-delete"),
                "{state} status must confirm removal"
            );
            draw_card_test_frames(&ctx, &mut app, 2);
            let (keep, files, cancel) = ctx.data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new((
                        "nsl_test_download_delete_option",
                        false,
                    )))
                    .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new((
                        "nsl_test_download_delete_option",
                        true,
                    )))
                    .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_delete_cancel"))
                        .unwrap(),
                )
            });
            // popup_footer is right-to-left: the first/priority option
            // appears at the right edge; cancel stays at the left edge.
            assert_eq!(files.left() < keep.left(), !first_option);
            assert!(cancel.right() < files.right() && cancel.right() < keep.right());
            click_cell(&ctx, &mut app, cancel.center(), 1456.0, 900.0);
            assert!(app.download_delete_confirm.is_none());
            assert_eq!(
                app.torrent_progress.len(),
                1,
                "cancel must not remove entry"
            );

            // A confirmed choice is different from dismissing the popup,
            // and must never silently run without a torrent engine.
            draw_card_test_frames(&ctx, &mut app, 2);
            let delete = ctx.data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new((
                    "nsl_test_download_delete_button",
                    "stage20-delete",
                )))
                .unwrap()
            });
            click_cell(&ctx, &mut app, delete.center(), 1456.0, 900.0);
            draw_card_test_frames(&ctx, &mut app, 2);
            let chosen = if first_option { files } else { keep };
            click_cell(&ctx, &mut app, chosen.center(), 1456.0, 900.0);
            assert!(app.download_delete_confirm.is_none());
            assert_eq!(app.status, "토렌트 엔진을 사용할 수 없습니다");
            assert_eq!(app.torrent_progress.len(), 1);
        }
    }
}

#[test]
fn loaded_grid_icon_and_store_cover_are_clickable_whole_card_targets() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let texture = ctx.load_texture(
        "stage15-synthetic-artwork",
        egui::ColorImage::new([24, 24], egui::Color32::from_rgb(33, 88, 153)),
        egui::TextureOptions::LINEAR,
    );
    for row in &app.rows {
        app.icon_textures
            .insert(row.title_id.clone(), texture.clone());
    }
    app.view_mode = "grid".into();
    app.selected = None;
    draw_card_test_frames(&ctx, &mut app, 3);
    let grid = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_first_card"))
            .expect("grid texture card missing")
    });
    click_cell(
        &ctx,
        &mut app,
        grid.left_top() + egui::vec2(grid.width() / 2.0, 35.0),
        1456.0,
        900.0,
    );
    assert!(
        app.selected.is_some(),
        "loaded grid texture blocked selection"
    );

    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = vec![catalog::CatalogItem {
        title: "Store cover test".into(),
        topic_id: "stage15-cover".into(),
        ..Default::default()
    }];
    app.catalog_cover_textures
        .insert("stage15-cover".into(), texture);
    app.catalog_title_metadata_loaded = true;
    app.catalog_search_cache = None;
    app.catalog_selected = None;
    app.torrent_engine = None;
    draw_card_test_frames(&ctx, &mut app, 3);
    let store = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
            .expect("store texture card missing")
    });
    click_cell(
        &ctx,
        &mut app,
        store.left_top() + egui::vec2(32.0, 32.0),
        1456.0,
        900.0,
    );
    assert_eq!(
        app.catalog_selected.as_deref(),
        Some("stage15-cover"),
        "loaded store cover blocked selection"
    );
}

#[test]
fn selection_moves_between_distinct_grid_store_and_download_items() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);

    app.view_mode = "grid".into();
    // Include both the base title and the DLC test fixture.
    app.kind_filter.clear();
    app.selected = None;
    draw_card_test_frames(&ctx, &mut app, 3);
    let first_grid = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_first_card"))
            .expect("first grid card")
    });
    click_cell(
        &ctx,
        &mut app,
        first_grid.left_top() + egui::vec2(4.0, 4.0),
        1456.0,
        900.0,
    );
    let first_id = app.selected.expect("grid first selection");
    draw_card_test_frames(&ctx, &mut app, 3);
    let second_grid = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_second_card"))
            .expect("second grid card")
    });
    click_cell(
        &ctx,
        &mut app,
        second_grid.left_top() + egui::vec2(4.0, 4.0),
        1456.0,
        900.0,
    );
    assert_ne!(app.selected, Some(first_id), "grid cannot switch cards");

    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = ["topic-a", "topic-b"]
        .iter()
        .map(|topic| catalog::CatalogItem {
            title: topic.to_string(),
            topic_id: topic.to_string(),
            ..Default::default()
        })
        .collect();
    app.catalog_title_metadata_loaded = true;
    app.catalog_search_cache = None;
    app.catalog_selected = None;
    app.torrent_engine = None;
    draw_card_test_frames(&ctx, &mut app, 3);
    let store_first = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
            .expect("first store row")
    });
    click_cell(
        &ctx,
        &mut app,
        store_first.left_top() + egui::vec2(3.0, 3.0),
        1456.0,
        900.0,
    );
    assert_eq!(app.catalog_selected.as_deref(), Some("topic-a"));
    draw_card_test_frames(&ctx, &mut app, 3);
    let store_last = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_last_row"))
            .expect("second store row")
    });
    click_cell(
        &ctx,
        &mut app,
        store_last.left_top() + egui::vec2(3.0, 3.0),
        1456.0,
        900.0,
    );
    assert_eq!(app.catalog_selected.as_deref(), Some("topic-b"));

    app.folder_filter = "DOWNLOADS".into();
    app.download_selected = None;
    app.torrent_progress = ["job-a", "job-b"]
        .iter()
        .map(|key| {
            (
                key.to_string(),
                torrent::TorrentProgress {
                    name: key.to_string(),
                    paused: true,
                    ..Default::default()
                },
            )
        })
        .collect();
    draw_card_test_frames(&ctx, &mut app, 3);
    let download_first = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_first_row"))
            .expect("first download row")
    });
    click_cell(
        &ctx,
        &mut app,
        download_first.left_top() + egui::vec2(4.0, 4.0),
        1456.0,
        900.0,
    );
    assert_eq!(app.download_selected.as_deref(), Some("job-a"));
    draw_card_test_frames(&ctx, &mut app, 3);
    let download_last = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_last_row"))
            .expect("second download row")
    });
    click_cell(
        &ctx,
        &mut app,
        download_last.left_top() + egui::vec2(4.0, 4.0),
        1456.0,
        900.0,
    );
    assert_eq!(app.download_selected.as_deref(), Some("job-b"));
    assert_eq!(app.torrent_progress.len(), 2);
}

#[test]
fn store_download_popup_selects_nested_directories_and_can_cancel() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.torrent_engine = None;
    let preview = torrent::TorrentPreview {
        files: vec![
            torrent::TorrentFileEntry {
                index: 0,
                path: PathBuf::from("game/base/a.nsp"),
                size: 100,
            },
            torrent::TorrentFileEntry {
                index: 1,
                path: PathBuf::from("game/dlc/extra.nsp"),
                size: 55,
            },
            torrent::TorrentFileEntry {
                index: 2,
                path: PathBuf::from("readme.txt"),
                size: 5,
            },
        ],
        torrent_bytes: vec![1, 2, 3],
    };
    app.catalog_torrent_dialog = Some(CatalogTorrentDialog {
        item: catalog::CatalogItem {
            title: "Nested torrent QA".into(),
            topic_id: "nested-folder-test".into(),
            magnet: "magnet:?xt=urn:btih:1111111111111111111111111111111111111111".into(),
            ..Default::default()
        },
        request: None,
        files: Some(torrent_file_picker::TorrentFilePicker::new(preview).unwrap()),
        error: None,
    });
    draw_card_test_frames(&ctx, &mut app, 3);
    let popup = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_torrent_file_selection_popup"))
            .expect("file-picker popup not visible")
    });
    assert!(popup.width() >= 480.0 && popup.height() > 145.0);
    assert_eq!(
        app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .selected_count(),
        3
    );
    let root_checkbox = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("nsl_test_torrent_node_checkbox", "game")))
            .expect("root folder checkbox missing")
    });
    let nested_checkbox = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new((
            "nsl_test_torrent_node_checkbox",
            "game/base",
        )))
        .expect("nested folder checkbox missing")
    });
    assert!(
            nested_checkbox.center().x - root_checkbox.center().x >= 27.0,
            "nested folder needs a full explorer-style indent: root={root_checkbox:?} child={nested_checkbox:?}"
        );
    let plus = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new((
            "nsl_test_torrent_node_expander",
            "game/base",
        )))
        .expect("nested folder + control missing")
    });
    assert!(plus.width() >= 17.0 && plus.height() >= 17.0);
    click_cell(&ctx, &mut app, plus.center(), 1456.0, 900.0);
    assert!(
        app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .is_expanded("game/base"),
        "clicking + must expand the nested directory"
    );
    draw_card_test_frames(&ctx, &mut app, 2);
    let grandchild_checkbox = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new((
            "nsl_test_torrent_node_checkbox",
            "game/base/a.nsp",
        )))
        .expect("expanded grandchild file missing")
    });
    assert!(
            grandchild_checkbox.center().x - nested_checkbox.center().x >= 27.0,
            "file nested two levels must be further indented: root={root_checkbox:?}, nested={nested_checkbox:?}, grandchild={grandchild_checkbox:?}, expander={plus:?}"
        );
    let minus = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new((
            "nsl_test_torrent_node_expander",
            "game/base",
        )))
        .expect("nested folder minus control missing")
    });
    click_cell(&ctx, &mut app, minus.center(), 1456.0, 900.0);
    assert!(
        !app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .is_expanded("game/base"),
        "clicking minus must collapse the nested directory"
    );
    // Folder and file names must stay clickable after being moved left.
    let folder_name = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("nsl_test_torrent_node_name", "game/base")))
            .expect("folder name hitbox missing")
    });
    click_cell(&ctx, &mut app, folder_name.center(), 1456.0, 900.0);
    assert!(
        app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .is_expanded("game/base"),
        "clicking the left-aligned folder name must expand the folder"
    );
    let file_name = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new((
            "nsl_test_torrent_node_name",
            "game/base/a.nsp",
        )))
        .expect("nested filename hitbox missing")
    });
    click_cell(&ctx, &mut app, file_name.center(), 1456.0, 900.0);
    assert_eq!(
        app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .selected_ids(),
        vec![1, 2],
        "clicking a left-aligned filename must toggle only its file"
    );
    let clear = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_torrent_clear_all"))
            .unwrap()
    });
    click_cell(&ctx, &mut app, clear.center(), 1456.0, 900.0);
    assert_eq!(
        app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .selected_count(),
        0
    );
    let folder = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("nsl_test_torrent_node_checkbox", "game")))
            .expect("game directory checkbox missing")
    });
    click_cell(&ctx, &mut app, folder.center(), 1456.0, 900.0);
    assert_eq!(
        app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .selected_ids(),
        vec![0, 1],
        "checking parent folder must recursively include DLC subdirectory"
    );
    let all = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_torrent_select_all"))
            .unwrap()
    });
    click_cell(&ctx, &mut app, all.center(), 1456.0, 900.0);
    assert_eq!(
        app.catalog_torrent_dialog
            .as_ref()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .selected_count(),
        3
    );
    let cancel = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_torrent_cancel_button"))
            .unwrap()
    });
    click_cell(&ctx, &mut app, cancel.center(), 1456.0, 900.0);
    assert!(app.catalog_torrent_dialog.is_none());
    assert!(app.torrent_progress.is_empty());
}

#[test]
fn torrent_preview_status_bar_only_shows_loading_while_request_is_pending() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.torrent_engine = None;
    app.catalog_title_metadata_loaded = true;
    app.status = "이전 작업 완료".into();
    let (request, response, canceled_flag) = torrent::synthetic_preview_request();
    app.catalog_torrent_dialog = Some(CatalogTorrentDialog {
        item: catalog::CatalogItem {
            title: "Status bar regression".into(),
            topic_id: "status-qa".into(),
            ..Default::default()
        },
        request: Some(request),
        files: None,
        error: None,
    });

    assert!(app
        .active_progress_views()
        .iter()
        .any(|view| view.label == "토렌트 파일 목록 확인" && view.fraction.is_none()));
    draw_card_test_frames(&ctx, &mut app, 1);
    assert_eq!(
        app.status, "이전 작업 완료",
        "loading must not stick in self.status"
    );
    let pending = ctx.data_mut(|data| {
        data.get_temp::<String>(egui::Id::new("nsl_test_status_message"))
            .expect("status bar was not drawn")
    });
    assert!(pending.contains("토렌트 파일 목록 확인"), "{pending}");

    response
        .send(Ok(torrent::TorrentPreview {
            files: vec![torrent::TorrentFileEntry {
                index: 0,
                path: PathBuf::from("content/base.nsp"),
                size: 4096,
            }],
            torrent_bytes: vec![1, 2, 3],
        }))
        .unwrap();
    draw_card_test_frames(&ctx, &mut app, 2);
    let dialog = app.catalog_torrent_dialog.as_ref().unwrap();
    assert!(dialog.request.is_none());
    assert_eq!(dialog.files.as_ref().unwrap().total_files(), 1);
    assert_eq!(app.status, "토렌트 파일 목록 준비 완료 · 1개");
    assert!(!app
        .active_progress_views()
        .iter()
        .any(|view| view.label == "토렌트 파일 목록 확인"));
    assert!(canceled_flag.load(Ordering::Relaxed));
    assert_eq!(
        ctx.data_mut(|data| data.get_temp::<String>(egui::Id::new("nsl_test_status_message"))),
        Some(app.status.clone())
    );

    let cancel = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_torrent_cancel_button"))
            .unwrap()
    });
    click_cell(&ctx, &mut app, cancel.center(), 1456.0, 900.0);
    draw_card_test_frames(&ctx, &mut app, 3);
    assert!(app.catalog_torrent_dialog.is_none());
    assert_eq!(app.status, "토렌트 파일 선택 취소됨");
    assert_eq!(
        ctx.data_mut(|data| data.get_temp::<String>(egui::Id::new("nsl_test_status_message"))),
        Some(app.status.clone())
    );
    assert!(app.torrent_progress.is_empty());
}

#[test]
fn torrent_preview_failure_disconnect_and_escape_clear_busy_status() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.torrent_engine = None;
    app.catalog_title_metadata_loaded = true;
    let item = catalog::CatalogItem {
        title: "Failure and cancel status QA".into(),
        topic_id: "failure-status".into(),
        ..Default::default()
    };

    for disconnected in [false, true] {
        app.status = "준비".into();
        let (request, sender, canceled_flag) = torrent::synthetic_preview_request();
        app.catalog_torrent_dialog = Some(CatalogTorrentDialog {
            item: item.clone(),
            request: Some(request),
            files: None,
            error: None,
        });
        if disconnected {
            drop(sender);
        } else {
            sender
                .send(Err("테스트 메타데이터 조회 실패".into()))
                .unwrap();
        }
        draw_card_test_frames(&ctx, &mut app, 2);
        let dialog = app.catalog_torrent_dialog.as_ref().unwrap();
        assert!(dialog.request.is_none());
        assert!(dialog.error.is_some());
        assert!(app.status.contains("토렌트 파일 목록 조회 실패"));
        assert!(!app.status.contains("확인 중"));
        assert_eq!(
            ctx.data_mut(|data| data.get_temp::<String>(egui::Id::new("nsl_test_status_message"))),
            Some(app.status.clone())
        );
        assert!(!app
            .active_progress_views()
            .iter()
            .any(|view| view.label == "토렌트 파일 목록 확인"));
        assert!(canceled_flag.load(Ordering::Relaxed));
        send_escape_to_app(&mut app, &ctx);
        assert!(app.catalog_torrent_dialog.is_none());
        assert_eq!(app.status, "토렌트 파일 선택 취소됨");
    }

    let (request, _sender, canceled_flag) = torrent::synthetic_preview_request();
    app.catalog_torrent_dialog = Some(CatalogTorrentDialog {
        item,
        request: Some(request),
        files: None,
        error: None,
    });
    draw_card_test_frames(&ctx, &mut app, 1);
    assert!(app
        .active_progress_views()
        .iter()
        .any(|view| view.label == "토렌트 파일 목록 확인"));
    send_escape_to_app(&mut app, &ctx);
    assert!(app.catalog_torrent_dialog.is_none());
    assert!(
        canceled_flag.load(Ordering::Relaxed),
        "Esc must cancel the actual metadata request"
    );
    assert_eq!(app.status, "토렌트 파일 선택 취소됨");
    assert!(!app
        .active_progress_views()
        .iter()
        .any(|view| view.label == "토렌트 파일 목록 확인"));
    assert!(app.torrent_progress.is_empty());
}

#[test]
fn store_original_post_button_opens_selected_web_url() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = vec![catalog::CatalogItem {
        title: "Link button test".into(),
        topic_id: "post-button-qa".into(),
        url: "https://example.org/original-topic".into(),
        ..Default::default()
    }];
    app.catalog_selected = Some("post-button-qa".into());
    // Disable asynchronous metadata edits while sending pointer events.
    // Otherwise a background DB lookup may move the button mid-click.
    app.catalog_title_metadata_loaded = true;
    app.catalog_title_load_result = None;
    app.catalog_search_cache = None;
    for _ in 0..3 {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    }
    let button = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_store_original_post_button"))
            .expect("original post must be rendered as a button")
    });
    let click_at = button.center();
    assert!(button.width() > 150.0 && click_at.y < 900.0);
    for pressed in [true, false] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            events: vec![
                egui::Event::PointerMoved(click_at),
                egui::Event::PointerButton {
                    pos: click_at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let output = ctx.end_pass();
        if !pressed {
            let url = output
                .platform_output
                .open_url
                .expect("clicking the original post button must open the URL");
            assert_eq!(url.url, "https://example.org/original-topic");
            assert!(url.new_tab);
        }
    }
    assert!(app.torrent_progress.is_empty());
}

#[test]
#[ignore = "reads the existing real catalog and SQLite title DB without modifying user data"]
fn real_catalog_title_db_metadata_matches_read_only_and_is_bounded() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let catalog = catalog::load(root).expect("existing local catalog");
    let ids = catalog
        .items
        .iter()
        .filter_map(catalog::CatalogItem::base_title_id)
        .collect::<HashSet<_>>();
    // A real-data QA test must not open the user's SQLite with write flags.
    let sandbox = tempfile::tempdir().unwrap();
    let snapshot = sandbox.path().join("library.db");
    database::backup_read_only(&root.join("library.db"), &snapshot).unwrap();
    let db = database::Database::open(snapshot).unwrap();
    let start = std::time::Instant::now();
    let details = db.catalog_title_details(&ids).unwrap();
    let elapsed = start.elapsed();
    assert!(ids.len() > 5_000, "real catalog was unexpectedly small");
    assert!(details.len() > 1_000, "real title DB matching failed");
    assert!(details.len() <= ids.len());
    eprintln!(
        "CATALOG_DB_METADATA titles={} base_ids={} matched={} elapsed={:.3}s",
        catalog.items.len(),
        ids.len(),
        details.len(),
        elapsed.as_secs_f64()
    );
}

#[test]
fn download_sidebar_filters_search_and_mutually_exclusive_states() {
    let mut progress = torrent::TorrentProgress {
        name: "Alpha Update DLC".into(),
        state: "다운로드 중".into(),
        ..Default::default()
    };
    assert!(download_matches_filters("job-a", &progress, "", "all"));
    assert!(download_matches_filters(
        "job-a", &progress, "update", "active"
    ));
    assert!(download_matches_filters(
        "job-a", &progress, "job-a", "active"
    ));
    assert!(!download_matches_filters("job-a", &progress, "beta", "all"));
    assert!(!download_matches_filters("job-a", &progress, "", "paused"));
    progress.paused = true;
    assert!(download_matches_filters("job-a", &progress, "", "paused"));
    assert!(!download_matches_filters("job-a", &progress, "", "active"));
    progress.error = Some("network error".into());
    assert!(download_matches_filters("job-a", &progress, "", "failed"));
    assert!(!download_matches_filters("job-a", &progress, "", "paused"));
    progress.paused = false;
    progress.error = None;
    progress.finished = true;
    assert!(download_matches_filters("job-a", &progress, "", "finished"));
    assert!(!download_matches_filters("job-a", &progress, "", "active"));
}

#[test]
fn download_paging_clicks_and_clamps_to_filtered_job_count() {
    let (mut app, _dir) = seeded_app();
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None;
    app.torrent_progress = (0..237)
        .map(|i| {
            let finished = i < 12;
            (
                format!("test-{i:03}"),
                torrent::TorrentProgress {
                    name: format!("Download task {i:03}"),
                    finished,
                    state: if finished {
                        "완료"
                    } else {
                        "다운로드 중"
                    }
                    .into(),
                    ..Default::default()
                },
            )
        })
        .collect();
    let ctx = run_app_ticks(&mut app, 3);
    assert_eq!(app.download_page, 0);
    let next = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_pagination_next"))
            .expect("download next-page button missing")
            .center()
    });
    let render = |app: &mut NslApp, events: Vec<egui::Event>| {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    };
    for pressed in [true, false] {
        render(
            &mut app,
            vec![
                egui::Event::PointerMoved(next),
                egui::Event::PointerButton {
                    pos: next,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(app.download_page, 1, "next should advance from page 1 to 2");
    app.download_page = 2;
    render(&mut app, vec![]);
    assert_eq!(
        ctx.data_mut(|data| {
            data.get_temp::<usize>(egui::Id::new("nsl_test_download_visible_items"))
        }),
        Some(37),
        "last page must contain only the remaining 37 tasks"
    );
    app.download_status_filter = "finished".into();
    render(&mut app, vec![]);
    assert_eq!(
        app.download_page, 0,
        "short filter must clamp to first page"
    );
    assert_eq!(
        ctx.data_mut(|data| {
            data.get_temp::<usize>(egui::Id::new("nsl_test_download_visible_items"))
        }),
        Some(12),
    );
    app.download_search = "not-found".into();
    render(&mut app, vec![]);
    assert_eq!(app.download_page, 0);
    assert_eq!(
        ctx.data_mut(|data| {
            data.get_temp::<usize>(egui::Id::new("nsl_test_download_visible_items"))
        }),
        Some(0),
    );
    assert_eq!(app.torrent_progress.len(), 237);
}

#[test]
fn store_left_paging_click_and_filter_reset_keep_all_catalog_rows() {
    let (mut app, _dir) = seeded_app();
    // Keep this fixture's historical 100-item page expectations explicit;
    // Store now uses Settings.page_size rather than a hard-coded value.
    app.config.settings.insert("page_size".into(), "100".into());
    app.config.settings.insert("paging_on".into(), "0".into());
    app.folder_filter = "CATALOG".into();
    app.catalog_state.items = (0..225)
        .map(|i| catalog::CatalogItem {
            title: format!("Catalog game {i:03}"),
            topic_id: format!("page-test-{i:03}"),
            magnet: format!("magnet:?xt=urn:btih:{i:040x}"),
            genre: "Action".into(),
            ..Default::default()
        })
        .collect();
    app.catalog_search_cache = None;
    let ctx = run_app_ticks(&mut app, 3);
    assert_eq!(app.catalog_matching_indices.len(), 225);
    let next = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_pagination_next"))
            .expect("store next-page button missing")
            .center()
    });
    let render = |app: &mut NslApp, events: Vec<egui::Event>| {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    };
    for pressed in [true, false] {
        render(
            &mut app,
            vec![
                egui::Event::PointerMoved(next),
                egui::Event::PointerButton {
                    pos: next,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(app.catalog_page, 1, "store next button should advance page");
    app.catalog_page = 2;
    render(&mut app, vec![]);
    assert_eq!(
        ctx.data_mut(|data| {
            data.get_temp::<usize>(egui::Id::new("nsl_test_catalog_visible_items"))
        }),
        Some(25),
        "last catalog page should contain only the remaining 25 games"
    );
    app.catalog_genre_filter = "Puzzle".into();
    render(&mut app, vec![]);
    assert_eq!(app.catalog_page, 0);
    assert!(app.catalog_matching_indices.is_empty());
    assert_eq!(app.catalog_state.items.len(), 225);
}

#[test]
fn store_sidebar_filter_changes_reset_pagination_and_center_rows() {
    let (mut app, _dir) = seeded_app();
    app.catalog_state.items = vec![
        catalog::CatalogItem {
            title: "Alpha [NSZ]".into(),
            topic_id: "a".into(),
            genre: "Action".into(),
            image_format: "NSZ".into(),
            ..Default::default()
        },
        catalog::CatalogItem {
            title: "Beta [XCI]".into(),
            topic_id: "b".into(),
            genre: "Puzzle".into(),
            image_format: "XCI".into(),
            ..Default::default()
        },
    ];
    app.folder_filter = "CATALOG".into();
    app.catalog_genre_filter = "Action".into();
    app.catalog_format_filter = "NSZ".into();
    app.catalog_page = 99;
    app.catalog_search_cache = None;
    let ctx = run_app_ticks(&mut app, 3);
    assert_eq!(app.catalog_matching_indices, vec![0]);
    assert_eq!(app.catalog_page, 0);
    assert!(ctx
        .data_mut(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
        })
        .is_some());
    app.catalog_genre_filter = "Puzzle".into();
    run_app_ticks(&mut app, 2);
    assert_eq!(app.catalog_matching_indices.len(), 0);
    app.catalog_format_filter = "XCI".into();
    run_app_ticks(&mut app, 2);
    assert_eq!(app.catalog_matching_indices, vec![1]);
    app.catalog_search = "alpha".into();
    app.catalog_search_committed = "alpha".into();
    run_app_ticks(&mut app, 2);
    assert!(app.catalog_matching_indices.is_empty());
}

#[test]
fn store_and_download_tabs_have_consistent_compact_rows_at_three_widths() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    // Management views have independent right-hand controls even when
    // the library still has a selected game and a saved detail width.
    let selected_library_game = app.rows.iter().find(|row| row.has_base).unwrap().id;
    app.selected = Some(selected_library_game);
    // The real torrent worker normally supplies the live snapshot each
    // frame. Keep synthetic status cases intact during layout-only QA.
    app.torrent_engine = None;
    app.catalog_state.items = (0..240)
        .map(|i| catalog::CatalogItem {
            title: format!(
                "Long Switch catalog game number {i:04} with extended release name {}",
                "X".repeat(80)
            ),
            topic_id: format!("layout-{i:04}"),
            title_id: Some(format!("0100{i:012X}")),
            size: "12.5 GB".into(),
            image_format: "NSP + Update + DLC".into(),
            ..Default::default()
        })
        .collect();
    app.catalog_search_cache = None;
    app.catalog_page = 1;
    app.torrent_progress = vec![
        (
            "downloading".into(),
            torrent::TorrentProgress {
                name: "A very long game name with an update and multiple DLCs ".repeat(6),
                total_bytes: 10_000_000,
                progress_bytes: 3_000_000,
                download_speed_bps: 400_000,
                finished: false,
                paused: false,
                state: "다운로드 중".into(),
                error: None,
                ..Default::default()
            },
        ),
        (
            "paused".into(),
            torrent::TorrentProgress {
                name: "Paused game".into(),
                total_bytes: 400_000,
                progress_bytes: 100_000,
                download_speed_bps: 0,
                finished: false,
                paused: true,
                state: "일시정지".into(),
                error: None,
                ..Default::default()
            },
        ),
        (
            "finished".into(),
            torrent::TorrentProgress {
                name: "Completed game".into(),
                total_bytes: 600_000,
                progress_bytes: 600_000,
                download_speed_bps: 0,
                finished: true,
                paused: false,
                state: "완료".into(),
                error: None,
                ..Default::default()
            },
        ),
        (
            "error".into(),
            torrent::TorrentProgress {
                name: "Failed torrent".into(),
                total_bytes: 900_000,
                progress_bytes: 20_000,
                download_speed_bps: 0,
                finished: false,
                paused: false,
                state: "오류".into(),
                error: Some("로컬 테스트 오류".into()),
                ..Default::default()
            },
        ),
    ];
    for (tab, marker) in [
        ("CATALOG", "nsl_test_catalog_last_row"),
        ("DOWNLOADS", "nsl_test_download_last_row"),
    ] {
        app.folder_filter = tab.into();
        let pagination_marker = if tab == "CATALOG" {
            "nsl_test_catalog_pagination_rect"
        } else {
            "nsl_test_download_pagination_rect"
        };
        let sidebar_marker = if tab == "CATALOG" {
            "nsl_test_store_sidebar_rect"
        } else {
            "nsl_test_download_sidebar_rect"
        };
        let search_marker = if tab == "CATALOG" {
            "nsl_test_store_sidebar_search"
        } else {
            "nsl_test_download_sidebar_search"
        };
        let results_marker = if tab == "CATALOG" {
            "nsl_test_store_result_count"
        } else {
            "nsl_test_download_result_count"
        };
        let reset_marker = if tab == "CATALOG" {
            "nsl_test_store_reset_button"
        } else {
            "nsl_test_download_reset_button"
        };
        for width in [1456.0, 980.0, 720.0] {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 750.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let output = ctx.end_pass();
            let rect = ctx
                    .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new(marker)))
                    .unwrap_or_else(|| panic!(
                        "missing {marker} tab={tab} width={width} actual_filter={} catalog_items={} matched={} page={} settings_open={}",
                        app.folder_filter, app.catalog_state.items.len(),
                        app.catalog_matching_indices.len(), app.catalog_page, app.settings_open,
                    ));
            let (sidebar, search, pagination, results, reset) = ctx.data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new(sidebar_marker))
                        .expect("management right sidebar missing"),
                    data.get_temp::<egui::Rect>(egui::Id::new(search_marker))
                        .expect("management sidebar search missing"),
                    data.get_temp::<egui::Rect>(egui::Id::new(pagination_marker))
                        .expect("management list pagination missing"),
                    data.get_temp::<egui::Rect>(egui::Id::new(results_marker))
                        .expect("management search count missing"),
                    data.get_temp::<egui::Rect>(egui::Id::new(reset_marker))
                        .expect("management reset button missing"),
                )
            });
            assert!(
                sidebar.width() >= 234.0 && sidebar.width() <= 366.0,
                "{tab} sidebar width at {width}: {sidebar:?}"
            );
            assert!(
                (sidebar.right() - width).abs() < 3.0,
                "{tab} panel does not touch the right edge: {sidebar:?}"
            );
            assert!(
                search.left() >= sidebar.left() + 12.0 && search.right() <= sidebar.right() - 12.0,
                "{tab} search lacks sidebar padding: search={search:?}, sidebar={sidebar:?}"
            );
            assert!(
                    search.height() >= 26.0 && results.bottom() < reset.top()
                        && reset.left() >= sidebar.left() + 12.0
                        && reset.right() <= sidebar.right() - 12.0,
                    "{tab} results/reset ordering or search height failed at {width}: search={search:?} results={results:?} reset={reset:?}"
                );
            if tab == "CATALOG" {
                for (label, id) in [
                    ("카탈로그 새로고침", "nsl_test_store_refresh_button"),
                    ("게임 다운로드", "nsl_test_store_game_download_button"),
                ] {
                    let button = ctx
                        .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new(id)).unwrap());
                    // Inspect the actual painted glyph position, not just
                    // the click rectangle: min_size alone formerly left
                    // these two labels aligned to the left.
                    let painted = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == label => Some(text),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        painted.len(),
                        1,
                        "missing or duplicate button label={label} width={width}"
                    );
                    let glyph_center = painted[0].pos.x + painted[0].galley.size().x * 0.5;
                    assert!(
                            (glyph_center - button.center().x).abs() <= 2.0,
                            "{label} text is not horizontally centered at {width}: painted={glyph_center}, button={button:?}"
                        );
                    assert!(
                            (button.width() - reset.width()).abs() < 2.0,
                            "{label} differs from reset button width at {width}: {button:?} vs {reset:?}"
                        );
                }
            }
            assert!(
                    pagination.left() < sidebar.left()
                        && pagination.right() <= sidebar.left() + 1.0
                        && pagination.top() > 70.0
                        && pagination.bottom() < rect.top(),
                    "{tab} paging is not above the left-side list: pagination={pagination:?}, row={rect:?}, sidebar={sidebar:?}"
                );
            assert!(
                rect.is_finite() && rect.left() >= -1.0 && rect.right() <= width + 1.0,
                "{tab} row spills off screen at {width}: {rect:?}"
            );
            assert!(
                    rect.right() <= sidebar.left() + 1.0
                        && rect.right() >= sidebar.left() - 65.0,
                    "{tab} row does not occupy the available center width: row={rect:?}, sidebar={sidebar:?}"
                );
            assert!(
                rect.height() >= 60.0 && rect.height() < 175.0,
                "{tab} row height differs from library compact layout: {rect:?}"
            );
            assert!(
                rect.top() > 80.0,
                "{tab} row overlaps the library tab/header: {rect:?}"
            );
        }
    }
    assert_eq!(
        app.catalog_state.items.len(),
        240,
        "catalog view unexpectedly modified source content"
    );
    assert_eq!(
        app.torrent_progress.len(),
        4,
        "downloading/paused/completed/failed rows must remain intact"
    );
    assert_eq!(
        app.selected,
        Some(selected_library_game),
        "switching management tabs must not lose the selected library game"
    );
}

#[test]
fn download_empty_state_is_centered_and_opens_store_without_torrent_side_effects() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None;
    app.torrent_progress.clear();
    app.download_search.clear();
    let mut action = egui::Rect::NOTHING;
    for (width, height) in [(1456.0, 900.0), (980.0, 680.0), (720.0, 600.0)] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        let (card, title, button, sidebar) = ctx.data_mut(|data| {
            (
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_empty_card"))
                    .expect("empty queue card missing"),
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_empty_heading"))
                    .expect("empty queue heading missing"),
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_empty_action"))
                    .expect("empty queue store button missing"),
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_sidebar_rect"))
                    .expect("download sidebar missing"),
            )
        });
        assert!(
                card.width() >= 250.0
                    && card.width() <= 510.0
                    && card.left() > 10.0
                    && card.right() <= sidebar.left() - 2.0
                    && card.top() > 85.0
                    && card.bottom() <= height - 32.0,
                "empty card overflows content area at {width}x{height}: card={card:?}, sidebar={sidebar:?}"
            );
        assert!(
                (button.center().x - card.center().x).abs() < 2.0
                    && (title.center().x - card.center().x).abs() < 2.0
                    && button.top() > title.bottom(),
                "empty card heading/button alignment failed at {width}: card={card:?}, title={title:?}, button={button:?}"
            );
        action = button;
    }

    // Activate the actual central call-to-action. The state change is
    // identical to the existing "스토어로 이동" button in the sidebar.
    for pressed in [true, false] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(720.0, 600.0),
            )),
            events: vec![
                egui::Event::PointerMoved(action.center()),
                egui::Event::PointerButton {
                    pos: action.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    }
    assert_eq!(app.folder_filter, "CATALOG");
    assert_eq!(app.current_page, 0);
    assert_eq!(
        app.config
            .settings
            .get("last_tab_key")
            .and_then(serde_json::Value::as_str),
        Some("CATALOG")
    );
    assert!(app.torrent_progress.is_empty());
}

#[test]
fn download_filtered_empty_state_clears_only_filters_and_preserves_jobs() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None;
    app.torrent_progress = vec![(
        "still-there".into(),
        torrent::TorrentProgress {
            name: "Saved update".into(),
            paused: true,
            ..Default::default()
        },
    )];
    app.download_search = "no matching title".into();
    app.download_status_filter = "active".into();
    app.download_page = 4;
    let render = |app: &mut NslApp, events| {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(980.0, 680.0),
            )),
            events,
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    };
    render(&mut app, Vec::new());
    let action = ctx.data_mut(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_empty_action"))
            .expect("filtered empty CTA missing")
            .center()
    });
    for pressed in [true, false] {
        render(
            &mut app,
            vec![
                egui::Event::PointerMoved(action),
                egui::Event::PointerButton {
                    pos: action,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(app.folder_filter, "DOWNLOADS");
    assert!(app.download_search.is_empty());
    assert_eq!(app.download_status_filter, "all");
    assert_eq!(app.download_page, 0);
    assert_eq!(app.torrent_progress.len(), 1);
    assert_eq!(app.torrent_progress[0].0, "still-there");
}

#[test]
fn store_first_catalog_item_is_fully_visible_after_opening_tab() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.catalog_state.items = (0..25)
        .map(|i| catalog::CatalogItem {
            title: format!("Game {i:03}: update + DLC"),
            topic_id: format!("first-row-{i:04}"),
            magnet: format!("magnet:?xt=urn:btih:{i:040x}"),
            size: "12.5 GB".into(),
            ..Default::default()
        })
        .collect();
    app.catalog_search_cache = None;
    app.catalog_page = 0;
    app.folder_filter = "CATALOG".into();
    app.selected = None;
    for _ in 0..4 {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1296.0, 799.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    }
    let (first, viewport) = ctx.data_mut(|data| {
        (
            data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
                .unwrap(),
            data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_viewport"))
                .unwrap(),
        )
    });
    eprintln!("CATALOG_FIRST_ROW={first:?} VIEWPORT={viewport:?}");
    assert!(
        first.top() >= viewport.top() - 1.0,
        "first catalog item clipped above viewport; first={first:?}, viewport={viewport:?}"
    );
    assert!(
        first.bottom() <= viewport.bottom() + 1.0,
        "first catalog item clipped below viewport"
    );
}

#[test]
fn store_catalog_does_not_truncate_at_two_thousand_entries() {
    let (mut app, _dir) = seeded_app();
    app.config.settings.insert("page_size".into(), "100".into());
    app.config.settings.insert("paging_on".into(), "0".into());
    app.catalog_state.items = (0..2_101)
        .map(|i| catalog::CatalogItem {
            title: format!("Catalog item {i}"),
            magnet: format!("magnet:?xt=urn:btih:{i}"),
            ..Default::default()
        })
        .collect();
    app.folder_filter = "CATALOG".into();
    run_app_ticks(&mut app, 2);
    assert_eq!(app.catalog_matching_indices.len(), 2_101);
    app.catalog_page = 21;
    run_app_ticks(&mut app, 1);
    assert_eq!(app.catalog_page, 21);
}

#[test]
fn catalog_cover_jobs_respect_disk_cache_and_retry_cooldown() {
    let (mut app, dir) = seeded_app();
    let item = catalog::CatalogItem {
        topic_id: "opt-test-001".into(),
        cover: "https://example.test/cover.jpg".into(),
        ..Default::default()
    };
    let now = std::time::Instant::now();
    assert!(app.should_prefetch_catalog_cover(&item, now));
    app.catalog_cover_retry_after
        .insert(item.topic_id.clone(), now + STORE_COVER_RETRY_DELAY);
    assert!(!app.should_prefetch_catalog_cover(&item, now));
    assert!(app.should_prefetch_catalog_cover(
        &item,
        now + STORE_COVER_RETRY_DELAY + std::time::Duration::from_millis(1)
    ));
    let cover_dir = dir.path().join("icons").join("store");
    std::fs::create_dir_all(&cover_dir).unwrap();
    std::fs::write(cover_dir.join("opt-test-001.jpg"), b"cached").unwrap();
    assert!(!app.should_prefetch_catalog_cover(
        &item,
        now + STORE_COVER_RETRY_DELAY + std::time::Duration::from_secs(1)
    ));
    let invalid = catalog::CatalogItem {
        topic_id: "../escape".into(),
        ..item
    };
    assert!(!app.should_prefetch_catalog_cover(&invalid, now));
}

#[test]
fn background_messages_do_not_force_log_window_open() {
    let (mut app, _dir) = seeded_app();
    assert!(!app.log_open);
    app.log_sender
        .send("background cover update".into())
        .unwrap();
    run_app_ticks(&mut app, 2);
    assert!(app
        .log_lines
        .iter()
        .any(|line| line == "background cover update"));
    assert!(
        !app.log_open,
        "background logs must not obscure the library"
    );
}

#[test]
fn log_panel_renders_only_visible_rows_when_messages_grow() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.log_open = true;
    let render = |app: &mut NslApp| -> usize {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 760.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        ctx.end_pass().shapes.len()
    };
    app.log_lines = (0..10).map(|i| format!("log {i:04}")).collect();
    let small = render(&mut app);
    app.log_lines = (0..2_000).map(|i| format!("log {i:04}")).collect();
    let large = render(&mut app);
    assert!(
        large < small + 75,
        "log was laid out in full: {small} -> {large} shapes"
    );
}

#[test]
fn footer_stays_at_screen_bottom_with_logs_open_and_closed() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.log_lines = (0..120).map(|i| format!("작업 로그 {i}")).collect();
    for (width, height) in [
        (1456.0, 900.0),
        (980.0, 640.0),
        (680.0, 420.0),
        (500.0, 330.0),
    ] {
        for log_open in [false, true] {
            app.log_open = log_open;
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
            let (status, progress, log) = ctx.data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_status_rect"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_status_progress_rect"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_log_rect"))
                        .unwrap(),
                )
            });
            assert!(
                (status.bottom() - height).abs() <= 1.0,
                "footer left the bottom: {status:?}, h={height}"
            );
            assert!(
                progress.top() >= status.top() - 1.0 && progress.bottom() <= status.bottom() + 1.0,
                "progress left the fixed status bar: {progress:?} vs {status:?}"
            );
            assert!(
                progress.right() <= width + 1.0,
                "progress overflows width {width}"
            );
            if log_open {
                assert!(log.is_positive(), "log panel was not rendered");
                let scroll = ctx.data_mut(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_log_scroll_rect"))
                        .unwrap()
                });
                assert!(
                    scroll.bottom() <= status.top() + 0.5,
                    "log scroll area entered the status bar: {scroll:?} vs {status:?}"
                );
                assert!(
                    log.bottom() <= status.top() + 0.5,
                    "log must never cover footer: log={log:?}, status={status:?}"
                );
            } else {
                assert!(!log.is_positive(), "closed log consumes screen space");
            }
        }
    }
}

#[test]
fn footer_progress_has_real_percentages_and_indeterminate_stages() {
    let (mut app, _dir) = seeded_app();
    let (_tx, rx) = mpsc::channel::<std::result::Result<titledb::UpdateOutcome, String>>();
    app.titledb_result = Some(rx);
    let progress = Arc::new(TaskProgress::default());
    progress.begin("타이틀 DB 2/3 · versions.json", Some(100), true);
    progress.set(37);
    app.titledb_progress = Some(progress.clone());
    let views = app.active_progress_views();
    let titledb = views
        .iter()
        .find(|view| view.label.contains("versions.json"))
        .unwrap();
    assert_eq!(titledb.fraction, Some(0.37));
    assert!(!titledb.detail.is_empty());
    progress.begin("타이틀 DB 서버 연결 중", None, true);
    let waiting = app.active_progress_views();
    let titledb = waiting
        .iter()
        .find(|view| view.label.contains("연결 중"))
        .unwrap();
    assert!(
        titledb.fraction.is_none(),
        "unknown size must not fabricate a percent"
    );
    app.titledb_result = None;
    app.titledb_progress = None;
    let (_tx, rx) = mpsc::channel::<std::result::Result<(usize, usize), String>>();
    app.bulk_rename_result = Some(rx);
    let rename = Arc::new(TaskProgress::default());
    rename.begin("파일명 일괄 정규화", Some(5), false);
    rename.set(3);
    app.rename_progress = Some(rename);
    assert!(app
        .active_progress_views()
        .iter()
        .any(|view| view.fraction == Some(0.6)));
}

#[test]
fn escape_clears_both_edit_buffer_and_committed_filters() {
    // Regression from real Windows GUI QA: Esc emptied the search edit,
    // while its debounced query still filtered the library indefinitely.
    let (mut app, _dir) = seeded_app();
    app.search = "nonexistent-game".into();
    app.search_committed = app.search.clone();
    app.search_pending = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
    app.catalog_search = "Mario".into();
    app.catalog_search_committed = "Mario".into();
    app.catalog_search_pending =
        Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
    app.folder_filter = "CATALOG".into();
    app.current_page = 3;
    app.catalog_page = 2;
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 860.0),
        )),
        ..Default::default()
    });
    app.ui_frame(&ctx);
    let _ = ctx.end_pass();
    assert_eq!(app.search, "");
    assert_eq!(app.search_committed, "");
    assert!(app.search_pending.is_none());
    assert_eq!(app.catalog_search, "");
    assert_eq!(app.catalog_search_committed, "");
    assert!(app.catalog_search_pending.is_none());
    assert_eq!(app.current_page, 0);
    assert_eq!(app.catalog_page, 0);
    app.folder_filter = "DOWNLOADS".into();
    app.download_search = "previous query".into();
    app.download_page = 3;
    ctx.begin_pass(egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 860.0),
        )),
        ..Default::default()
    });
    app.ui_frame(&ctx);
    let _ = ctx.end_pass();
    assert!(app.download_search.is_empty());
    assert_eq!(app.download_page, 0);
}

#[test]
fn escape_closes_settings_without_touching_searches_or_filters_behind_it() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    // Escape from Settings behaves exactly like its close button, not the
    // global search-clear shortcut used by the underlying library views.
    for tab in ["", "CATALOG", "DOWNLOADS"] {
        for category in [0, 1, 4, 6] {
            app.folder_filter = tab.into();
            app.settings_open = true;
            app.settings_category = category;
            app.search = "my library query".into();
            app.search_committed = "my library query".into();
            app.catalog_search = "my store query".into();
            app.catalog_search_committed = "my store query".into();
            app.download_search = "my download query".into();
            app.catalog_genre_filter = "Action".into();
            app.catalog_format_filter = "NSZ".into();
            app.catalog_language_filter = "ko".into();
            ctx.begin_pass(egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 780.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
            assert!(
                !app.settings_open,
                "Esc failed to close settings at category={category}, tab={tab}"
            );
            assert_eq!(app.search, "my library query");
            assert_eq!(app.search_committed, "my library query");
            assert_eq!(app.catalog_search, "my store query");
            assert_eq!(app.catalog_search_committed, "my store query");
            assert_eq!(app.download_search, "my download query");
            assert_eq!(app.catalog_genre_filter, "Action");
            assert_eq!(app.catalog_format_filter, "NSZ");
            assert_eq!(app.catalog_language_filter, "ko");
        }
    }
}

/// Deliver a real egui keyboard event without sending global Windows input.
fn send_escape_to_app(app: &mut NslApp, ctx: &egui::Context) {
    ctx.begin_pass(egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    });
    app.ui_frame(ctx);
    let _ = ctx.end_pass();
}

#[test]
fn escape_closes_all_dialog_kinds_before_settings_log_or_filters() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.torrent_engine = None;
    for index in 0..17 {
        // A popup from Settings or Downloads must close without also closing
        // Settings or the log pane, or activating a destructive action.
        app.settings_open = true;
        app.log_open = true;
        app.search = "library remains".into();
        app.search_committed = app.search.clone();
        app.catalog_search = "store remains".into();
        app.catalog_search_committed = app.catalog_search.clone();
        app.download_search = "download remains".into();
        app.catalog_genre_filter = "Action".into();
        match index {
            0 => app.rename_preview = Some(("rename-from".into(), "rename-to".into())),
            1 => {
                app.catalog_torrent_dialog = Some(CatalogTorrentDialog {
                    item: catalog::CatalogItem {
                        title: "Escape selection dialog".into(),
                        ..Default::default()
                    },
                    request: None,
                    files: None,
                    error: Some("Test preview error".into()),
                });
            }
            2 => app.download_delete_confirm = Some(("t42".into(), "test download".into())),
            3 => app.bulk_rename_preview = Some(vec![("from".into(), "to".into())]),
            4 => app.titledb_info = Some("DB file status".into()),
            5 => app.clear_titledb_confirm = true,
            6 => app.clear_library_confirm = true,
            7 => app.remove_folder_confirm = Some("E:\\NSW".into()),
            8 => app.folder_notice = Some("Notice".into()),
            9 => app.orphan_cleanup_confirm = true,
            10 => app.workspace_cleanup_confirm = true,
            11 => {
                app.update_info = Some(updater::ReleaseInfo {
                    current_version: "a".into(),
                    latest_version: "b".into(),
                    latest_commit: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                    release_tag: "test".into(),
                    is_newer: true,
                    notes: "Synthetic update".into(),
                    download_url: None,
                });
            }
            12 => app.update_ready = Some(PathBuf::from("uninstalled-update")),
            13 => app.cache_info = Some("cache status".into()),
            14 => app.shortcuts_open = true,
            15 => app.about_open = true,
            16 => app.exit_confirm = true,
            _ => unreachable!(),
        }
        // Render the actual popup before the Escape frame.
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        send_escape_to_app(&mut app, &ctx);
        let closed = match index {
            0 => app.rename_preview.is_none(),
            1 => app.catalog_torrent_dialog.is_none(),
            2 => app.download_delete_confirm.is_none(),
            3 => app.bulk_rename_preview.is_none(),
            4 => app.titledb_info.is_none(),
            5 => !app.clear_titledb_confirm,
            6 => !app.clear_library_confirm,
            7 => app.remove_folder_confirm.is_none(),
            8 => app.folder_notice.is_none(),
            9 => !app.orphan_cleanup_confirm,
            10 => !app.workspace_cleanup_confirm,
            11 => app.update_info.is_none(),
            12 => app.update_ready.is_none(),
            13 => app.cache_info.is_none(),
            14 => !app.shortcuts_open,
            15 => !app.about_open,
            16 => !app.exit_confirm,
            _ => unreachable!(),
        };
        assert!(closed, "Esc failed to close popup kind {index}");
        assert!(
            app.settings_open,
            "Esc also closed settings at popup kind {index}"
        );
        assert!(
            app.log_open,
            "Esc also closed the log at popup kind {index}"
        );
        assert!(!app.exit_confirmed, "Esc unexpectedly approved exit");
        assert_eq!(app.search, "library remains");
        assert_eq!(app.search_committed, "library remains");
        assert_eq!(app.catalog_search, "store remains");
        assert_eq!(app.catalog_search_committed, "store remains");
        assert_eq!(app.download_search, "download remains");
        assert_eq!(app.catalog_genre_filter, "Action");
        app.settings_open = false;
        app.log_open = false;
    }
}

#[test]
fn escape_closes_only_topmost_popup_then_settings_then_log_then_filters() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "DOWNLOADS".into();
    app.torrent_engine = None;
    app.search = "keep library".into();
    app.search_committed = "keep library".into();
    app.download_search = "keep download".into();
    app.download_delete_confirm = Some(("t1".into(), "do not delete".into()));
    app.about_open = true;
    app.exit_confirm = true;
    app.settings_open = true;
    app.log_open = true;
    app.log_lines = vec!["keep log contents".into()];

    send_escape_to_app(&mut app, &ctx);
    assert!(!app.exit_confirm && app.about_open);
    assert!(app.download_delete_confirm.is_some() && app.settings_open && app.log_open);
    send_escape_to_app(&mut app, &ctx);
    assert!(!app.about_open && app.download_delete_confirm.is_some());
    send_escape_to_app(&mut app, &ctx);
    assert!(app.download_delete_confirm.is_none());
    assert!(app.settings_open && app.log_open);
    send_escape_to_app(&mut app, &ctx);
    assert!(!app.settings_open && app.log_open);
    send_escape_to_app(&mut app, &ctx);
    assert!(!app.log_open);
    assert_eq!(app.log_lines, ["keep log contents"]);
    assert_eq!(app.search, "keep library");
    assert_eq!(app.download_search, "keep download");
    assert!(!app.exit_confirmed);

    // With no overlays, Esc retains the existing search-clear behavior.
    send_escape_to_app(&mut app, &ctx);
    assert!(app.search.is_empty());
    assert!(app.search_committed.is_empty());
    assert!(app.download_search.is_empty());
    assert_eq!(app.log_lines, ["keep log contents"]);
}

#[test]
fn escape_closes_native_context_menu_before_dialogs_and_log() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    app.folder_filter = "DOWNLOADS".into();
    app.download_search = "preserved query".into();
    app.about_open = true;
    app.settings_open = true;
    app.log_open = true;
    ctx.memory_mut(|memory| memory.open_popup(egui::Id::new("test_native_context_menu")));
    assert!(ctx.memory(|memory| memory.any_popup_open()));

    send_escape_to_app(&mut app, &ctx);
    assert!(!ctx.memory(|memory| memory.any_popup_open()));
    assert!(app.about_open && app.settings_open && app.log_open);
    assert_eq!(app.download_search, "preserved query");

    send_escape_to_app(&mut app, &ctx);
    assert!(!app.about_open && app.settings_open && app.log_open);
    send_escape_to_app(&mut app, &ctx);
    assert!(!app.settings_open && app.log_open);
    send_escape_to_app(&mut app, &ctx);
    assert!(!app.log_open);
    assert_eq!(app.download_search, "preserved query");
    send_escape_to_app(&mut app, &ctx);
    assert!(app.download_search.is_empty());
}

/// A desktop-style key-down/up cycle through the real egui update loop.
fn management_key_frame(app: &mut NslApp, ctx: &egui::Context, key: egui::Key) {
    for pressed in [true, false] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        });
        app.ui_frame(ctx);
        let _ = ctx.end_pass();
    }
}

#[test]
fn store_uses_configured_page_size_even_when_library_paging_is_disabled() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.torrent_engine = None;
    app.catalog_title_metadata_loaded = true;
    app.catalog_state.items = (0..9)
        .map(|i| catalog::CatalogItem {
            title: format!("Store title {i}"),
            topic_id: format!("store-{i:03}"),
            ..Default::default()
        })
        .collect();
    app.config.settings.insert("paging_on".into(), "0".into());
    app.config.settings.insert("page_size".into(), "3".into());
    app.catalog_search_cache = None;
    draw_card_test_frames(&ctx, &mut app, 3);
    assert_eq!(app.catalog_page, 0);
    assert_eq!(app.catalog_matching_indices.len(), 9);
    let visible = ctx.data_mut(|data| {
        data.get_temp::<usize>(egui::Id::new("nsl_test_catalog_visible_items"))
            .unwrap()
    });
    assert_eq!(visible, 3);

    app.catalog_page = 1;
    draw_card_test_frames(&ctx, &mut app, 2);
    assert_eq!(app.catalog_page, 1);
    assert_eq!(
        ctx.data_mut(|data| data.get_temp::<usize>(egui::Id::new("nsl_test_catalog_visible_items"))),
        Some(3)
    );
    app.config.settings.insert("page_size".into(), "2".into());
    app.catalog_page = 0;
    draw_card_test_frames(&ctx, &mut app, 2);
    assert_eq!(
        ctx.data_mut(|data| data.get_temp::<usize>(egui::Id::new("nsl_test_catalog_visible_items"))),
        Some(2)
    );
    app.catalog_page = 4;
    draw_card_test_frames(&ctx, &mut app, 2);
    assert_eq!(app.catalog_page, 4);
    assert_eq!(
        ctx.data_mut(|data| data.get_temp::<usize>(egui::Id::new("nsl_test_catalog_visible_items"))),
        Some(1)
    );
    assert_eq!(configured_game_page_size(&app.config.settings), 2);
}

#[test]
fn store_and_downloads_keyboard_navigation_crosses_page_boundaries() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.torrent_engine = None;
    app.catalog_title_metadata_loaded = true;
    app.catalog_state.items = (0..9)
        .map(|i| catalog::CatalogItem {
            title: format!("Keyboard title {i}"),
            topic_id: format!("store-{i:03}"),
            ..Default::default()
        })
        .collect();
    app.catalog_search_cache = None;
    app.config.settings.insert("page_size".into(), "3".into());
    app.config.settings.insert("paging_on".into(), "0".into());
    draw_card_test_frames(&ctx, &mut app, 3);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-000"));
    management_key_frame(&mut app, &ctx, egui::Key::ArrowDown);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-001"));
    management_key_frame(&mut app, &ctx, egui::Key::ArrowRight);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-002"));
    management_key_frame(&mut app, &ctx, egui::Key::ArrowDown);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-003"));
    assert_eq!(app.catalog_page, 1);
    management_key_frame(&mut app, &ctx, egui::Key::End);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-008"));
    assert_eq!(app.catalog_page, 2);
    management_key_frame(&mut app, &ctx, egui::Key::Home);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-000"));
    assert_eq!(app.catalog_page, 0);
    // Search text editing and a modal dialog must override list shortcuts.
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("catalog_search")));
    management_key_frame(&mut app, &ctx, egui::Key::ArrowDown);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-000"));
    ctx.memory_mut(|memory| memory.surrender_focus(egui::Id::new("catalog_search")));
    app.about_open = true;
    management_key_frame(&mut app, &ctx, egui::Key::ArrowDown);
    assert_eq!(app.catalog_selected.as_deref(), Some("store-000"));
    app.about_open = false;

    app.folder_filter = "DOWNLOADS".into();
    app.download_selected = None;
    app.torrent_progress = (0..105)
        .map(|i| {
            (
                format!("job-{i:03}"),
                torrent::TorrentProgress {
                    name: format!("Download {i}"),
                    total_bytes: 1000,
                    progress_bytes: 100,
                    ..Default::default()
                },
            )
        })
        .collect();
    draw_card_test_frames(&ctx, &mut app, 3);
    management_key_frame(&mut app, &ctx, egui::Key::ArrowDown);
    assert_eq!(app.download_selected.as_deref(), Some("job-000"));
    management_key_frame(&mut app, &ctx, egui::Key::ArrowRight);
    assert_eq!(app.download_selected.as_deref(), Some("job-001"));
    management_key_frame(&mut app, &ctx, egui::Key::End);
    assert_eq!(app.download_selected.as_deref(), Some("job-104"));
    assert_eq!(app.download_page, 1);
    management_key_frame(&mut app, &ctx, egui::Key::Home);
    assert_eq!(app.download_selected.as_deref(), Some("job-000"));
    assert_eq!(app.download_page, 0);
    // Store's configured page size must not silently change download paging.
    assert_eq!(
        ctx.data_mut(
            |data| data.get_temp::<usize>(egui::Id::new("nsl_test_download_visible_items"))
        ),
        Some(100)
    );
}

#[test]
fn management_keyboard_end_reveals_selected_row_in_long_scrollable_lists() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.folder_filter = "CATALOG".into();
    app.torrent_engine = None;
    app.catalog_title_metadata_loaded = true;
    app.config.settings.insert("page_size".into(), "120".into());
    app.catalog_state.items = (0..120)
        .map(|index| catalog::CatalogItem {
            title: format!("Scrolled title {index}"),
            topic_id: format!("scroll-{index:03}"),
            ..Default::default()
        })
        .collect();
    app.catalog_search_cache = None;
    draw_card_test_frames(&ctx, &mut app, 3);
    management_key_frame(&mut app, &ctx, egui::Key::End);
    draw_card_test_frames(&ctx, &mut app, 3);
    assert_eq!(app.catalog_selected.as_deref(), Some("scroll-119"));
    let ((id, row), viewport) = ctx.data_mut(|data| {
        (
            data.get_temp::<(String, egui::Rect)>(egui::Id::new("nsl_test_catalog_selected_row"))
                .expect("selected virtualized store row was not rendered"),
            data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_viewport"))
                .unwrap(),
        )
    });
    assert_eq!(id, "scroll-119");
    assert!(
        row.top() >= viewport.top() - 3.0 && row.bottom() <= viewport.bottom() + 3.0,
        "selected Store row is outside the visible viewport: row={row:?}, viewport={viewport:?}"
    );

    app.folder_filter = "DOWNLOADS".into();
    app.torrent_progress = (0..100)
        .map(|index| {
            (
                format!("download-{index:03}"),
                torrent::TorrentProgress {
                    name: format!("Test task {index}"),
                    total_bytes: 10_000,
                    progress_bytes: 1_000,
                    ..Default::default()
                },
            )
        })
        .collect();
    app.download_page = 0;
    app.download_selected = None;
    app.download_scroll_reset = true;
    draw_card_test_frames(&ctx, &mut app, 2);
    management_key_frame(&mut app, &ctx, egui::Key::End);
    draw_card_test_frames(&ctx, &mut app, 3);
    assert_eq!(app.download_selected.as_deref(), Some("download-099"));
    let ((id, row), viewport) = ctx.data_mut(|data| {
        (
            data.get_temp::<(String, egui::Rect)>(egui::Id::new("nsl_test_download_selected_row"))
                .expect("selected Download row was not rendered"),
            data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_download_viewport"))
                .unwrap(),
        )
    });
    assert_eq!(id, "download-099");
    assert!(
        row.top() >= viewport.top() - 3.0 && row.bottom() <= viewport.bottom() + 3.0,
        "selected Download row is outside the visible viewport: row={row:?}, viewport={viewport:?}"
    );
}

#[test]
fn active_tab_draws_two_pixel_selection_accent_for_each_screen() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.torrent_engine = None;
    for theme_id in ["github_dark", "github_light", "catppuccin_mocha"] {
        app.config.settings.insert("theme".into(), theme_id.into());
        for tab_key in ["", "CATALOG", "DOWNLOADS"] {
            app.folder_filter = tab_key.into();
            draw_card_test_frames(&ctx, &mut app, 2);
            let tabs = app.library_tabs_cached().to_vec();
            let selected_index = tabs.iter().position(|(key, _)| key == tab_key).unwrap();
            let (_, _, rects) = ctx.data_mut(|data| {
                data.get_temp::<(f32, Vec<f32>, Vec<egui::Rect>)>(egui::Id::new(
                    "nsl_test_directory_tab_layout",
                ))
                .unwrap()
            });
            let (bar, color) = ctx.data_mut(|data| {
                data.get_temp::<(egui::Rect, egui::Color32)>(egui::Id::new(
                    "nsl_test_active_tab_top_bar",
                ))
                .unwrap()
            });
            let tab = rects[selected_index];
            assert_eq!(bar.width(), tab.width());
            assert_eq!(bar.height(), 2.0);
            assert_eq!(bar.min, tab.min);
            assert_eq!(color, ctx.style().visuals.hyperlink_color);
            assert_eq!(color, theme::swatches(theme_id).1);
            if theme_id == "github_dark" {
                assert_eq!(color, egui::Color32::from_rgb(0x1F, 0x6F, 0xEB));
                assert_ne!(color, ctx.style().visuals.widgets.active.bg_fill);
            }
        }
    }
}

#[test]
fn top_library_statistics_stay_small_right_aligned_and_on_the_first_row() {
    use egui::containers::panel::PanelState;

    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let mut top_heights = Vec::new();
    for width in [1456.0, 980.0, 720.0] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 800.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        let (stats_rect, menus_right) = ctx.data_mut(|data| {
            (
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_top_stats_rect"))
                    .unwrap(),
                data.get_temp::<f32>(egui::Id::new("nsl_test_top_menus_right"))
                    .unwrap(),
            )
        });
        assert!(
            stats_rect.left() >= menus_right - 1.0,
            "stats overlap top menu: width={width}, label={stats_rect:?}, menu_end={menus_right}"
        );
        assert!(
            stats_rect.right() <= width + 1.0,
            "stats overflow right edge: width={width}, label={stats_rect:?}"
        );
        assert!(
            stats_rect.top() < 40.0 && stats_rect.height() < 19.0,
            "stats should be small and on top menu row: width={width}, label={stats_rect:?}"
        );
        let top_height = PanelState::load(&ctx, egui::Id::new("top"))
            .unwrap()
            .size()
            .y;
        top_heights.push(top_height);
        assert!(
            top_height < 95.0,
            "statistics added an unwanted toolbar row: {top_height}"
        );
    }
    assert!(
        (top_heights[0] - top_heights[1]).abs() < 3.0,
        "toolbar grows on compact screens: {top_heights:?}"
    );
}

#[test]
fn store_download_share_right_sidebar_width_and_preserve_real_drag_across_tabs() {
    use egui::containers::panel::PanelState;

    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.torrent_engine = None;
    let panel_id = egui::Id::new("management_controls");
    let resize_id = panel_id.with("__resize");
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1456.0, 900.0));
    let frame = |app: &mut NslApp, events: Vec<egui::Event>| {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    };
    let set_cache = |width: f32| {
        ctx.data_mut(|data| {
            data.insert_persisted(
                panel_id,
                PanelState {
                    rect: egui::Rect::from_min_size(
                        egui::pos2(1456.0 - width, 0.0),
                        egui::vec2(width, 900.0),
                    ),
                },
            );
        });
    };
    // Both tabs must use the exact same panel cache, including at the
    // endpoints of the permitted splitter width.
    for width in [235.0_f32, 282.0, 342.0, 365.0] {
        app.config.settings.insert(
            "management_sidebar_width".into(),
            format!("{width:.0}").into(),
        );
        set_cache(width);
        for tab in ["CATALOG", "DOWNLOADS", "CATALOG", "DOWNLOADS"] {
            app.folder_filter = tab.into();
            frame(&mut app, vec![]);
            let observed = PanelState::load(&ctx, panel_id).unwrap().size().x;
            assert!(
                (observed - width).abs() <= 1.0,
                "switching to {tab} changed shared sidebar: requested={width}, actual={observed}"
            );
        }
    }
    app.config
        .settings
        .insert("management_sidebar_width".into(), "282".into());
    app.config.settings.insert("sash_pos".into(), "440".into());
    set_cache(282.0);
    app.folder_filter = "CATALOG".into();
    frame(&mut app, vec![]);
    // egui stores response hitboxes separately from the panel cache.
    // Let the resize handle catch up after the preceding 365px tab.
    frame(&mut app, vec![]);
    let start = ctx
        .read_response(resize_id)
        .expect("shared sidebar resize handle not rendered")
        .rect
        .center();
    assert!(
        (start.x - 1174.0).abs() < 3.0,
        "splitter must line up with the actual right panel: {start:?}"
    );
    let target = start + egui::vec2(-53.0, 0.0);
    let click = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut app, vec![egui::Event::PointerMoved(start)]);
    frame(&mut app, vec![click(start, true)]);
    frame(&mut app, vec![egui::Event::PointerMoved(target)]);
    frame(&mut app, vec![egui::Event::PointerMoved(target)]);
    frame(&mut app, vec![click(target, false)]);
    frame(&mut app, vec![]);
    let resized = PanelState::load(&ctx, panel_id).unwrap().size().x;
    assert!(
        (resized - 335.0).abs() < 3.0,
        "store resize did not reach target: {resized}"
    );
    let persisted = app
        .config
        .settings
        .get("management_sidebar_width")
        .and_then(|v| v.as_str())
        .unwrap()
        .parse::<f32>()
        .unwrap();
    assert!(
        (persisted - resized).abs() <= 2.0,
        "shared sidebar width was not persisted: stored={persisted} actual={resized}"
    );
    for tab in ["DOWNLOADS", "CATALOG", "DOWNLOADS"] {
        app.folder_filter = tab.into();
        frame(&mut app, vec![]);
        let actual = PanelState::load(&ctx, panel_id).unwrap().size().x;
        assert!(
            (actual - resized).abs() < 1.0,
            "resized Store width not shared with {tab}: {actual} vs {resized}"
        );
    }
    // Settings and the library's own independent sash must not overwrite
    // the shared management width.
    app.settings_open = true;
    frame(&mut app, vec![]);
    app.settings_open = false;
    app.folder_filter = "CATALOG".into();
    frame(&mut app, vec![]);
    assert!(
        (PanelState::load(&ctx, panel_id).unwrap().size().x - resized).abs() < 1.0,
        "returning from Settings changed the management sidebar size"
    );
    assert_eq!(
        app.config.settings.get("sash_pos").and_then(|v| v.as_str()),
        Some("440"),
        "management resize changed the library detail width"
    );
    let disk_config = database::load_config(&app.base_dir.join("config.json")).unwrap();
    assert_eq!(
        disk_config.settings.get("management_sidebar_width"),
        app.config.settings.get("management_sidebar_width"),
        "shared sidebar width was not saved for the next app launch"
    );
    // A stale/clamped egui panel cache must not reset the user's width.
    ctx.data_mut(|data| {
        let mut state = data.get_persisted::<PanelState>(panel_id).unwrap();
        state.rect.min.x = state.rect.max.x - 245.0;
        data.insert_persisted(panel_id, state);
    });
    frame(&mut app, vec![]);
    assert!(
        (PanelState::load(&ctx, panel_id).unwrap().size().x - resized).abs() < 1.0,
        "temporary panel layout changed the saved shared sidebar width"
    );
}

#[test]
fn detail_panel_keeps_resized_width_when_switching_between_long_game_names() {
    use egui::containers::panel::PanelState;
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);

    let template = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    let mut short = template.clone();
    short.id = 10_001;
    short.title_name = "Short Title".into();
    short.filename = "Short.nsp".into();
    let mut long = template.clone();
    long.id = 10_002;
    long.title_name = "An Extremely Long Game Title With Many Words ".repeat(12);
    long.filename = "Some Very Long Filename ".repeat(10);
    long.path = format!("E:\\NSW\\{}", long.filename);
    long.nacp_en = long.title_name.clone();
    long.nacp_ko = "긴 한글 게임 이름과 버전 정보 ".repeat(35);
    long.nacp_ja = "長い日本語ゲームタイトル".repeat(30);
    long.description = "Long game description that must wrap inside the panel. ".repeat(70);
    long.owned_dlcs = vec![("0100D2C0115B7001".into(), "DLC Long Label ".repeat(35))];
    long.missing_dlcs = vec![("0100D2C0115B7002".into(), "Other DLC ".repeat(35))];
    app.rows = vec![short, long];
    app.rows_generation = app.rows_generation.wrapping_add(1);
    app.selected = Some(10_001);
    app.folder_filter.clear();
    app.kind_filter = "base".into();
    app.config
        .settings
        .insert("sash_remember".into(), "1".into());
    let id = egui::Id::new("detail");
    let mut observed = Vec::new();
    for preferred_width in [320.0f32, 380.0, 740.0, 900.0] {
        let expected = format!("{preferred_width:.0}");
        app.config
            .settings
            .insert("sash_pos".into(), expected.clone().into());
        ctx.data_mut(|data| {
            data.insert_persisted(
                id,
                PanelState {
                    rect: egui::Rect::from_min_size(
                        egui::pos2(1456.0 - preferred_width, 0.0),
                        egui::vec2(preferred_width, 900.0),
                    ),
                },
            )
        });
        for detail_tab in 0..4 {
            app.detail_tab = detail_tab;
            for selected in [10_001, 10_002, 10_001, 10_002, 10_001] {
                app.selected = Some(selected);
                ctx.begin_pass(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1_456.0, 900.0),
                    )),
                    ..Default::default()
                });
                app.ui_frame(&ctx);
                let _ = ctx.end_pass();
                let width = PanelState::load(&ctx, id).unwrap().size().x;
                observed.push(width);
                assert!(
                        (width - preferred_width).abs() < 1.0,
                        "detail panel expanded for selected game {selected}, tab {detail_tab}, requested {preferred_width}: {observed:?}"
                    );
                assert_eq!(
                    app.config.settings.get("sash_pos").and_then(|v| v.as_str()),
                    Some(expected.as_str()),
                    "content unexpectedly overwrote the saved width"
                );
            }
        }
    }
}

#[test]
fn detail_panel_still_saves_manual_splitter_drags_and_restores_after_reselection() {
    use egui::containers::panel::PanelState;
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.selected = Some(app.rows.iter().find(|row| row.has_base).unwrap().id);
    app.config
        .settings
        .insert("sash_remember".into(), "1".into());
    app.config.settings.insert("sash_pos".into(), "380".into());

    let panel_id = egui::Id::new("detail");
    let resize_id = panel_id.with("__resize");
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1456.0, 900.0));
    let frame = |app: &mut NslApp, events: Vec<egui::Event>| {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    };
    ctx.data_mut(|data| {
        data.insert_persisted(
            panel_id,
            PanelState {
                rect: egui::Rect::from_min_size(egui::pos2(1076.0, 0.0), egui::vec2(380.0, 900.0)),
            },
        )
    });
    frame(&mut app, vec![]);
    let start = ctx.read_response(resize_id).unwrap().rect.center();
    let target = start + egui::vec2(-75.0, 0.0);
    let click = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut app, vec![egui::Event::PointerMoved(start)]);
    frame(&mut app, vec![click(start, true)]);
    frame(&mut app, vec![egui::Event::PointerMoved(target)]);
    frame(&mut app, vec![egui::Event::PointerMoved(target)]);
    frame(&mut app, vec![click(target, false)]);
    frame(&mut app, vec![]);
    let actual = PanelState::load(&ctx, panel_id).unwrap().size().x;
    assert!(
        (actual - 455.0).abs() < 3.0,
        "real splitter drag did not resize: {actual}"
    );
    let stored = app
        .config
        .settings
        .get("sash_pos")
        .and_then(|v| v.as_str())
        .unwrap()
        .parse::<f32>()
        .unwrap();
    assert!(
        (stored - actual).abs() <= 2.0,
        "drag width not persisted: stored={stored}, actual={actual}"
    );

    app.selected = None;
    frame(&mut app, vec![]);
    app.selected = Some(app.rows.iter().find(|row| row.has_base).unwrap().id);
    frame(&mut app, vec![]);
    let reopened = PanelState::load(&ctx, panel_id).unwrap().size().x;
    assert!(
        (reopened - actual).abs() < 1.0,
        "reopening changed sash width"
    );
    let disk_config = database::load_config(&app.base_dir.join("config.json")).unwrap();
    assert_eq!(
        disk_config.settings.get("sash_pos"),
        app.config.settings.get("sash_pos"),
        "manual resize width was not saved across app restarts"
    );

    app.settings_open = true;
    frame(&mut app, vec![]);
    app.settings_open = false;
    frame(&mut app, vec![]);
    let after_settings = PanelState::load(&ctx, panel_id).unwrap().size().x;
    assert!(
        (after_settings - actual).abs() < 1.0,
        "switching settings view changed the user's panel width"
    );

    // A temporary egui layout constraint can shrink its cached panel
    // rectangle without a splitter drag. The saved user width must win
    // when the layout has room again.
    ctx.data_mut(|data| {
        let mut panel = data.get_persisted::<PanelState>(panel_id).unwrap();
        panel.rect.min.x = panel.rect.max.x - 320.0;
        data.insert_persisted(panel_id, panel);
    });
    frame(&mut app, vec![]);
    let after_constraint = PanelState::load(&ctx, panel_id).unwrap().size().x;
    assert!(
        (after_constraint - actual).abs() < 1.0,
        "layout constraint overwrote saved width: {after_constraint} vs {actual}"
    );
}

#[test]
fn virtualized_scroll_offset_reveals_row_without_jumping_within_viewport() {
    assert_eq!(
        scroll_offset_for_selected_row(0.0, 300.0, 0, 110.0, 100.0),
        None
    );
    assert_eq!(
        scroll_offset_for_selected_row(0.0, 300.0, 1, 110.0, 100.0),
        None
    );
    assert_eq!(
        scroll_offset_for_selected_row(0.0, 300.0, 3, 110.0, 100.0),
        Some(134.0)
    );
    assert_eq!(
        scroll_offset_for_selected_row(200.0, 300.0, 3, 110.0, 100.0),
        None
    );
    assert_eq!(
        scroll_offset_for_selected_row(350.0, 300.0, 1, 110.0, 100.0),
        Some(106.0)
    );
    assert_eq!(
        scroll_offset_for_selected_row(0.0, 100.0, 99, 110.0, 100.0),
        Some(10_890.0)
    );
    assert_eq!(
        scroll_offset_for_selected_row(0.0, 0.0, 3, 110.0, 100.0),
        None
    );
}

#[test]
fn keyboard_navigation_page_boundaries_and_unselected_rows() {
    assert_eq!(
        library_navigation_target(Some(0), 350, 0, 200, Some(-4), false, false),
        Some(0)
    );
    assert_eq!(
        library_navigation_target(Some(349), 350, 200, 350, Some(4), false, false),
        Some(349)
    );
    assert_eq!(
        library_navigation_target(Some(5), 350, 0, 200, Some(12), false, false),
        Some(17)
    );
    assert_eq!(
        library_navigation_target(Some(205), 350, 0, 200, Some(1), false, false),
        Some(0)
    );
    assert_eq!(
        library_navigation_target(Some(205), 350, 0, 200, Some(-1), false, false),
        Some(199)
    );
    assert_eq!(
        library_navigation_target(None, 350, 200, 350, Some(1), false, false),
        Some(200)
    );
    assert_eq!(
        library_navigation_target(Some(250), 350, 200, 350, None, true, false),
        Some(0)
    );
    assert_eq!(
        library_navigation_target(Some(0), 350, 0, 200, None, false, true),
        Some(349)
    );
    assert_eq!(
        library_navigation_target(None, 0, 0, 0, Some(1), false, false),
        None
    );
}

#[test]
fn keyboard_navigation_keeps_selection_in_virtualized_grid_and_list() {
    fn key_frame(
        app: &mut NslApp,
        ctx: &egui::Context,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        for pressed in [true, false] {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(980.0, 690.0),
                )),
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            });
            app.ui_frame(ctx);
            let _ = ctx.end_pass();
        }
    }

    fn assert_visible(app: &mut NslApp, ctx: &egui::Context) -> usize {
        let matching = app.library_rows_cached();
        let index = matching
            .iter()
            .position(|row| app.selected == Some(row.id))
            .unwrap();
        let start = if app
            .config
            .settings
            .get("paging_on")
            .and_then(|v| v.as_str())
            == Some("0")
        {
            0
        } else {
            app.current_page * 200
        };
        assert!(index >= start, "selected item is on a different page");
        if app.view_mode == "grid" {
            let (visible, columns) = ctx.data_mut(|data| {
                (
                    data.get_temp::<std::ops::Range<usize>>(egui::Id::new(
                        "nsl_test_visible_grid_rows",
                    ))
                    .unwrap(),
                    data.get_temp::<usize>(egui::Id::new("nsl_test_grid_columns"))
                        .unwrap(),
                )
            });
            let selected_grid_row = (index - start) / columns;
            assert!(visible.contains(&selected_grid_row),
                    "grid selection {index} (row {selected_grid_row}, cols {columns}) not rendered in {visible:?}");
            let (card, viewport) = ctx.data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_selected_grid_rect"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_viewport_rect"))
                        .unwrap(),
                )
            });
            assert!(
                card.top() >= viewport.top() - 2.0 && card.bottom() <= viewport.bottom() + 2.0,
                "selected grid card {index} is clipped: card={card:?}, viewport={viewport:?}"
            );
        } else {
            let visible = ctx.data_mut(|data| {
                data.get_temp::<std::ops::Range<usize>>(egui::Id::new("nsl_test_visible_list_rows"))
                    .unwrap()
            });
            assert!(
                visible.contains(&(index - start)),
                "list selection {index} not rendered in {visible:?}"
            );
            let (cell, viewport) = ctx.data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_selected_list_rect"))
                        .unwrap(),
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_list_viewport_rect"))
                        .unwrap(),
                )
            });
            assert!(
                cell.top() >= viewport.top() - 2.0 && cell.bottom() <= viewport.bottom() + 2.0,
                "selected list row {index} is clipped: cell={cell:?}, viewport={viewport:?}"
            );
        }
        index
    }

    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    let template = app.rows.iter().find(|row| row.has_base).unwrap().clone();
    app.rows = (0..350)
        .map(|n| {
            let mut row = template.clone();
            row.id = 10_000 + n;
            row.title_name = format!("Synthetic Game {n:04}");
            row.title_id = format!("0100{n:012X}");
            row.path = format!("E:\\NSW\\synthetic_{n:04}.nsp");
            row
        })
        .collect();
    app.rows_generation = app.rows_generation.wrapping_add(1);
    app.folder_filter.clear();
    app.kind_filter = "base".into();
    app.view_mode = "grid".into();
    app.selected = None;
    app.current_page = 0;
    app.config.settings.insert("paging_on".into(), "1".into());
    app.config.settings.insert("page_size".into(), "200".into());
    assert_eq!(app.library_rows_cached().len(), 350);

    // End jumps across the page boundary. The second frame reflows the
    // grid after the detail panel opens; the selection must remain visible.
    key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 349);
    assert_eq!(app.current_page, 1);
    key_frame(&mut app, &ctx, egui::Key::Home, egui::Modifiers::CTRL);
    assert_eq!(assert_visible(&mut app, &ctx), 0);
    assert_eq!(app.current_page, 0);
    key_frame(&mut app, &ctx, egui::Key::PageDown, egui::Modifiers::NONE);
    let first_page_down = assert_visible(&mut app, &ctx);
    assert!(first_page_down > 0 && first_page_down < 100);
    for _ in 0..35 {
        key_frame(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::NONE);
        assert_visible(&mut app, &ctx);
    }
    let later = assert_visible(&mut app, &ctx);
    key_frame(&mut app, &ctx, egui::Key::PageUp, egui::Modifiers::NONE);
    assert!(assert_visible(&mut app, &ctx) < later);
    key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::CTRL);
    assert_eq!(assert_visible(&mut app, &ctx), 349);
    key_frame(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 349);
    key_frame(&mut app, &ctx, egui::Key::ArrowLeft, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 348);
    key_frame(&mut app, &ctx, egui::Key::ArrowRight, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 349);

    // A single 350-item virtual grid must work without the optional
    // 200-item paginator as well.
    app.config.settings.insert("paging_on".into(), "0".into());
    key_frame(&mut app, &ctx, egui::Key::Home, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 0);
    key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 349);

    // List mode must also reveal the last row, then PageUp and Home.
    app.view_mode = "list".into();
    key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 349);
    key_frame(&mut app, &ctx, egui::Key::PageUp, egui::Modifiers::NONE);
    assert!(assert_visible(&mut app, &ctx) < 349);
    key_frame(&mut app, &ctx, egui::Key::Home, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 0);

    // With paging disabled, End must reach the physical last item in one
    // enormous virtual list instead of stopping at item 199.
    key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
    assert_eq!(assert_visible(&mut app, &ctx), 349);
}

#[test]
fn headless_app_loads_library_and_renders_every_screen() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    assert_eq!(app.rows.len(), 2, "both records should be visible");

    // Grid, list, settings, and each section of the settings drawer must render.
    for view in ["grid", "list"] {
        app.view_mode = view.into();
        run_app_ticks(&mut app, 2);
    }
    app.settings_open = true;
    for category in 0..6 {
        app.settings_category = category;
        app.status = format!("settings category {category}");
        run_app_ticks(&mut app, 2);
    }
    app.settings_open = false;
    app.settings_category = 0;

    for tab in 0..4 {
        app.detail_tab = tab;
        run_app_ticks(&mut app, 2);
    }

    app.shortcuts_open = true;
    run_app_ticks(&mut app, 1);
    app.shortcuts_open = false;
    app.about_open = true;
    run_app_ticks(&mut app, 1);
    app.about_open = false;
    app.log_open = true;
    run_app_ticks(&mut app, 1);
}

#[test]
fn first_launch_matches_bundled_title_db_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    std::fs::create_dir_all(base.join("titledb")).unwrap();
    std::fs::write(
        base.join("titledb").join("titles.json"),
        r#"{"titledb":{"0100D2C0115B6000":{"name":"Bundled Title"}}}"#,
    )
    .unwrap();

    let mut app = NslApp::with_base_dir(base.clone()).unwrap();
    assert!(
        app.titledb_result.is_some(),
        "bundled title database must be imported on first launch"
    );
    let ctx = egui::Context::default();
    for _ in 0..600 {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        if app.titledb_result.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        app.titledb_result.is_none(),
        "title database import stalled"
    );
    assert_eq!(app.db.stats().unwrap().title_db_count, 1);

    // A second construction must not re-import an already populated table.
    let restarted = NslApp::with_base_dir(base).unwrap();
    assert!(
        restarted.titledb_result.is_none(),
        "populated title table must not be re-imported on every launch"
    );
}

#[test]
fn long_directory_tab_names_elide_at_character_boundaries_and_keep_counts() {
    let measure = |value: &str| value.chars().count() as f32 * 10.0;
    let label = "매우길고긴다운로드폴더이름 (3,137)";
    assert_eq!(elide_tab_label(label, 500.0, measure), label);
    let fitted = elide_tab_label(label, 120.0, measure);
    assert!(
        fitted.contains('…') && fitted.ends_with(" (3,137)"),
        "{fitted}"
    );
    assert!(measure(&fitted) <= 120.0, "fitted text overflows: {fitted}");
    assert_eq!(elide_tab_label("스토어", 25.0, measure), "스…");
    assert_eq!(elide_tab_label("스토어", 10.0, measure), "…");
    assert!(measure(&elide_tab_label(label, 35.0, measure)) <= 35.0);
}

#[test]
fn directory_tab_widths_shrink_only_when_needed() {
    // 5 user directories plus All, Missing Updates, Missing DLC, Store,
    // Downloads = a maximum of 10 tab buttons in the usual configuration.
    let preferred = [
        100.0, 170.0, 320.0, 220.0, 190.0, 240.0, 180.0, 150.0, 90.0, 110.0,
    ];
    let full_width: f32 = preferred.iter().sum();
    assert_eq!(
        fit_directory_tab_widths(&preferred, full_width + 10.0),
        preferred
    );
    for available in [1800.0, 1050.0, 820.0, 620.0, 490.0] {
        let fitted = fit_directory_tab_widths(&preferred, available);
        assert_eq!(fitted.len(), preferred.len());
        assert!(fitted.iter().all(|w| *w >= 44.0 && w.is_finite()));
        assert!(fitted
            .iter()
            .zip(preferred)
            .all(|(actual, original)| *actual <= original));
        assert!(
            fitted.iter().sum::<f32>() <= available + 0.05,
            "tabs overflow available width {available}: {fitted:?}"
        );
    }
    let fallback = fit_directory_tab_widths(&preferred, 240.0);
    assert_eq!(
        fallback,
        vec![44.0; preferred.len()],
        "unavoidably narrow windows should retain scrollable tab buttons"
    );
    assert!(fit_directory_tab_widths(&[], 1200.0).is_empty());
}

#[test]
fn five_long_directory_tabs_fit_screen_and_remain_clickable() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    app.selected = None;
    app.folders = (0..5)
            .map(|index| format!(
                "E:\\NSW\\This is an extremely long directory name to fit {index} — 가나다라마바사게임 목록"
            ))
            .collect();
    app.rows_generation = app.rows_generation.wrapping_add(1);
    app.cached_tabs_generation = u64::MAX;
    for width in [1456.0, 1100.0, 900.0, 760.0, 600.0] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 850.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        let (available, widths, rects) = ctx.data_mut(|data| {
            data.get_temp::<(f32, Vec<f32>, Vec<egui::Rect>)>(egui::Id::new(
                "nsl_test_directory_tab_layout",
            ))
            .unwrap()
        });
        assert_eq!(rects.len(), 10, "five folders must produce ten total tabs");
        assert_eq!(widths.len(), rects.len());
        assert!(rects.iter().all(|rect| rect.is_finite()));
        for (rect, fitted) in rects.iter().zip(widths.iter()) {
            assert!(
                (rect.width() - fitted).abs() < 0.5,
                "tab width differs from computed allocation: {rect:?} {fitted}"
            );
        }
        if available >= (10.0 * 44.0 + 10.0) {
            assert!(
                rects.last().unwrap().right() <= width + 1.0,
                "last tab hidden at window width {width}: {rects:?}"
            );
            assert!(
                rects.last().unwrap().right() - rects[0].left() <= available + 1.0,
                "tabs overflow available area {available}: {rects:?}"
            );
        }
    }

    // Clicking an elided tab must still select its full original key.
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1456.0, 850.0),
        )),
        ..Default::default()
    });
    app.ui_frame(&ctx);
    let _ = ctx.end_pass();
    let (_, _, rects) = ctx.data_mut(|data| {
        data.get_temp::<(f32, Vec<f32>, Vec<egui::Rect>)>(egui::Id::new(
            "nsl_test_directory_tab_layout",
        ))
        .unwrap()
    });
    let folder_path = app.folders[4].clone();
    let center = rects[5].center();
    for pressed in [true, false] {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 850.0),
            )),
            events: vec![
                egui::Event::PointerMoved(center),
                egui::Event::PointerButton {
                    pos: center,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
    }
    assert_eq!(
        app.folder_filter, folder_path,
        "truncated tab selected wrong directory"
    );
    assert_eq!(
        app.config
            .settings
            .get("last_tab_key")
            .and_then(|s| s.as_str()),
        Some(folder_path.as_str()),
    );
}

#[test]
fn headless_app_filters_search_and_folder_tabs() {
    let (mut app, _dir) = seeded_app();
    let ctx = run_app_ticks(&mut app, 2);
    wait_for_rows(&mut app, &ctx);
    assert_eq!(app.rows.len(), 2);

    // The default library view is base-only, matching the Python build: a
    // standalone DLC file never becomes its own card.
    app.kind_filter = "base".into();
    let base_rows = app
        .rows
        .iter()
        .filter(|row| app.row_matches_current_filter(row))
        .count();
    assert_eq!(base_rows, 1);
    assert!(app
        .rows
        .iter()
        .filter(|row| app.row_matches_current_filter(row))
        .all(|row| row.content_type == "base"));

    // Search matches title name, filename, or title id.
    app.kind_filter.clear();
    app.search = "Beta".into();
    let matches: Vec<_> = library::visible(&app.rows, &app.search)
        .filter(|row| app.row_matches_current_filter(row))
        .map(|row| row.size)
        .collect();
    assert_eq!(matches, vec![1_000_000]);

    app.search = "0100D2C0115B6000".into();
    let by_id = library::visible(&app.rows, &app.search).count();
    assert_eq!(by_id, 1, "title id search must hit exactly one file");
    app.search.clear();

    app.kind_filter = "dlc".into();
    let dlc_rows: Vec<_> = app
        .rows
        .iter()
        .filter(|row| app.row_matches_current_filter(row))
        .collect();
    assert_eq!(
        dlc_rows.len(),
        2,
        "DLC filter lists the owning base card and the DLC file itself"
    );
    assert!(dlc_rows.iter().any(|row| row.dlc_count > 0));

    // An unreachable folder tab must not panic or hide everything.
    app.folder_filter = "MISSING_UPDATES".into();
    run_app_ticks(&mut app, 1);
    app.folder_filter = "Z:\\missing".into();
    run_app_ticks(&mut app, 1);
    app.folder_filter.clear();
    app.kind_filter = "base".into();
    run_app_ticks(&mut app, 1);
    assert_eq!(app.rows.len(), 2);
}

/// Full scan of the real local corpus into a scratch database. Ignored by
/// default because it needs E:\NSW and prod.keys present.
#[test]
#[ignore = "requires the local game corpus and prod.keys"]
fn full_scan_of_real_corpus_matches_existing_library() {
    let (mut app, dir) = seeded_app();
    // Profile the bounded worker count without touching persistent user
    // settings. Set NSL_TEST_SCAN_THREADS only for this ignored QA test.
    if let Ok(threads) = std::env::var("NSL_TEST_SCAN_THREADS") {
        app.config.settings.insert(
            "max_scan_threads".into(),
            serde_json::Value::String(threads),
        );
    }
    let base = dir.path().to_path_buf();
    app.folders = vec!["E:\\NSW".into()];
    app.config.scan_folders = app.folders.clone();
    database::save_config(&base.join("config.json"), &app.config).unwrap();
    app.db.clear_library_data().unwrap();

    app.start_scan(false);
    let ctx = egui::Context::default();
    let mut finished = false;
    for _ in 0..120_000 {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        if app.scan_result.is_none() && app.reload_result.is_none() && app.scan_progress.is_none() {
            finished = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(finished, "scan did not finish: {}", app.status);

    let count = app.rows.len();
    let stats = app.db.stats().unwrap();
    let failures = app
        .log_lines
        .iter()
        .filter(|line| line.contains("분석 실패"))
        .count();
    assert_eq!(
        failures, 0,
        "no file may fail analysis; status: {}",
        app.status
    );
    assert!(
        count > 700,
        "expected the full library, scanned only {count} rows"
    );
    let reference_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("library.db");
    let reference = rusqlite::Connection::open_with_flags(
        reference_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let reference_counts: (i64, i64, i64) = reference
            .query_row(
                "SELECT \
                 (SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1 AND fc.content_type='base'), \
                 (SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1 AND fc.content_type='update'), \
                 (SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1 AND fc.content_type='dlc')",
                [app.folders[0].as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
    assert_eq!(
        (stats.base_count, stats.update_count, stats.dlc_count),
        reference_counts,
        "scan content totals must match the current reference library"
    );

    // Every discovered file must carry a fast hash so the next pass can reuse it.
    let cached = app.db.cached_file_records().unwrap();
    assert_eq!(cached.len(), count);
    assert!(cached.values().all(|record| !record.fast_hash.is_empty()));
    assert!(cached.values().all(|record| !record.contents.is_empty()));
    // Compare the same scanned root against its read-only existing rows;
    // the complete library may include unrelated download/desktop roots.
    let mut file_stmt = reference.prepare(
            "SELECT path,COALESCE(nacp_ko,''),COALESCE(nacp_en,''),COALESCE(nacp_ja,'') FROM game_files WHERE scan_folder=?1"
        ).unwrap();
    let mut compared_files = 0usize;
    let mut reference_name_differences = 0usize;
    for row in file_stmt
        .query_map([app.folders[0].as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
    {
        let (path, ko, en, ja) = row.unwrap();
        let normalized = normalize_scan_path_text(&path);
        let new = cached
            .get(&normalized)
            .unwrap_or_else(|| panic!("missing reference path"));
        if (
            new.nacp_ko.as_str(),
            new.nacp_en.as_str(),
            new.nacp_ja.as_str(),
        ) != (ko.as_str(), en.as_str(), ja.as_str())
        {
            reference_name_differences += 1;
        }
        compared_files += 1;
    }
    let reference_files: i64 = reference
        .query_row(
            "SELECT COUNT(*) FROM game_files WHERE scan_folder=?1",
            [app.folders[0].as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(compared_files as i64, reference_files);
    let mut content_stmt = reference
            .prepare("SELECT fc.path,fc.title_id,fc.version,fc.content_type FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1")
            .unwrap();
    let mut reference_contents: HashMap<String, HashSet<(Option<String>, i64, String)>> =
        HashMap::new();
    for row in content_stmt
        .query_map([app.folders[0].as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
    {
        let (path, id, version, kind) = row.unwrap();
        reference_contents
            .entry(normalize_scan_path_text(&path))
            .or_default()
            .insert((id, version, kind));
    }
    let mut compared_contents = 0usize;
    let mut reference_content_differences = 0usize;
    for (path, expected) in &reference_contents {
        let actual: HashSet<_> = cached
            .get(path)
            .unwrap()
            .contents
            .iter()
            .map(|entry| {
                (
                    entry.title_id.clone(),
                    entry.version,
                    entry.content_type.clone(),
                )
            })
            .collect();
        if &actual != expected {
            reference_content_differences += 1;
        }
        compared_contents += actual.len();
    }
    let reference_content_count: i64 = reference.query_row(
            "SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1",
            [app.folders[0].as_str()], |row| row.get(0)
        ).unwrap();
    assert_eq!(compared_contents as i64, reference_content_count);
    eprintln!("SCAN_REFERENCE_COMPARISON files={compared_files} content_entries={compared_contents} old_names_different={reference_name_differences} old_contents_different={reference_content_differences}");
    let localized = cached
        .values()
        .filter(|record| {
            !record.nacp_ko.is_empty() || !record.nacp_en.is_empty() || !record.nacp_ja.is_empty()
        })
        .count();
    let korean = cached
        .values()
        .filter(|record| !record.nacp_ko.is_empty())
        .count();
    assert!(localized >= 800, "NACP localization was lost: {localized}");
    assert!(korean >= 530, "Korean names were lost: {korean}");
    eprintln!("SCAN_LOCALES available={localized} korean={korean}");
    eprintln!("SCAN_PROFILE {}", scanner::scan_profile::report());
}

/// Incremental pass must reuse the cache written by the full pass, keep the
/// same totals, and stay much faster than a cold scan.
#[test]
#[ignore = "requires the local game corpus and prod.keys"]
fn incremental_scan_reuses_cache_after_full_scan() {
    let (mut app, dir) = seeded_app();
    let base = dir.path().to_path_buf();
    app.folders = vec!["E:\\NSW".into()];
    app.config.scan_folders = app.folders.clone();
    database::save_config(&base.join("config.json"), &app.config).unwrap();
    app.db.clear_library_data().unwrap();
    let ctx = egui::Context::default();

    let full_start = std::time::Instant::now();
    app.start_scan(false);
    assert!(run_until_idle(&mut app, &ctx), "full scan timed out");
    let full_elapsed = full_start.elapsed();
    let full_stats = app.db.stats().unwrap();

    let incremental_start = std::time::Instant::now();
    app.start_scan(true);
    assert!(run_until_idle(&mut app, &ctx), "incremental scan timed out");
    let incremental_elapsed = incremental_start.elapsed();
    let incremental_stats = app.db.stats().unwrap();

    assert!(full_stats.base_count > 0);
    assert_eq!(incremental_stats.base_count, full_stats.base_count);
    assert_eq!(incremental_stats.update_count, full_stats.update_count);
    assert_eq!(incremental_stats.dlc_count, full_stats.dlc_count);
    assert_eq!(
        incremental_stats.file_count, full_stats.file_count,
        "incremental pass must not drop records"
    );
    assert!(
            incremental_elapsed < full_elapsed,
            "incremental scan ({incremental_elapsed:?}) should be faster than the cold scan ({full_elapsed:?})"
        );
    eprintln!(
        "full={full_elapsed:?} incremental={incremental_elapsed:?} files={}",
        incremental_stats.file_count
    );
}

fn run_until_idle(app: &mut NslApp, ctx: &egui::Context) -> bool {
    for _ in 0..120_000 {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        });
        app.ui_frame(ctx);
        let _ = ctx.end_pass();
        if app.scan_result.is_none() && app.reload_result.is_none() && app.scan_progress.is_none() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}
