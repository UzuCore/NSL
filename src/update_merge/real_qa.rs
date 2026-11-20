//! Long-running real package QA. All tests require explicit --ignored opt-in.
use super::*;
use sha2::Digest;

fn qa_pfs0_region_sha256(
    path: &Path,
    index: &nsrp_core::formats::pfs0::Pfs0Index,
    entry: &nsrp_core::formats::pfs0::Pfs0Entry,
) -> [u8; 32] {
    use std::io::{Read, Seek, SeekFrom};

    let mut input = File::open(path).unwrap();
    input
        .seek(SeekFrom::Start(index.data_start + entry.offset))
        .unwrap();
    let mut remaining = entry.size;
    let mut buffer = vec![0u8; 1 << 20];
    let mut digest = sha2::Sha256::new();
    while remaining != 0 {
        let amount = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..amount]).unwrap();
        digest.update(&buffer[..amount]);
        remaining -= amount as u64;
    }
    digest.finalize().into()
}

/// No decompression, re-encoding, or modification of source NSZ. Validates
/// the exact compressed ranges the Stage 5 passthrough will use.
#[test]
#[ignore = "requires NSL_QA_ROOT and NSL_QA_BASE pointing to an isolated, read-only NSZ"]
fn real_package_ncz_passthrough_preflight_readonly() {
    use nsrp_core::formats::{ncz_stream::validate_ncz_region_layout, pfs0::Pfs0Index};
    use std::io::BufReader;

    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let source = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    assert!(source.starts_with(&root));
    assert!(source
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")));
    let index = Pfs0Index::parse(&source).unwrap();
    let mut reader = BufReader::new(File::open(&source).unwrap());
    let mut count = 0u64;
    let mut reconstructed_size = 0u64;
    for entry in &index.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".ncz") {
            continue;
        }
        let restored =
            validate_ncz_region_layout(&mut reader, index.data_start + entry.offset, entry.size)
                .unwrap_or_else(|error| panic!("malformed NCZ {}: {error}", entry.name));
        assert!(restored >= 0x4000);
        reconstructed_size += restored;
        count += 1;
    }
    assert!(count > 0, "read-only sample contains no NCZ");
    println!(
        "NSL_STAGE5_NCZ_READONLY_PREFLIGHT_PASS blocks={count} reconstructed_bytes={reconstructed_size}"
    );
}

