//! Stage 13 opt-in actual DLC QA. The user's files are always read-only;
//! merged outputs are created only inside an auto-cleaned, isolated directory.
use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::time::Instant;

type DlcCandidate = (String, u32, PathBuf);
type Stage13Inventory = (String, Vec<CnmtInfo>, Vec<DlcCandidate>);

fn fixture() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let root = fs::canonicalize(std::env::var("NSL_STAGE13_SOURCE_ROOT")?)?;
    let source = fs::canonicalize(std::env::var("NSL_STAGE13_BASE")?)?;
    let dlc_dir = fs::canonicalize(std::env::var("NSL_STAGE13_DLC_DIR")?)?;
    let qa = fs::canonicalize(std::env::var("NSL_STAGE13_QA_ROOT")?)?;
    anyhow::ensure!(
        source.starts_with(&root) && dlc_dir.starts_with(&root) && !qa.starts_with(&root),
        "Stage 13 source and QA must be separate directories"
    );
    anyhow::ensure!(
        source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")),
        "Stage 13 source must be an NSZ"
    );
    anyhow::ensure!(source.is_file() && dlc_dir.is_dir() && qa.is_dir());
    anyhow::ensure!(fs::metadata(&source)?.len() < 2 * (1u64 << 30));
    Ok((source, dlc_dir, qa))
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let len = input.read(&mut buf)?;
        if len == 0 {
            break;
        }
        hasher.update(&buf[..len]);
    }
    Ok(hasher.finalize().into())
}

fn discovered(path: &Path, dir: &Path, keys: &Path) -> Result<Vec<CnmtInfo>> {
    let entries = nsrp_core::ops::container::list_entries(path, true)?;
    let crypto_keys = KeySet::load_with_local_title_keys(keys)?;
    let mut successes = 0usize;
    let mut failures = 0usize;
    for entry in entries.iter().filter(|e| e.name.ends_with(".cnmt.nca")) {
        let outcome = nsrp_core::ops::native_meta::read_cnmt_from_nca_region(
            path,
            entry.offset,
            entry.size,
            &crypto_keys,
        )
        .and_then(|raw| cnmt::parse_cnmt(&raw, entry.name.clone()));
        match outcome {
            Ok(_) => successes += 1,
            Err(error) => {
                failures += 1;
                if failures <= 4 {
                    println!(
                        "STAGE13_NATIVE_CNMT_FAILED name={} offset={} bytes={} reason={}",
                        entry.name, entry.offset, entry.size, error
                    );
                    let mut f = File::open(path)?;
                    f.seek(SeekFrom::Start(entry.offset))?;
                    let mut header = [0u8; nsrp_core::formats::nca::NCA_XTS_HEADER_SIZE];
                    f.read_exact(&mut header)?;
                    match nsrp_core::formats::nca::decrypt_header(&header, &crypto_keys) {
                        Ok(info) => {
                            println!(
                                "STAGE13_NCA_LAYOUT content_type={} generation={} rights={} sections={:?}",
                                info.content_type,
                                info.key_generation,
                                info.rights_id.iter().any(|b| *b != 0),
                                info.sections.iter().map(|sec| (sec.offset, sec.size, sec.encryption_type)).collect::<Vec<_>>()
                            );
                        }
                        Err(error) => println!("STAGE13_NCA_LAYOUT_DECRYPT_FAILURE {}", error),
                    }
                }
            }
        }
    }
    println!(
        "STAGE13_NATIVE_CNMT_CHECK ok={successes} failed={failures} container_entries={}",
        entries.len()
    );
    let (_entries, cnmts, _legacy) = cnmt::discover_game_metadata(
        path,
        dir,
        &dir.join("_external_tool_disabled.exe"),
        keys,
        &AtomicBool::new(false),
    )?;
    Ok(cnmts)
}

