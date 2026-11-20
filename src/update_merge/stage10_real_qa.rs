//! Opt-in, read-only real package QA for the Stage 10 selected-file XCI path.
//! The gamecard template used here is synthetic and is NOT Nintendo-signed.
use super::*;
use nsrp_core::formats::hfs0;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    time::Instant,
};

fn sha256_extent(path: &Path, offset: u64, len: u64) -> Result<[u8; 32]> {
    let mut input = File::open(path)?;
    let input_size = input.metadata()?.len();
    anyhow::ensure!(
        offset.checked_add(len).is_some_and(|end| end <= input_size),
        "XCI 해시 범위가 비정상입니다"
    );
    input.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut remaining = len;
    let mut sha = Sha256::new();
    while remaining > 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..take])?;
        sha.update(&buffer[..take]);
        remaining -= take as u64;
    }
    Ok(sha.finalize().into())
}

fn synthetic_card_template(path: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut head = [0u8; 0x400];
    head[0x100..0x104].copy_from_slice(b"HEAD");
    head[0x130..0x138].copy_from_slice(&0x400u64.to_le_bytes());
    file.write_all(&head)?;
    hfs0::build_to_stream(
        &["secure".to_owned(), "normal".to_owned()],
        &mut file,
        0x200,
        0x200,
        cancel,
        |position, output| {
            if position == 0 {
                hfs0::build_to_stream(
                    &["placeholder.nca".to_owned()],
                    output,
                    0x200,
                    0x200,
                    cancel,
                    |_, nested| {
                        nested.write_all(&[0x55u8; 0x1000])?;
                        Ok(())
                    },
                )?;
            } else {
                output.write_all(&[0x44u8; 0x400])?;
            }
            Ok(())
        },
    )?;
    file.sync_all()?;
    Ok(())
}

#[test]
#[ignore = "requires NSL_QA_ROOT, NSL_QA_BASE (.nsz), NSL_QA_PATCH (.nsp), and read-only prod.keys"]
fn isolated_real_nsz_patch_to_xci_without_intermediate_nsp() -> Result<()> {
    let root = std::fs::canonicalize(std::env::var("NSL_QA_ROOT")?)?;
    let base = std::fs::canonicalize(std::env::var("NSL_QA_BASE")?)?;
    let patch = std::fs::canonicalize(std::env::var("NSL_QA_PATCH")?)?;
    anyhow::ensure!(base.starts_with(&root) && patch.starts_with(&root));
    anyhow::ensure!(base
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("nsz")));
    anyhow::ensure!(patch
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("nsp")));
    anyhow::ensure!(std::fs::metadata(&base)?.len() < (1u64 << 30));
    anyhow::ensure!(std::fs::metadata(&patch)?.len() < (1u64 << 30));
    anyhow::ensure!(fs2::available_space(&root)? >= 4 * (1u64 << 30));
    anyhow::ensure!(fs2::available_space(std::env::temp_dir())? >= 4 * (1u64 << 30));
    let root_hashes = [
        sha256_extent(&base, 0, std::fs::metadata(&base)?.len())?,
        sha256_extent(&patch, 0, std::fs::metadata(&patch)?.len())?,
    ];
    let private = tempfile::Builder::new()
        .prefix("stage10-selected-xci-")
        .tempdir_in(&root)?;
    let cancel = AtomicBool::new(false);
    let template = private.path().join("synthetic_template.xci");
    synthetic_card_template(&template, &cancel)?;
    let output = private.path().join("selected_merged.xci");
    let prod_keys = scanner::prod_keys().context("read-only QA prod.keys is unavailable")?;
    let mut keys = KeySet::load_with_local_title_keys(&prod_keys)?;
    let sources = [base.clone(), patch.clone()];
    let options = MergeOptions {
        output_format: MergeFormat::Nsp,
        compress_after: false,
        use_game_title: false,
        decompression_threads: 2,
        compression: compression_options(),
    };
    let tool = private.path().join("_external_tool_disabled.exe");
    let started = Instant::now();
    let mut selected_bytes = 0u64;
    let mut entry_hashes = Vec::<(String, u64, [u8; 32])>::new();
    let outputs = merge::merge_packages_selected(
        &sources,
        private.path(),
        true,
        private.path(),
        &tool,
        &prod_keys,
        &mut keys,
        &options,
        &cancel,
        &mut |_| {},
        &mut |_| {},
        &mut |group| {
            anyhow::ensure!(
                group
                    .prepared_base
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("nsp")),
                "NSZ original was not prepared as a decoded NSP"
            );
            anyhow::ensure!(group.files.iter().any(|(n, _)| n.ends_with(".cnmt.nca")));
            for (name, path) in group.files {
                let len = std::fs::metadata(path)?.len();
                selected_bytes = selected_bytes
                    .checked_add(len)
                    .context("selected bytes overflow")?;
                entry_hashes.push((name.clone(), len, sha256_extent(path, 0, len)?));
            }
            // NSRP selection should not publish a second, giant NSP to the
            // merge-group directory before the direct XCI builder runs.
            let group_dir = group
                .files
                .first()
                .context("empty CNMT selection")?
                .1
                .parent()
                .context("missing group directory")?;
            anyhow::ensure!(
                !std::fs::read_dir(group_dir)?.any(|file| file.as_ref().is_ok_and(|item| item
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("nsp")))),
                "unexpected intermediate merged NSP during selected-file XCI build"
            );
            xci_merge::build_merged_xci_from_selected_files(
                &template,
                group.files,
                &output,
                group.cancel,
            )?;
            Ok(output.clone())
        },
    )?;
    anyhow::ensure!(outputs.len() == 1 && outputs[0] == output);
    let elapsed = started.elapsed().as_secs_f64();
    let leaves = hfs0::collect_leaf_entries(&output, false)?;
    anyhow::ensure!(
        leaves.len() == entry_hashes.len(),
        "XCI HFS0 selected entry count mismatch"
    );
    for (name, size, hash) in &entry_hashes {
        let (actual_name, offset, actual_size) = leaves
            .iter()
            .find(|(actual, _, _)| actual == name)
            .with_context(|| format!("missing XCI output entry: {name}"))?;
        anyhow::ensure!(actual_name == name && actual_size == size);
        anyhow::ensure!(
            sha256_extent(&output, *offset, *size)? == *hash,
            "XCI NCA/ticket differs: {name}"
        );
    }
    let (_, cnmts, _) = cnmt::discover_game_metadata(
        &output,
        &private.path().join("verify"),
        &tool,
        &prod_keys,
        &cancel,
    )?;
    validate_selected_cnmts(&cnmts, "0100F7301AAB6000", 65536)?;
    anyhow::ensure!(root_hashes[0] == sha256_extent(&base, 0, std::fs::metadata(&base)?.len())?);
    anyhow::ensure!(root_hashes[1] == sha256_extent(&patch, 0, std::fs::metadata(&patch)?.len())?);
    println!(
        "STAGE10_REAL_SELECTED_XCI_NO_INTERMEDIATE_NSP_PASS entries={} selected_bytes={} output_bytes={} elapsed_seconds={elapsed:.3}",
        entry_hashes.len(),
        selected_bytes,
        std::fs::metadata(&output)?.len()
    );
    Ok(())
}