/// Fast NSZ composition against read-only, explicitly isolated game copies.
/// A reference from the previous fully SHA-256 verified Stage 3 output can
/// additionally prove every decompressed NCA is byte-identical. For very
/// large input, STRUCTURAL_ONLY skips the expensive double-decompression.
#[test]
#[ignore = "requires NSL_QA_ROOT, NSL_QA_BASE (.nsz), NSL_QA_PATCH (.nsp), NSL_QA_BASE_ID, NSL_QA_VERSION"]
fn real_package_fast_nsz_equivalence() {
    use std::time::Instant;

    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let base = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    let patch = std::fs::canonicalize(std::env::var("NSL_QA_PATCH").unwrap()).unwrap();
    let base_id = std::env::var("NSL_QA_BASE_ID").unwrap();
    let version: i64 = std::env::var("NSL_QA_VERSION").unwrap().parse().unwrap();
    assert!(base.starts_with(&root) && patch.starts_with(&root));
    assert!(base
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")));
    assert!(patch
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nsp")));
    let temp = tempfile::Builder::new()
        .prefix("stage4-nsz-qa-")
        .tempdir_in(&root)
        .unwrap();
    let prod_keys = scanner::prod_keys().unwrap();
    let mut keys = KeySet::load_with_local_title_keys(&prod_keys).unwrap();
    let tool = temp.path().join("_external_tools_disabled.exe");
    let cancel = AtomicBool::new(false);
    let precompress_patch = std::env::var("NSL_QA_PRECOMPRESS_PATCH").ok().as_deref() == Some("1");
    let patch_nsz = temp.path().join("independent_patch.nsz");
    if precompress_patch {
        compress::compress_nsp(
            &patch,
            &patch_nsz,
            &mut keys,
            &compression_options(),
            &cancel,
            &mut |_| {},
            &mut |_| {},
        )
        .unwrap();
    }
    let merge_patch = if precompress_patch {
        &patch_nsz
    } else {
        &patch
    };
    let started = Instant::now();
    let output = fast_nsz::try_merge_nsz_fast(
        fast_nsz::FastNszRequest {
            original: &base,
            patch: merge_patch,
            base_id: &base_id,
            version,
            work: temp.path(),
            disabled_tool: &tool,
            prod_keys: &prod_keys,
        },
        &mut keys,
        &cancel,
    )
    .unwrap()
    .expect("isolated NSZ + NSP samples should use the fast path");
    if precompress_patch {
        assert!(
            !temp.path().join("fast_patch.nsz").exists(),
            "already compressed NSZ update was redundantly re-encoded"
        );
    }
    let elapsed = started.elapsed();
    let index = nsrp_core::formats::pfs0::Pfs0Index::parse(&output).unwrap();
    let compressed = index
        .entries
        .iter()
        .filter(|entry| entry.name.to_ascii_lowercase().ends_with(".ncz"))
        .count();
    assert!(compressed > 0);
    if precompress_patch {
        let mut passthrough_counts = [0usize; 2];
        for (position, source) in [&base, &patch_nsz].iter().enumerate() {
            let source_index = nsrp_core::formats::pfs0::Pfs0Index::parse(source).unwrap();
            for entry in source_index
                .entries
                .iter()
                .filter(|entry| entry.name.to_ascii_lowercase().ends_with(".ncz"))
            {
                let Some(final_entry) = index.entry(&entry.name) else {
                    continue;
                };
                assert_eq!(entry.size, final_entry.size);
                assert_eq!(
                    qa_pfs0_region_sha256(source, &source_index, entry),
                    qa_pfs0_region_sha256(&output, &index, final_entry),
                    "compressed NCZ bytes were changed: {}",
                    entry.name
                );
                passthrough_counts[position] += 1;
            }
        }
        assert!(passthrough_counts.iter().all(|&count| count > 0));
        println!(
            "NSL_STAGE6_DUAL_NSZ_BYTE_PASSTHROUGH_PASS base={} update={}",
            passthrough_counts[0], passthrough_counts[1]
        );
    }
    let (_, cnmts, _) = cnmt::discover_game_metadata(
        &output,
        &temp.path().join("verify"),
        &tool,
        &prod_keys,
        &cancel,
    )
    .unwrap();
    validate_selected_cnmts(&cnmts, &base_id, version).unwrap();
    println!(
        "NSL_STAGE4_FAST_NSZ seconds={:.3} output_bytes={} entries={} ncz={}",
        elapsed.as_secs_f64(),
        std::fs::metadata(&output).unwrap().len(),
        index.entries.len(),
        compressed,
    );
    if let Ok(path) = std::env::var("NSL_QA_EDEN_EXPORT") {
        use std::io::Write;
        let destination = PathBuf::from(path);
        let parent = std::fs::canonicalize(destination.parent().unwrap()).unwrap();
        assert!(
            parent.starts_with(&root),
            "Eden output must be under QA root"
        );
        assert!(
            destination
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")),
            "Eden QA output must have an NSZ extension"
        );
        // Exclusive creation rejects overwriting any pre-existing user or QA
        // file. On failure, remove only the file just created by this test.
        let mut copied = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .unwrap();
        let result = (|| -> std::io::Result<u64> {
            let copied_bytes = std::io::copy(&mut File::open(&output)?, &mut copied)?;
            copied.flush()?;
            copied.sync_all()?;
            Ok(copied_bytes)
        })();
        if result.is_err() {
            drop(copied);
            let _ = std::fs::remove_file(&destination);
        }
        assert_eq!(result.unwrap(), std::fs::metadata(&output).unwrap().len());
        println!(
            "NSL_STAGE5_EDEN_ISOLATED_ARTIFACT={}",
            destination.display()
        );
    }
    if std::env::var("NSL_QA_STRUCTURAL_ONLY").ok().as_deref() == Some("1") {
        return;
    }
    let reference = std::fs::canonicalize(std::env::var("NSL_QA_EXPECTED").unwrap()).unwrap();
    assert!(reference.starts_with(&root));
    let decoded_new = temp.path().join("fast_decoded.nsp");
    let decoded_reference = temp.path().join("reference_decoded.nsp");
    compress::decompress_nsz(&output, &decoded_new, &cancel, &mut |_| {}, &mut |_| {}).unwrap();
    compress::decompress_nsz(
        &reference,
        &decoded_reference,
        &cancel,
        &mut |_| {},
        &mut |_| {},
    )
    .unwrap();
    let new_hashes = qa_nca_hashes(&decoded_new);
    let reference_hashes = qa_nca_hashes(&decoded_reference);
    assert_eq!(new_hashes, reference_hashes, "every merged NCA must match");
    println!("NSL_STAGE4_SHA256_ALL_NCA_PASS count={}", new_hashes.len());
}

/// Opt-in, end-to-end backend test on private copies. Exercises atomic
/// publication, journal recovery and output collision checks.
#[test]
#[ignore = "requires NSL_QA_ROOT, NSL_QA_BASE (.nsz), NSL_QA_PATCH (.nsp), NSL_QA_BASE_ID, NSL_QA_VERSION; use a sub-1GiB sample"]
fn real_package_fast_nsz_end_to_end_publish() {
    use std::io::Read;

    fn file_sha256(path: &Path) -> [u8; 32] {
        let mut file = File::open(path).unwrap();
        let mut digest = sha2::Sha256::new();
        let mut buffer = vec![0u8; 2 * 1024 * 1024];
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        digest.finalize().into()
    }

    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let original = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    let patch = std::fs::canonicalize(std::env::var("NSL_QA_PATCH").unwrap()).unwrap();
    let base_id = std::env::var("NSL_QA_BASE_ID").unwrap();
    let version: i64 = std::env::var("NSL_QA_VERSION").unwrap().parse().unwrap();
    assert!(original.starts_with(&root) && patch.starts_with(&root));
    assert!(std::fs::metadata(&original).unwrap().len() < (1 << 30));
    assert!(std::fs::metadata(&patch).unwrap().len() < (1 << 30));
    let temp = tempfile::Builder::new()
        .prefix("stage4-publish-qa-")
        .tempdir_in(&root)
        .unwrap();
    let base_copy = temp.path().join("base.nsz");
    let patch_copy = temp.path().join("patch.nsp");
    // Unlike hard links, disposable copies can never modify original QA
    // files even if a future regression accidentally opens inputs for write.
    std::fs::copy(&original, &base_copy).unwrap();
    std::fs::copy(&patch, &patch_copy).unwrap();
    let base_hash = file_sha256(&base_copy);
    let raw_patch_hash = file_sha256(&patch_copy);
    let compressed_patch = temp.path().join("patch.nsz");
    let precompress_patch = std::env::var("NSL_QA_PRECOMPRESS_PATCH").ok().as_deref() == Some("1");
    if precompress_patch {
        let prod_keys = scanner::prod_keys().unwrap();
        let mut keys = KeySet::load_with_local_title_keys(&prod_keys).unwrap();
        compress::compress_nsp(
            &patch_copy,
            &compressed_patch,
            &mut keys,
            &compression_options(),
            &AtomicBool::new(false),
            &mut |_| {},
            &mut |_| {},
        )
        .unwrap();
    }
    let effective_patch = if precompress_patch {
        &compressed_patch
    } else {
        &patch_copy
    };
    let patch_hash = file_sha256(effective_patch);
    let output = merge_verified_package(&base_copy, effective_patch, &base_id, version).unwrap();
    assert!(output.starts_with(temp.path()) && output.is_file());
    assert_eq!(output.extension().and_then(|ext| ext.to_str()), Some("nsz"));
    let output_size = std::fs::metadata(&output).unwrap().len();
    assert!(output_size > 0);
    assert_eq!(
        merge_or_recover_existing(&base_copy, effective_patch, &base_id, version).unwrap(),
        output
    );
    assert!(merge_verified_package(&base_copy, effective_patch, &base_id, version).is_err());
    assert_eq!(std::fs::metadata(&output).unwrap().len(), output_size);
    assert_eq!(file_sha256(&base_copy), base_hash);
    assert_eq!(file_sha256(effective_patch), patch_hash);
    assert_eq!(file_sha256(&patch_copy), raw_patch_hash);
    println!(
        "NSL_STAGE6_PUBLISH_RECOVERY_INPUT_IMMUTABILITY_PASS compressed_patch={} output_bytes={output_size}",
        precompress_patch
    );
}

#[test]
#[ignore = "requires NSL_QA_ROOT and NSL_QA_BASE pointing to an isolated plain NSP"]
fn real_package_pfs0_repack_without_updates() {
    use nsrp_core::formats::pfs0::{build_from_sources, Pfs0Index, Pfs0Source};

    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let source = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    assert!(source.starts_with(&root) && source.extension().unwrap() == "nsp");
    let output = root.join("repacked_base_only.nsp");
    assert!(!output.exists(), "never overwrite previous QA output");
    let index = Pfs0Index::parse(&source).unwrap();
    let entries: Vec<_> = index
        .entries
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                Pfs0Source::Region {
                    container: source.clone(),
                    offset: index.data_start + entry.offset,
                    size: entry.size,
                },
            )
        })
        .collect();
    build_from_sources(&entries, &output, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        index.entries.len(),
        Pfs0Index::parse(&output).unwrap().entries.len()
    );
    println!("NSL_REPACKED_PFS0={}", output.display());
}

