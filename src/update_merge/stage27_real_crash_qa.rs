//! Stage 27 opt-in actual real-DLC NSZ crash test.
//! Every child and source path is confined to a unique QA directory.
//! An OS kill targets only the process spawned and owned by this test.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const BASE: &str = "0100800015926000";
const DLC: &str = "010080001592701D";
const EXPECTED_GAME_SHA: &str = "23B3F50B2D2969A6A52FD646FB1FC7C33CFE9BDF222C88ACC8AE2EE9B49D8188";
const EXPECTED_DLC_SHA: &str = "040232A33243311C6DF1017E8E1256E357D3185299B3834CA3AF51E22BF6107E";

fn sha256(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = [0u8; 1024 * 1024];
    loop {
        let count = source.read(&mut block)?;
        if count == 0 {
            break;
        }
        hash.update(&block[..count]);
    }
    Ok(format!("{:X}", hash.finalize()))
}

fn real_fixture() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let root = fs::canonicalize(std::env::var("NSL_STAGE13_SOURCE_ROOT")?)?;
    let source = fs::canonicalize(std::env::var("NSL_STAGE13_BASE")?)?;
    let dlcs = fs::canonicalize(std::env::var("NSL_STAGE13_DLC_DIR")?)?;
    let qa = fs::canonicalize(std::env::var("NSL_STAGE13_QA_ROOT")?)?;
    anyhow::ensure!(source.starts_with(&root) && dlcs.starts_with(&root));
    anyhow::ensure!(!qa.starts_with(&root) && !root.starts_with(&qa));
    anyhow::ensure!(source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")));
    let candidates = fs::read_dir(&dlcs)?
        .filter_map(|entry| entry.ok().map(|item| item.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(&format!("[{DLC}]")) && name.ends_with(".nsp"))
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        candidates.len() == 1,
        "missing or ambiguous Stage27 real DLC"
    );
    anyhow::ensure!(fs2::available_space(&qa)? >= 16 * (1u64 << 30));
    anyhow::ensure!(fs2::available_space(std::env::temp_dir())? >= 16 * (1u64 << 30));
    Ok((source, candidates[0].canonicalize()?, qa))
}

/// Executed only by the parent-owned child test. Normal cargo test does not
/// set the private root environment variable, so this has no side effects.
#[test]
#[ignore = "child-only Stage27 real NSZ writer; spawned by parent test"]
fn stage27_private_real_dlc_merge_child() -> Result<()> {
    let Some(root) = std::env::var_os("NSL_STAGE27_CHILD_ROOT") else {
        return Ok(());
    };
    let root = fs::canonicalize(root)?;
    anyhow::ensure!(
        root.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("stage27-merge-crash-")),
        "refusing non-Stage27 QA path"
    );
    let original = fs::canonicalize(root.join("source-game.nsz"))?;
    let dlc = fs::canonicalize(root.join("private-app-state/downloads/dlc/patch.nsp"))?;
    anyhow::ensure!(original.parent() == Some(root.as_path()));
    anyhow::ensure!(dlc.starts_with(root.join("private-app-state/downloads")));
    let output =
        merge_or_recover_dlc_controlled(&original, &dlc, BASE, DLC, 0, &MergeControl::default())?;
    println!("STAGE27_CHILD_FINISHED_WITHOUT_CRASH={}", output.display());
    Ok(())
}

/// Stage28 child-only: runs a real merge but is held by the test-only
/// production publication hook after the final file exists and before
/// TempDir cleanup or the durable batch journal's Merged transition.
#[test]
#[ignore = "child-only opt-in Stage28 real NSZ publication crash QA"]
fn stage28_private_dlc_after_publish_child() -> Result<()> {
    let Some(root) = std::env::var_os("NSL_STAGE28_CHILD_ROOT") else {
        return Ok(());
    };
    let root = fs::canonicalize(root)?;
    anyhow::ensure!(
        root.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("stage28-published-crash-")),
        "Stage28 child was pointed outside test-owned QA directory"
    );
    let game = fs::canonicalize(root.join("source-game.nsz"))?;
    let dlc = fs::canonicalize(root.join("private-app-state/downloads/dlc/patch.nsp"))?;
    anyhow::ensure!(game.parent() == Some(root.as_path()));
    anyhow::ensure!(dlc.starts_with(root.join("private-app-state/downloads")));
    let result =
        merge_or_recover_dlc_controlled(&game, &dlc, BASE, DLC, 0, &MergeControl::default())?;
    anyhow::bail!(
        "Stage28 child must not return before owned kill: {}",
        result.display()
    );
}

