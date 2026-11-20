//! Opt-in production-wrapper QA using *actual* isolated Switch content and an
//! unsigned synthetic gamecard. Not a retail XCI or Eden boot qualification.
use super::*;
use nsrp_core::formats::{hfs0, pfs0::Pfs0Index};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Instant;

fn digest_file(path: &Path) -> Result<[u8; 32]> {
    let mut source = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let bytes = source.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        digest.update(&buffer[..bytes]);
    }
    Ok(digest.finalize().into())
}

fn synthetic_gamecard_with_original_cnmts(
    decoded_base: &Path,
    gamecard: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let index = Pfs0Index::parse(decoded_base)?;
    let names = index
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    anyhow::ensure!(names.iter().any(|name| name.ends_with(".cnmt.nca")));
    let mut out = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(gamecard)?;
    let mut head = [0u8; 0x400];
    head[0x100..0x104].copy_from_slice(b"HEAD");
    head[0x130..0x138].copy_from_slice(&0x400u64.to_le_bytes());
    out.write_all(&head)?;
    let mut buf = vec![0u8; 2 * 1024 * 1024];
    hfs0::build_to_stream(
        &["secure".into(), "normal".into()],
        &mut out,
        0x200,
        0x200,
        cancel,
        |partition, root| {
            if partition == 0 {
                hfs0::build_to_stream(&names, root, 0x200, 0x200, cancel, |position, target| {
                    let entry = &index.entries[position];
                    let at = index
                        .data_start
                        .checked_add(entry.offset)
                        .context("NSP offset overflow")?;
                    let mut input = File::open(decoded_base)?;
                    input.seek(SeekFrom::Start(at))?;
                    let mut remaining = entry.size;
                    while remaining > 0 {
                        anyhow::ensure!(
                            !cancel.load(std::sync::atomic::Ordering::Relaxed),
                            "QA gamecard builder cancelled"
                        );
                        let take = remaining.min(buf.len() as u64) as usize;
                        input.read_exact(&mut buf[..take])?;
                        target.write_all(&buf[..take])?;
                        remaining -= take as u64;
                    }
                    Ok(())
                })?;
            } else {
                root.write_all(&[0x77u8; 0x400])?;
            }
            Ok(())
        },
    )?;
    out.sync_all()?;
    Ok(())
}

#[test]
#[ignore = "requires NSL_QA_ROOT, NSL_QA_BASE (.nsz), NSL_QA_PATCH (.nsp), and read-only prod.keys"]
fn isolated_native_xci_update_wrapper_end_to_end() -> Result<()> {
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
    anyhow::ensure!(fs2::available_space(&root)? >= 5 * (1u64 << 30));
    anyhow::ensure!(fs2::available_space(std::env::temp_dir())? >= 5 * (1u64 << 30));
    let private = tempfile::Builder::new()
        .prefix("stage10-wrapper-xci-")
        .tempdir_in(&root)?;
    let decoded = private.path().join("decoded_qa_base.nsp");
    let cancel = AtomicBool::new(false);
    compress::decompress_nsz(&base, &decoded, &cancel, &mut |_| {}, &mut |_| {})?;
    let original = private.path().join("unsigned_original.xci");
    synthetic_gamecard_with_original_cnmts(&decoded, &original, &cancel)?;
    let original_sha = digest_file(&original)?;
    let prod_keys = scanner::prod_keys().context("read-only prod.keys unavailable")?;
    let disabled_tool = private.path().join("_external_tool_disabled.exe");
    let (_, original_cnmts, _) = cnmt::discover_game_metadata(
        &original,
        &private.path().join("source_meta"),
        &disabled_tool,
        &prod_keys,
        &cancel,
    )?;
    anyhow::ensure!(original_cnmts
        .iter()
        .any(|c| c.meta_type == ContentMetaType::Base));
    let start = Instant::now();
    let published = merge_verified_package(&original, &patch, "0100F7301AAB6000", 65536)?;
    let elapsed = start.elapsed().as_secs_f64();
    anyhow::ensure!(published.is_file() && published.parent() == Some(private.path()));
    let (_, result_cnmts, _) = cnmt::discover_game_metadata(
        &published,
        &private.path().join("output_meta"),
        &disabled_tool,
        &prod_keys,
        &cancel,
    )?;
    validate_selected_cnmts(&result_cnmts, "0100F7301AAB6000", 65536)?;
    let leaves = hfs0::collect_leaf_entries(&published, false)?;
    anyhow::ensure!(
        leaves
            .iter()
            .filter(|(name, _, _)| name.ends_with(".nca"))
            .count()
            >= 2
    );
    let published_sha = digest_file(&published)?;
    anyhow::ensure!(
        merge_verified_package(&original, &patch, "0100F7301AAB6000", 65536).is_err(),
        "duplicate publication must reject a preexisting result"
    );
    anyhow::ensure!(published_sha == digest_file(&published)?);
    anyhow::ensure!(original_sha == digest_file(&original)?);
    println!(
        "STAGE10_NSL_XCI_WRAPPER_REAL_CONTENT_PASS output_bytes={} nca_ticket_entries={} merge_seconds={elapsed:.3}",
        std::fs::metadata(&published)?.len(),
        leaves.len()
    );
    Ok(())
}