/// Diagnose content selection independently of the NCZ encoder. This
/// writes only inside the explicitly provided, isolated QA root.
#[test]
#[ignore = "requires NSL_QA_ROOT, NSL_QA_BASE and NSL_QA_PATCH"]
fn real_package_merged_nsp_without_recompression() {
    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let base = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    let patch = std::fs::canonicalize(std::env::var("NSL_QA_PATCH").unwrap()).unwrap();
    assert!(base.starts_with(&root) && patch.starts_with(&root));
    let prod_keys = scanner::prod_keys().unwrap();
    let mut keys = KeySet::load_with_local_title_keys(&prod_keys).unwrap();
    let output_dir = root.join("diagnostic_uncompressed_merge");
    assert!(!output_dir.exists());
    std::fs::create_dir(&output_dir).unwrap();
    let disabled_tool = output_dir.join("_nsl_external_tool_disabled.exe");
    let cancelled = AtomicBool::new(false);
    // Keep the directly decoded source too, so Eden can distinguish a
    // malformed original NSZ decode from a malformed merged container.
    let decoded_base = output_dir.join("decoded_original_base.nsp");
    compress::decompress_nsz(&base, &decoded_base, &cancelled, &mut |_| {}, &mut |_| {}).unwrap();
    println!("NSL_DIAGNOSTIC_DECODED_BASE={}", decoded_base.display());
    let result = merge::merge_packages(
        &[decoded_base, patch],
        &output_dir,
        true,
        &output_dir,
        &disabled_tool,
        &prod_keys,
        &mut keys,
        &MergeOptions {
            output_format: MergeFormat::Nsp,
            compress_after: false,
            use_game_title: false,
            decompression_threads: 2,
            compression: compression_options(),
        },
        &cancelled,
        &mut |_| {},
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].starts_with(&output_dir));
    println!("NSL_DIAGNOSTIC_NSP={}", result[0].display());
}