/// Real 1.37GiB completed DLC NSZ, killed in the previously-unverified
/// *post-publication, pre-journal* window. The verified output must be
/// adopted without a second write or an orphaned 1.37GiB workspace.
#[test]
#[ignore = "real read-only DLC + private 16GiB scratch + owned child OS kill"]
fn stage28_real_dlc_postpublish_prejournal_crash_recovers_without_rewriting() -> Result<()> {
    use crate::update_batch::{
        BatchKind, UpdateBatch, UpdateBatchJob, UpdateBatchJobStatus, UpdateBatchRow,
    };

    let (source, source_dlc, qa) = real_fixture()?;
    let started = Instant::now();
    let source_hash = sha256(&source)?;
    let dlc_hash = sha256(&source_dlc)?;
    anyhow::ensure!(source_hash == EXPECTED_GAME_SHA && dlc_hash == EXPECTED_DLC_SHA);
    let scratch = tempfile::Builder::new()
        .prefix("stage28-published-crash-")
        .tempdir_in(qa)?;
    let game = scratch.path().join("source-game.nsz");
    fs::copy(&source, &game)?;
    anyhow::ensure!(sha256(&game)? == source_hash);
    let app_root = scratch.path().join("private-app-state");
    let download_dir = app_root.join("downloads/dlc");
    fs::create_dir_all(&download_dir)?;
    let downloaded = download_dir.join("patch.nsp");
    fs::copy(&source_dlc, &downloaded)?;
    anyhow::ensure!(sha256(&downloaded)? == dlc_hash);
    crate::update_batch::verify_downloaded_dlc(&app_root, &downloaded, BASE, DLC, 0)?;
    let job = UpdateBatchJob {
        key: "stage28-private-published-job".into(),
        directory: download_dir,
        output: downloaded.clone(),
        expected_version: 0,
        expected_file_size: fs::metadata(&downloaded)?.len(),
        original_size: fs::metadata(&game)?.len(),
        transfer_finished: true,
        recovering_since: None,
        status: UpdateBatchJobStatus::Verifying,
        verifying: None,
        merge_control: None,
    };
    let batch = UpdateBatch {
        kind: BatchKind::Dlc,
        rows: vec![UpdateBatchRow {
            library_id: 28,
            merge_permitted: true,
            title_id: BASE.into(),
            dlc_id: Some(DLC.into()),
            name: "Stage28 already-published DLC crash recovery".into(),
            installed_version: -1,
            latest_version: 0,
            original_path: game.clone(),
            original_type: "nsz".into(),
            candidates: Vec::new(),
            next_candidate: 0,
            found: None,
            checked: false,
            failure: None,
            job: Some(job),
        }],
        running: false,
        cursor: 1,
        active: None,
        confirm_queue: false,
        preview_cache: Default::default(),
        preview_cache_bytes: 0,
        pending_catalog_hints: Default::default(),
    };
    crate::update_manifest::save(&app_root, &batch)?;
    let destination = expected_dlc_output(&game, BASE, DLC, 0)?;
    anyhow::ensure!(!destination.exists());
    let child_log = File::create(scratch.path().join("stage28-child-stderr.log"))?;
    let child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "update_merge::stage27_real_crash_qa::stage28_private_dlc_after_publish_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("NSL_STAGE28_CHILD_ROOT", scratch.path())
        .env("NSL_STAGE28_AFTER_PUBLISH_CHILD_ROOT", scratch.path())
        .stdout(Stdio::null())
        .stderr(Stdio::from(child_log))
        .spawn()?;
    let mut owned = OwnedChild(child);
    let signal = scratch.path().join("stage28-published-and-locked.signal");
    let waited = Instant::now();
    while waited.elapsed() < Duration::from_secs(260) {
        if owned.0.try_wait()?.is_some() {
            let message = fs::read_to_string(scratch.path().join("stage28-child-stderr.log"))
                .unwrap_or_default();
            anyhow::bail!("Stage28 child exited before publication hold: {message}");
        }
        if signal.is_file() && destination.is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::ensure!(
        signal.is_file() && destination.is_file(),
        "never observed a real published DLC output before killing owned child"
    );
    anyhow::ensure!(
        owned.0.try_wait()?.is_none(),
        "owned child died before controlled OS kill"
    );
    let published_bytes = fs::metadata(&destination)?.len();
    anyhow::ensure!(published_bytes > 1024 * 1024 * 1024);
    let mut owned_workspace: Option<PathBuf> = None;
    for entry in fs::read_dir(scratch.path())? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".nsl-merge-")
            && entry.file_type()?.is_dir()
            && entry.path().join(MERGE_WORK_MARKER).is_file()
        {
            anyhow::ensure!(
                owned_workspace.is_none(),
                "ambiguous owned merge workspaces"
            );
            owned_workspace = Some(entry.path());
        }
    }
    let orphan = owned_workspace.context("published output has no locked owner workspace")?;
    // Terminate only the test-owned child, not any unrelated NSL or emulator.
    owned.0.kill()?;
    let _ = owned.0.wait()?;
    anyhow::ensure!(destination.is_file() && orphan.is_dir());
    anyhow::ensure!(fs::read(&signal)? == b"published-qa-only");
    let before_recovery = sha256(&destination)?;
    anyhow::ensure!(sha256(&game)? == source_hash && sha256(&downloaded)? == dlc_hash);
    println!(
        "STAGE28_REAL_AFTER_PUBLISH_CHILD_KILLED bytes={} published=true journal_stale=true owner_orphan=true",
        published_bytes
    );

    drop(batch);
    let mut restored = crate::update_manifest::load(&app_root)?
        .context("published-job journal missing after Stage28 child exit")?;
    anyhow::ensure!(restored.rows[0].job.as_ref().is_some_and(|job| {
        job.transfer_finished && job.status == UpdateBatchJobStatus::Queued
    }));
    let recovery_started = Instant::now();
    let recovered = merge_or_recover_dlc_controlled(
        &game,
        &downloaded,
        BASE,
        DLC,
        0,
        &MergeControl::default(),
    )?;
    anyhow::ensure!(recovered == destination && recovered.is_file());
    anyhow::ensure!(
        sha256(&destination)? == before_recovery,
        "recovered file was rewritten or modified rather than adopted"
    );
    anyhow::ensure!(
        !orphan.exists(),
        "post-publication orphan was not reclaimed"
    );
    // Exactly the same verified output was adopted; mark the production
    // journal as Merged and verify that the durable path survives restart.
    restored.rows[0].job.as_mut().unwrap().status = UpdateBatchJobStatus::Merged(recovered.clone());
    crate::update_manifest::save(&app_root, &restored)?;
    let final_batch =
        crate::update_manifest::load(&app_root)?.context("merged job lost after durable save")?;
    anyhow::ensure!(matches!(
        final_batch.rows[0].job.as_ref().map(|job| &job.status),
        Some(UpdateBatchJobStatus::Merged(path)) if path == &destination
    ));
    let keys = scanner::prod_keys().context("Stage28 read-only prod.keys unavailable")?;
    let meta_root = scratch.path().join("stage28-final-cnmt-qa");
    let disabled_tool = meta_root.join("no-external-tool.exe");
    let (_, original_meta, _) = cnmt::discover_game_metadata(
        &game,
        &meta_root.join("base"),
        &disabled_tool,
        &keys,
        &AtomicBool::new(false),
    )?;
    let (_, recovered_meta, _) = cnmt::discover_game_metadata(
        &destination,
        &meta_root.join("final"),
        &disabled_tool,
        &keys,
        &AtomicBool::new(false),
    )?;
    validate_dlc_cnmts(&recovered_meta, BASE, DLC, 0)?;
    ensure_existing_content_preserved(&original_meta, &recovered_meta)?;
    anyhow::ensure!(fs::metadata(&destination)?.len() == published_bytes);
    anyhow::ensure!(sha256(&source)? == source_hash && sha256(&source_dlc)? == dlc_hash);
    anyhow::ensure!(sha256(&game)? == source_hash && sha256(&downloaded)? == dlc_hash);
    println!(
        "STAGE28_REAL_POSTPUBLISH_PREJOURNAL_RESTORE_PASS bytes={} prior_cnmts={} final_cnmts={} original_hashes=true reused_sha=true orphan_reaped=true durable_merged=true restore_seconds={:.2} total_seconds={:.2}",
        published_bytes, original_meta.len(), recovered_meta.len(),
        recovery_started.elapsed().as_secs_f64(), started.elapsed().as_secs_f64()
    );
    Ok(())
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Kill an owned child only after the actual PFS0 output stream has written
/// more than 32 MiB; then verify real durable job recovery and native CNMT.
#[test]
#[ignore = "real signed DLC QA: kill only owned child, private 16GiB scratch"]
fn stage27_real_dlc_mid_write_owned_child_crash_and_retry() -> Result<()> {
    use crate::update_batch::{
        BatchKind, UpdateBatch, UpdateBatchJob, UpdateBatchJobStatus, UpdateBatchRow,
    };

    let (source, source_dlc, qa) = real_fixture()?;
    let started = Instant::now();
    let game_sha = sha256(&source)?;
    let dlc_sha = sha256(&source_dlc)?;
    anyhow::ensure!(game_sha == EXPECTED_GAME_SHA && dlc_sha == EXPECTED_DLC_SHA);
    let scratch = tempfile::Builder::new()
        .prefix("stage27-merge-crash-")
        .tempdir_in(qa)?;
    let game = scratch.path().join("source-game.nsz");
    fs::copy(&source, &game)?;
    anyhow::ensure!(sha256(&game)? == game_sha);
    let app_root = scratch.path().join("private-app-state");
    let download_dir = app_root.join("downloads/dlc");
    fs::create_dir_all(&download_dir)?;
    let dlc_copy = download_dir.join("patch.nsp");
    fs::copy(&source_dlc, &dlc_copy)?;
    anyhow::ensure!(sha256(&dlc_copy)? == dlc_sha);
    crate::update_batch::verify_downloaded_dlc(&app_root, &dlc_copy, BASE, DLC, 0)?;

    let row = UpdateBatchRow {
        library_id: 27,
        merge_permitted: true,
        title_id: BASE.into(),
        dlc_id: Some(DLC.into()),
        name: "Stage27 real DLC crash".into(),
        installed_version: -1,
        latest_version: 0,
        original_path: game.clone(),
        original_type: "nsz".into(),
        candidates: Vec::new(),
        next_candidate: 0,
        found: None,
        checked: false,
        failure: None,
        job: Some(UpdateBatchJob {
            key: "stage27-private-transfer".into(),
            directory: download_dir,
            output: dlc_copy.clone(),
            expected_version: 0,
            expected_file_size: fs::metadata(&dlc_copy)?.len(),
            original_size: fs::metadata(&game)?.len(),
            transfer_finished: true,
            recovering_since: None,
            status: UpdateBatchJobStatus::Verifying,
            verifying: None,
            merge_control: None,
        }),
    };
    let batch = UpdateBatch {
        kind: BatchKind::Dlc,
        rows: vec![row],
        running: false,
        cursor: 1,
        active: None,
        confirm_queue: false,
        preview_cache: Default::default(),
        preview_cache_bytes: 0,
        pending_catalog_hints: Default::default(),
    };
    crate::update_manifest::save(&app_root, &batch)?;
    let destination = expected_dlc_output(&game, BASE, DLC, 0)?;
    anyhow::ensure!(!destination.exists());

    let child_log = File::create(scratch.path().join("stage27-child-stderr.log"))?;
    let child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "update_merge::stage27_real_crash_qa::stage27_private_real_dlc_merge_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("NSL_STAGE27_CHILD_ROOT", scratch.path())
        .stdout(Stdio::null())
        .stderr(Stdio::from(child_log))
        .spawn()?;
    let mut owned = OwnedChild(child);
    let observed_start = Instant::now();
    let mut orphan: Option<PathBuf> = None;
    let mut partial_bytes = 0u64;
    while observed_start.elapsed() < Duration::from_secs(110) {
        if owned.0.try_wait()?.is_some() {
            let message = fs::read_to_string(scratch.path().join("stage27-child-stderr.log"))
                .unwrap_or_default();
            anyhow::bail!("isolated child exited before mid-write observation: {message}");
        }
        for workspace in fs::read_dir(scratch.path())? {
            let workspace = workspace?;
            if !workspace
                .file_name()
                .to_string_lossy()
                .starts_with(".nsl-merge-")
                || !workspace.file_type()?.is_dir()
            {
                continue;
            }
            for file in fs::read_dir(workspace.path())? {
                let file = file?;
                let name = file.file_name();
                if name.to_string_lossy().starts_with(".nsrp-pfs0-")
                    && name.to_string_lossy().ends_with(".partial")
                {
                    let size = file.metadata()?.len();
                    if size >= 32 * 1024 * 1024 {
                        partial_bytes = size;
                        orphan = Some(workspace.path());
                        break;
                    }
                }
            }
            if orphan.is_some() {
                break;
            }
        }
        if orphan.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let orphan = orphan.context("child did not write a genuine 32MiB .partial NSZ")?;
    owned.0.kill()?;
    let _ = owned.0.wait()?;
    anyhow::ensure!(orphan.join(MERGE_WORK_MARKER).is_file());
    anyhow::ensure!(
        !destination.exists(),
        "incomplete merged package published too early"
    );
    anyhow::ensure!(sha256(&game)? == game_sha && sha256(&dlc_copy)? == dlc_sha);
    println!(
        "STAGE27_REAL_CHILD_KILLED_DURING_WRITE partial_bytes={partial_bytes} no_published_output=true"
    );

    drop(batch);
    let restored = crate::update_manifest::load(&app_root)?
        .context("durable verification journal was lost after killing owned child")?;
    anyhow::ensure!(
        restored.rows[0].job.as_ref().is_some_and(|job| {
            job.transfer_finished && job.status == UpdateBatchJobStatus::Queued
        }),
        "crashed Verifying job was not restored to Queued"
    );

    // A real retry must reap the lock-released orphan before the disk-space
    // preflight and repeat native CNMT validation and safe publication.
    let recovered =
        merge_or_recover_dlc_controlled(&game, &dlc_copy, BASE, DLC, 0, &MergeControl::default())?;
    anyhow::ensure!(recovered == destination && recovered.is_file());
    anyhow::ensure!(!orphan.exists(), "dead multi-GB merge workspace not reaped");
    let keys = scanner::prod_keys().context("private read-only keys are missing")?;
    let work = scratch.path().join("verified-meta");
    let (_, original_meta, _) = cnmt::discover_game_metadata(
        &game,
        &work.join("original"),
        &work.join("_external_tool_disabled.exe"),
        &keys,
        &AtomicBool::new(false),
    )?;
    let (_, final_meta, _) = cnmt::discover_game_metadata(
        &recovered,
        &work.join("final"),
        &work.join("_external_tool_disabled.exe"),
        &keys,
        &AtomicBool::new(false),
    )?;
    validate_dlc_cnmts(&final_meta, BASE, DLC, 0)?;
    ensure_existing_content_preserved(&original_meta, &final_meta)?;
    let index = nsrp_core::formats::pfs0::Pfs0Index::parse(&recovered)?;
    anyhow::ensure!(index.entries.iter().any(|item| item.name.ends_with(".ncz")));
    anyhow::ensure!(sha256(&source)? == game_sha && sha256(&source_dlc)? == dlc_sha);
    anyhow::ensure!(sha256(&game)? == game_sha && sha256(&dlc_copy)? == dlc_sha);
    println!(
        "STAGE27_REAL_MID_WRITE_CRASH_RETRY_PASS partial_bytes={} recovered_bytes={} prior_cnmts={} final_cnmts={} journal_queued=true orphan_reaped=true original_sha_unchanged=true elapsed_s={:.2}",
        partial_bytes, fs::metadata(&recovered)?.len(), original_meta.len(),
        final_meta.len(), started.elapsed().as_secs_f64()
    );
    Ok(())
}
