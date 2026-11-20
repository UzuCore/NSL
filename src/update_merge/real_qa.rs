//! Long-running real package QA. All tests require explicit --ignored opt-in.
use super::*;
use sha2::Digest;

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