#[test]
#[ignore = "requires NSL_QA_ROOT and NSL_QA_BASE pointing to an isolated plain NSP"]
fn real_package_inspect_nca_headers_readonly() {
    use nsrp_core::formats::{
        nca::{decrypt_header, derive_section_key, make_ncz_plan_from_reader},
        pfs0::Pfs0Index,
    };
    use std::io::{Read, Seek, SeekFrom};
    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let source = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    assert!(source.starts_with(&root) && source.extension().unwrap() == "nsp");
    let key_file = scanner::prod_keys().unwrap();
    let keys = if std::env::var("NSL_QA_KEY_SOURCE").ok().as_deref() == Some("ticket") {
        let mut keys = KeySet::load(&key_file, None).unwrap();
        keys.add_ticket_keys_from_nsp(&source).unwrap();
        keys
    } else {
        KeySet::load_with_local_title_keys(&key_file).unwrap()
    };
    let index = Pfs0Index::parse(&source).unwrap();
    let mut input = File::open(&source).unwrap();
    for entry in index.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".nca") {
            continue;
        }
        input
            .seek(SeekFrom::Start(index.data_start + entry.offset))
            .unwrap();
        let mut header = [0u8; 0xC00];
        input.read_exact(&mut header).unwrap();
        match decrypt_header(&header, &keys) {
            Ok(info) => {
                println!(
                    "QA_NCA_HEADER_PASS {} content_type={} keygen={} rights_managed={} title_key_present={} section_key_derived={} encryption_types={:?}",
                    entry.name,
                    info.content_type,
                    info.key_generation,
                    info.rights_id.iter().any(|&byte| byte != 0),
                    keys.title_key(&info.rights_id).is_some(),
                    derive_section_key(&info, &keys).is_ok(),
                    info.sections.iter().map(|section| section.encryption_type).collect::<Vec<_>>(),
                );
                if info.content_type == 0 && entry.size >= 0x4000 {
                    let offset = index.data_start + entry.offset;
                    input.seek(SeekFrom::Start(offset)).unwrap();
                    let mut head_4000 = [0u8; 0x4000];
                    input.read_exact(&mut head_4000).unwrap();
                    match make_ncz_plan_from_reader(
                        &mut input, offset, entry.size, &head_4000, &keys,
                    ) {
                        Ok(plan) => println!(
                            "QA_NCA_CRYPTO_PLAN_PASS {} spans={}",
                            entry.name,
                            plan.crypto_spans.len()
                        ),
                        Err(error) => {
                            println!("QA_NCA_CRYPTO_PLAN_FAIL {} {error}", entry.name)
                        }
                    }
                }
            }
            Err(error) => println!("QA_NCA_HEADER_FAIL {} {error}", entry.name),
        }
    }
}