fn choose_missing(
    source: &Path,
    dlc_dir: &Path,
    qa: &Path,
    keys: &Path,
) -> Result<Stage13Inventory> {
    let previous = discovered(source, &qa.join("original_meta"), keys)?;
    let base = previous
        .iter()
        .find(|c| c.meta_type == ContentMetaType::Base)
        .context("Source NSZ has no Base CNMT")?
        .title_id
        .clone();
    let mut available = Vec::new();
    let mut candidates = fs::read_dir(dlc_dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nsp"))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    let title_id_pattern = regex::Regex::new(r"(?i)\[([0-9a-f]{16})\]")?;
    for path in candidates {
        if fs::metadata(&path)?.len() > 16 * 1024 * 1024 {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let Some(id) = title_id_pattern
            .captures(name)
            .map(|match_| match_[1].to_ascii_uppercase())
        else {
            continue;
        };
        let Ok(title) = u64::from_str_radix(&id, 16) else {
            continue;
        };
        let Ok(base_number) = u64::from_str_radix(&base, 16) else {
            continue;
        };
        if title & !0x1fff != base_number || title & 0x1fff < 0x1000 {
            continue;
        }
        if previous.iter().any(|cnmt| {
            cnmt.meta_type == ContentMetaType::Dlc && cnmt.title_id.eq_ignore_ascii_case(&id)
        }) {
            continue;
        }
        let metadata = discovered(
            &path,
            &qa.join(format!("dlc_meta_{}", available.len())),
            keys,
        )?;
        if let Some(cnmt) = metadata
            .iter()
            .find(|c| c.meta_type == ContentMetaType::Dlc && c.title_id.eq_ignore_ascii_case(&id))
        {
            available.push((id, cnmt.version, path));
            if available.len() == 2 {
                break;
            }
        }
    }
    println!(
        "STAGE13_SOURCE_INVENTORY base={} base_cnmts={} updates={} prior_dlcs={} new_candidates={}",
        base,
        previous.len(),
        previous
            .iter()
            .filter(|c| c.meta_type == ContentMetaType::Update)
            .count(),
        previous
            .iter()
            .filter(|c| c.meta_type == ContentMetaType::Dlc)
            .count(),
        available.len()
    );
    for (id, version, path) in &available {
        println!(
            "STAGE13_MISSING_DLC id={} version={} bytes={}",
            id,
            version,
            fs::metadata(path)?.len()
        );
    }
    Ok((base, previous, available))
}

#[test]
#[ignore = "requires Stage 13 read-only NSZ + DLC-dir, separate QA-root and prod.keys"]
fn isolated_stage13_real_missing_dlc_inventory() -> Result<()> {
    let (source, dlcs, root) = fixture()?;
    let private = tempfile::Builder::new()
        .prefix("stage13-inventory-")
        .tempdir_in(root)?;
    let keys = scanner::prod_keys().context("read-only prod.keys missing")?;
    let (base, _prior, missing) = choose_missing(&source, &dlcs, private.path(), &keys)?;
    anyhow::ensure!(
        !base.is_empty() && !missing.is_empty(),
        "No eligible missing DLC candidates"
    );
    println!("STAGE13_REAL_DLC_INVENTORY_PASS");
    Ok(())
}

/// Examine the NCZ section descriptors without decoding game payload data.
#[test]
#[ignore = "requires NSL_STAGE13_BASE and Stage 13 read-only source/QA roots"]
fn isolated_stage13_real_ncz_section_inventory() -> Result<()> {
    use nsrp_core::formats::{ncz_stream, pfs0::Pfs0Index};
    let (source, _dlcs, _root) = fixture()?;
    let index = Pfs0Index::parse(&source)?;
    let mut file = File::open(&source)?;
    let mut valid = 0usize;
    let mut invalid = 0usize;
    let mut decoded_small = 0usize;
    for entry in index
        .entries
        .iter()
        .filter(|entry| entry.name.to_ascii_lowercase().ends_with(".ncz"))
    {
        let position = index
            .data_start
            .checked_add(entry.offset)
            .context("NCZ offset overflow")?;
        match ncz_stream::validate_ncz_region_layout(&mut file, position, entry.size) {
            Ok(nca_bytes) => {
                valid += 1;
                if entry.size <= 1024 * 1024 && nca_bytes <= 2 * 1024 * 1024 {
                    let mut recovered = Vec::new();
                    let reported = ncz_stream::decompress_ncz_region_to_nca(
                        &mut file,
                        position,
                        entry.size,
                        &mut recovered,
                        &AtomicBool::new(false),
                        |_, _| {},
                    )?;
                    anyhow::ensure!(reported == nca_bytes && recovered.len() as u64 == nca_bytes);
                    file.seek(SeekFrom::Start(position))?;
                    let mut original_header = vec![0u8; 0x4000];
                    file.read_exact(&mut original_header)?;
                    anyhow::ensure!(recovered.starts_with(&original_header));
                    decoded_small += 1;
                }
            }
            Err(error) => {
                invalid += 1;
                println!(
                    "STAGE13_INVALID_NCZ name={} bytes={} reason={error}",
                    entry.name, entry.size
                );
                file.seek(SeekFrom::Start(position + 0x4000))?;
                let mut head = [0u8; 16];
                file.read_exact(&mut head)?;
                if &head[..8] != b"NCZSECTN" {
                    continue;
                }
                let count = i64::from_le_bytes(head[8..16].try_into().unwrap());
                if !(1..=64).contains(&count) {
                    continue;
                }
                let mut spans = Vec::new();
                for _ in 0..count {
                    let mut row = [0u8; 64];
                    file.read_exact(&mut row)?;
                    spans.push((
                        i64::from_le_bytes(row[0..8].try_into().unwrap()),
                        i64::from_le_bytes(row[8..16].try_into().unwrap()),
                        i64::from_le_bytes(row[16..24].try_into().unwrap()),
                    ));
                }
                println!(
                    "STAGE13_INVALID_NCZ_SECTIONS name={} sections={spans:?}",
                    entry.name
                );
            }
        }
    }
    println!(
        "STAGE13_NCZ_LAYOUT_INVENTORY valid={} invalid={} small_decode_pass={}",
        valid, invalid, decoded_small
    );
    anyhow::ensure!(
        invalid == 0 && valid > 0 && decoded_small > 0,
        "Unexpected invalid NCZ layout or missing small decode fixture"
    );
    Ok(())
}

#[test]
#[ignore = "requires Stage 13 real read-only content and 16GiB QA scratch; may take several minutes"]
fn isolated_stage13_real_two_dlc_cumulative_nsz_wrapper() -> Result<()> {
    let (source, dlcs, root) = fixture()?;
    anyhow::ensure!(fs2::available_space(&root)? >= 16 * (1u64 << 30));
    anyhow::ensure!(fs2::available_space(std::env::temp_dir())? >= 16 * (1u64 << 30));
    let started = Instant::now();
    let private = tempfile::Builder::new()
        .prefix("stage13-real-dlc-")
        .tempdir_in(root)?;
    let keys = scanner::prod_keys().context("read-only prod.keys missing")?;
    let (base, prior, missing) = choose_missing(&source, &dlcs, private.path(), &keys)?;
    anyhow::ensure!(
        missing.len() >= 2,
        "Need two DLCs not already present in source NSZ"
    );
    let prior_update_count = prior
        .iter()
        .filter(|c| c.meta_type == ContentMetaType::Update)
        .count();
    let initial_sha = sha256_file(&source)?;
    let patch_hashes = missing
        .iter()
        .map(|(_, _, path)| sha256_file(path))
        .collect::<Result<Vec<_>>>()?;
    let isolated_base = private.path().join("source_base.nsz");
    fs::copy(&source, &isolated_base)?;
    anyhow::ensure!(initial_sha == sha256_file(&isolated_base)?);
    println!(
        "STAGE13_ISOLATED_BASE_COPIED bytes={} seconds={:.2}",
        fs::metadata(&isolated_base)?.len(),
        started.elapsed().as_secs_f64()
    );

    let mut current = isolated_base;
    let mut previous_content = prior;
    for (i, (id, version, patch)) in missing.iter().enumerate() {
        println!(
            "STAGE13_REAL_DLC_MERGE_START step={} id={} v={}",
            i + 1,
            id,
            version
        );
        let merged = merge_or_recover_dlc_controlled(
            &current,
            patch,
            &base,
            id,
            i64::from(*version),
            &MergeControl::default(),
        )?;
        anyhow::ensure!(merged.parent() == Some(private.path()) && merged.is_file());
        anyhow::ensure!(
            merged
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")),
            "Original NSZ output type not preserved"
        );
        let index = nsrp_core::formats::pfs0::Pfs0Index::parse(&merged)?;
        anyhow::ensure!(
            index
                .entries
                .iter()
                .any(|entry| entry.name.ends_with(".ncz")),
            "Merged output was not compressed NSZ"
        );
        let final_meta = discovered(
            &merged,
            &private.path().join(format!("step_{}_meta", i + 1)),
            &keys,
        )?;
        validate_dlc_cnmts(&final_meta, &base, id, i64::from(*version))?;
        ensure_existing_content_preserved(&previous_content, &final_meta)?;
        anyhow::ensure!(
            final_meta
                .iter()
                .filter(|c| c.meta_type == ContentMetaType::Update)
                .count()
                >= prior_update_count
        );
        println!(
            "STAGE13_REAL_DLC_STEP_PASS step={} id={} result_bytes={} cnmts={} dlcs={} total_seconds={:.2}",
            i+1, id, fs::metadata(&merged)?.len(), final_meta.len(),
            final_meta.iter().filter(|c| c.meta_type == ContentMetaType::Dlc).count(),
            started.elapsed().as_secs_f64()
        );
        let published_hash = sha256_file(&merged)?;
        let recovered = merge_or_recover_dlc_controlled(
            &current,
            patch,
            &base,
            id,
            i64::from(*version),
            &MergeControl::default(),
        )?;
        anyhow::ensure!(recovered == merged && published_hash == sha256_file(&merged)?);
        previous_content = final_meta;
        current = merged;
    }
    anyhow::ensure!(
        initial_sha == sha256_file(&source)?,
        "Original input changed"
    );
    for (i, (_, _, path)) in missing.iter().enumerate() {
        anyhow::ensure!(
            patch_hashes[i] == sha256_file(path)?,
            "Original DLC changed"
        );
    }
    // Stage 14 uses the fully verified cumulative output for a separate,
    // owned Eden loader smoke. Opt-in only: never publish QA packages into
    // the user's game library, and never overwrite an existing export.
    if let Some(value) = std::env::var_os("NSL_STAGE14_EDEN_EXPORT") {
        let export = PathBuf::from(value);
        let parent = fs::canonicalize(export.parent().context("Eden export has no parent")?)?;
        let qa = fs::canonicalize(std::env::var("NSL_STAGE13_QA_ROOT")?)?;
        anyhow::ensure!(
            parent.starts_with(&qa) && parent != qa && !export.exists(),
            "Eden export must be a new file in a private QA subdirectory"
        );
        anyhow::ensure!(
            export
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")),
            "Eden export must retain compressed NSZ format"
        );
        let mut staged = tempfile::Builder::new()
            .prefix(".stage14-export-")
            .tempfile_in(&parent)?;
        let copied = std::io::copy(&mut File::open(&current)?, staged.as_file_mut())?;
        anyhow::ensure!(copied == fs::metadata(&current)?.len());
        staged.as_file_mut().sync_all()?;
        let staged_hash = sha256_file(staged.path())?;
        let final_hash = sha256_file(&current)?;
        anyhow::ensure!(staged_hash == final_hash, "Eden export SHA-256 mismatch");
        staged.persist_noclobber(&export)?;
        let digest = final_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        println!(
            "STAGE14_EDEN_EXPORT_PASS file={} bytes={} sha256={}",
            export.display(),
            copied,
            digest
        );
    }
    println!("STAGE13_REAL_TWO_DLC_CUMULATIVE_NSZ_PASS two_dlcs=true original_sha_unchanged=true elapsed={:.2}",
        started.elapsed().as_secs_f64());
    Ok(())
}

/// Stage 26: prove the real two-DLC cumulative merge is also safe across the
/// production batch journal's stop/restart boundary and intermediate-output
/// retirement. All game inputs are read-only; the game and downloaded DLC
/// packages used by the engine are copies inside an auto-cleaned QA root.
#[test]
#[ignore = "requires Stage13 read-only NSZ+DLC fixtures, isolated 16GiB QA and prod.keys"]
fn stage26_real_two_dlc_batch_cancel_restart_and_cleanup() -> Result<()> {
    use crate::update_batch::{
        BatchKind, UpdateBatch, UpdateBatchJob, UpdateBatchJobStatus, UpdateBatchRow,
    };

    let (source, dlc_dir, qa_root) = fixture()?;
    const GIB: u64 = 1024 * 1024 * 1024;
    anyhow::ensure!(fs2::available_space(&qa_root)? >= 16 * GIB);
    anyhow::ensure!(fs2::available_space(std::env::temp_dir())? >= 16 * GIB);
    let started = Instant::now();
    let private = tempfile::Builder::new()
        .prefix("stage26-dlc-batch-")
        .tempdir_in(&qa_root)?;
    let keys = scanner::prod_keys().context("read-only prod.keys missing")?;
    let (base, prior_cnmts, candidates) = choose_missing(&source, &dlc_dir, private.path(), &keys)?;
    anyhow::ensure!(
        candidates.len() >= 2,
        "two distinct real missing DLCs required"
    );
    let candidates = &candidates[..2];
    anyhow::ensure!(candidates[0].0 != candidates[1].0);
    let source_sha = sha256_file(&source)?;
    let dlc_shas = candidates
        .iter()
        .map(|(_, _, input)| sha256_file(input))
        .collect::<Result<Vec<_>>>()?;
    let game = private.path().join("source-game.nsz");
    fs::copy(&source, &game)?;
    anyhow::ensure!(sha256_file(&game)? == source_sha);
    let game_size = fs::metadata(&game)?.len();
    let app_root = private.path().join("private-app-state");
    fs::create_dir_all(app_root.join("downloads"))?;

    let mut rows = Vec::new();
    for (index, (dlc_id, version, input)) in candidates.iter().enumerate() {
        let folder = app_root
            .join("downloads")
            .join(format!("stage26-package-{index}"));
        fs::create_dir_all(&folder)?;
        let downloaded = folder.join(format!("{dlc_id}.nsp"));
        fs::copy(input, &downloaded)?;
        anyhow::ensure!(sha256_file(&downloaded)? == dlc_shas[index]);
        // This runs the production input-path and trusted native/header CNMT
        // verification, not filename-based verification.
        crate::update_batch::verify_downloaded_dlc(
            &app_root,
            &downloaded,
            &base,
            dlc_id,
            i64::from(*version),
        )?;
        rows.push(UpdateBatchRow {
            library_id: 26,
            title_id: base.clone(),
            dlc_id: Some(dlc_id.clone()),
            name: format!("Stage26 verified DLC {}", index + 1),
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
                key: format!("stage26-torrent-{index}"),
                directory: folder,
                output: downloaded,
                expected_version: i64::from(*version),
                expected_file_size: fs::metadata(input)?.len(),
                original_size: game_size,
                transfer_finished: true,
                recovering_since: None,
                status: UpdateBatchJobStatus::Queued,
                verifying: None,
                merge_control: None,
            }),
        });
    }
    let mut batch = UpdateBatch {
        kind: BatchKind::Dlc,
        rows,
        running: false,
        cursor: 2,
        active: None,
        confirm_queue: false,
        preview_cache: Default::default(),
        preview_cache_bytes: 0,
        pending_catalog_hints: Default::default(),
    };
    crate::update_manifest::save(&app_root, &batch)?;
    anyhow::ensure!(crate::update_manifest::load(&app_root)?.is_some());

    let (first_id, first_version, _) = &candidates[0];
    let first_package = batch.rows[0]
        .job
        .as_ref()
        .context("missing downloaded first DLC")?
        .output
        .clone();
    let first_output = merge_or_recover_dlc_controlled(
        &game,
        &first_package,
        &base,
        first_id,
        i64::from(*first_version),
        &MergeControl::default(),
    )?;
    anyhow::ensure!(first_output.is_file());
    anyhow::ensure!(
        first_output == expected_dlc_output(&game, &base, first_id, i64::from(*first_version))?
    );
    let first_cnmts = discovered(&first_output, &private.path().join("first-meta"), &keys)?;
    validate_dlc_cnmts(&first_cnmts, &base, first_id, i64::from(*first_version))?;
    ensure_existing_content_preserved(&prior_cnmts, &first_cnmts)?;
    batch.rows[0].job.as_mut().unwrap().status = UpdateBatchJobStatus::Merged(first_output.clone());
    crate::update_manifest::save(&app_root, &batch)?;

    // Simulate a cancel at the boundary between two real DLC publications;
    // cancellation must not create the second output or harm the first.
    let (second_id, second_version, _) = &candidates[1];
    let second_package = batch.rows[1].job.as_ref().unwrap().output.clone();
    let expected_second = expected_dlc_output(&game, &base, second_id, i64::from(*second_version))?;
    let published_before_cancel = sha256_file(&first_output)?;
    let control = MergeControl::default();
    control.request_cancel();
    anyhow::ensure!(
        merge_or_recover_dlc_controlled(
            &first_output,
            &second_package,
            &base,
            second_id,
            i64::from(*second_version),
            &control,
        )
        .is_err(),
        "cancelled second merge unexpectedly succeeded"
    );
    anyhow::ensure!(!expected_second.exists());
    anyhow::ensure!(sha256_file(&first_output)? == published_before_cancel);

    // Simulated app exit and restart: reload from the real durable JSON,
    // resolve the first DLC as the next input and resume the queued second.
    drop(batch);
    let mut restored = crate::update_manifest::load(&app_root)?
        .context("Stage26 durable batch did not survive restart")?;
    let resume_from = crate::app::update_batch::cumulative_dlc_input(&restored, 1)
        .context("second DLC blocked despite completed predecessor")?
        .context("first DLC publication lost during restart")?;
    anyhow::ensure!(resume_from == first_output);
    anyhow::ensure!(restored.rows[1]
        .job
        .as_ref()
        .is_some_and(|job| job.transfer_finished && job.status == UpdateBatchJobStatus::Queued));
    let final_output = merge_or_recover_dlc_controlled(
        &resume_from,
        &second_package,
        &base,
        second_id,
        i64::from(*second_version),
        &MergeControl::default(),
    )?;
    anyhow::ensure!(final_output == expected_second && final_output.is_file());
    let final_index = nsrp_core::formats::pfs0::Pfs0Index::parse(&final_output)?;
    anyhow::ensure!(
        final_index
            .entries
            .iter()
            .any(|entry| entry.name.ends_with(".ncz")),
        "cumulative output must remain genuinely compressed NSZ"
    );
    let final_cnmts = discovered(&final_output, &private.path().join("final-meta"), &keys)?;
    validate_dlc_cnmts(&final_cnmts, &base, first_id, i64::from(*first_version))?;
    validate_dlc_cnmts(&final_cnmts, &base, second_id, i64::from(*second_version))?;
    ensure_existing_content_preserved(&prior_cnmts, &final_cnmts)?;
    ensure_existing_content_preserved(&first_cnmts, &final_cnmts)?;
    let final_sha = sha256_file(&final_output)?;
    restored.rows[1].job.as_mut().unwrap().status =
        UpdateBatchJobStatus::Merged(final_output.clone());
    crate::update_manifest::save(&app_root, &restored)?;

    // Real production finalization: save a crash-safe redirect BEFORE
    // retiring the first NSL output. Never delete downloaded NSPs or source.
    let retired =
        crate::app::update_batch::consolidate_finished_dlc_outputs(&app_root, &mut restored)?;
    anyhow::ensure!(retired == 1, "one prior NSL result should be retired");
    anyhow::ensure!(!first_output.exists());
    anyhow::ensure!(final_output.is_file() && sha256_file(&final_output)? == final_sha);
    let durable = crate::update_manifest::load(&app_root)?
        .context("final two-DLC journal missing after cleanup")?;
    anyhow::ensure!(durable.rows.iter().all(|row| {
        row.job
            .as_ref()
            .is_some_and(|job| job.status == UpdateBatchJobStatus::Merged(final_output.clone()))
    }));
    anyhow::ensure!(
        crate::app::update_batch::consolidate_finished_dlc_outputs(&app_root, &mut restored)? == 0,
        "repeated finalization must be idempotent"
    );
    anyhow::ensure!(sha256_file(&source)? == source_sha);
    anyhow::ensure!(sha256_file(&game)? == source_sha);
    for (index, (_, _, source_dlc)) in candidates.iter().enumerate() {
        anyhow::ensure!(sha256_file(source_dlc)? == dlc_shas[index]);
        anyhow::ensure!(
            sha256_file(&durable.rows[index].job.as_ref().unwrap().output)? == dlc_shas[index],
            "downloaded DLC inputs must never be cleanup targets"
        );
    }
    println!(
        "STAGE26_REAL_TWO_DLC_BATCH_RESTART_PASS dlcs=2 base={} initial_cnmts={} final_cnmts={} final_bytes={} cancelled_before_second=true journal_restored=true retired_intermediate=1 compressed_nsz=true original_sha_unchanged=true elapsed_s={:.2}",
        base,
        prior_cnmts.len(),
        final_cnmts.len(),
        fs::metadata(&final_output)?.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