#[test]
#[ignore = "requires NSL_QA_ROOT and NSL_QA_BASE pointing to an isolated plain NSP"]
fn real_package_pfs0_reorder_nca_before_tickets() {
    use nsrp_core::formats::pfs0::{build_from_sources, Pfs0Index, Pfs0Source};
    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let source = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    assert!(source.starts_with(&root) && source.extension().unwrap() == "nsp");
    let output = root.join("reordered_nca_first.nsp");
    assert!(!output.exists());
    let index = Pfs0Index::parse(&source).unwrap();
    let mut entries = index
        .entries
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                Pfs0Source::Region {
                    container: source.clone(),
                    offset: index.data_start + entry.offset,
                    size: entry.size,
                },
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|(name, _)| {
        (
            !name.to_ascii_lowercase().ends_with(".nca"),
            name.to_ascii_lowercase(),
        )
    });
    build_from_sources(&entries, &output, &AtomicBool::new(false)).unwrap();
    println!("NSL_NCA_FIRST_PFS0={}", output.display());
}

fn qa_nca_hashes(path: &Path) -> std::collections::BTreeMap<String, (u64, String)> {
    use std::io::{Read, Seek, SeekFrom};

    let index = nsrp_core::formats::pfs0::Pfs0Index::parse(path).unwrap();
    let mut input = File::open(path).unwrap();
    let mut hashes = std::collections::BTreeMap::new();
    let mut buffer = vec![0u8; 2 * 1024 * 1024];
    for entry in index.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".nca") {
            continue;
        }
        input
            .seek(SeekFrom::Start(index.data_start + entry.offset))
            .unwrap();
        let mut remaining = entry.size;
        let mut digest = sha2::Sha256::new();
        while remaining > 0 {
            let amount = remaining.min(buffer.len() as u64) as usize;
            input.read_exact(&mut buffer[..amount]).unwrap();
            digest.update(&buffer[..amount]);
            remaining -= amount as u64;
        }
        hashes.insert(
            entry.name.to_ascii_lowercase(),
            (entry.size, format!("{:x}", digest.finalize())),
        );
    }
    hashes
}

#[test]
#[ignore = "requires the NSL_QA_* variables and an already merged QA sample"]
fn real_package_nsz_roundtrip_nca_hashes() {
    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let original = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    let patch = std::fs::canonicalize(std::env::var("NSL_QA_PATCH").unwrap()).unwrap();
    let id = std::env::var("NSL_QA_ID").unwrap();
    let version: i64 = std::env::var("NSL_QA_VERSION").unwrap().parse().unwrap();
    assert!(original.starts_with(&root) && patch.starts_with(&root));
    let merged = output_for_game(&original, &id, version).unwrap().0;
    assert!(merged.is_file() && merged.starts_with(&root));
    let cancelled = AtomicBool::new(false);
    let mut temporary = Vec::new();
    let mut plain_packages = Vec::new();
    for package in [&original, &patch, &merged] {
        if package
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz"))
        {
            let path = tempfile::Builder::new()
                .prefix(".nsl-nca-qa-")
                .suffix(".nsp")
                .tempfile_in(&root)
                .unwrap()
                .into_temp_path();
            compress::decompress_nsz(package, path.as_ref(), &cancelled, &mut |_| {}, &mut |_| {})
                .unwrap();
            // Compare source NCA bytes as decompressed, preserving the
            // original ticketless header even if title.keys has entries.
            // A canonical retail header can match CNMT but fail to boot.
            plain_packages.push(path.to_path_buf());
            temporary.push(path);
        } else {
            plain_packages.push((*package).clone());
        }
    }
    let base_ncas = qa_nca_hashes(&plain_packages[0]);
    let update_ncas = qa_nca_hashes(&plain_packages[1]);
    let merged_ncas = qa_nca_hashes(&plain_packages[2]);
    assert!(!base_ncas.is_empty() && !update_ncas.is_empty());
    // A B+U source can contain an older Update CNMT; replacing it should
    // *not* preserve the superseded update's NCAs. Verify the selected
    // Base and latest Update content, including their CNMT archives.
    let key_file = scanner::prod_keys().unwrap();
    let disabled_tool = root.join("_nsl_external_tool_disabled.exe");
    let mut verified = [0usize; 2];
    for (index, source, metadata_type, hashes) in [
        (0, "base", ContentMetaType::Base, &base_ncas),
        (1, "patch", ContentMetaType::Update, &update_ncas),
    ] {
        let (_, cnmts, _) = cnmt::discover_game_metadata(
            &plain_packages[index],
            &root.join(format!("nca_hash_cnmt_{index}")),
            &disabled_tool,
            &key_file,
            &cancelled,
        )
        .unwrap();
        let selected = cnmts
            .iter()
            .filter(|cnmt| cnmt.meta_type == metadata_type)
            .max_by_key(|cnmt| cnmt.version)
            .unwrap();
        for name in selected.required_nca_names() {
            let name = name.to_ascii_lowercase();
            let value = hashes
                .get(&name)
                .unwrap_or_else(|| panic!("{source} has no required source NCA: {name}"));
            assert_eq!(
                merged_ncas.get(&name),
                Some(value),
                "{source} NCA hash changed: {name}"
            );
            verified[index] += 1;
        }
    }
    println!(
        "QA_NCA_HASHES selected_base={} selected_update={} merged_entries={} all_byte_identical=true",
        verified[0], verified[1], merged_ncas.len()
    );
    drop(temporary);
}

// Explicitly opt-in to this long-running QA case. Never run it as part of
// cargo test's normal suite and never publish into the source library.
#[test]
#[ignore = "requires NSL_QA_BASE, NSL_QA_PATCH, NSL_QA_ID, NSL_QA_VERSION and NSL_QA_ROOT"]
fn real_package_merge_in_isolated_qa_folder() {
    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT").unwrap()).unwrap();
    let original = std::fs::canonicalize(std::env::var("NSL_QA_BASE").unwrap()).unwrap();
    let patch = std::fs::canonicalize(std::env::var("NSL_QA_PATCH").unwrap()).unwrap();
    let id = std::env::var("NSL_QA_ID").unwrap();
    let version: i64 = std::env::var("NSL_QA_VERSION").unwrap().parse().unwrap();
    assert!(
        original.starts_with(&root),
        "QA 원본은 격리된 QA 폴더에 있어야 합니다"
    );
    assert!(
        patch.starts_with(&root),
        "QA 업데이트는 격리된 QA 폴더에 있어야 합니다"
    );
    let output = merge_verified_package(&original, &patch, &id, version).unwrap();
    assert!(output.starts_with(&root));
    println!("NSL_QA_OUTPUT={}", output.display());
    println!("NSL_QA_BYTES={}", std::fs::metadata(output).unwrap().len());
}

#[test]
#[ignore = "requires NSL_QA_BASE and NSL_QA_PATCH"]
fn real_package_preflight_readonly() {
    let prod_keys = scanner::prod_keys().expect("NSL prod.keys unavailable");
    let cancelled = AtomicBool::new(false);
    let work = tempfile::tempdir().unwrap();
    for var in ["NSL_QA_BASE", "NSL_QA_PATCH"] {
        let path = PathBuf::from(std::env::var(var).unwrap());
        let (_, metadata, _) = cnmt::discover_game_metadata(
            &path,
            &work.path().join(var),
            &work.path().join("_nsl_external_tool_disabled.exe"),
            &prod_keys,
            &cancelled,
        )
        .unwrap();
        println!("{var} {}", path.display());
        for item in metadata {
            println!(
                "CNMT {} {:?} v{} contents={}",
                item.title_id,
                item.meta_type,
                item.version,
                item.contents.len()
            );
            for content in &item.contents {
                println!(
                    "  CNMT_CONTENT id={} type={} size={}",
                    content.nca_id, content.content_type, content.size
                );
            }
        }
    }
}
