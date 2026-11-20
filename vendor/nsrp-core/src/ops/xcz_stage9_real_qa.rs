//! Opt-in gamecard pipeline QA using *real* NCZ bytes and an isolated,
//! synthetic HFS0 wrapper. It does not create a Nintendo-signed XCI.

use super::*;
use crate::formats::pfs0::Pfs0Index;

fn hash_region(path: &Path, offset: u64, mut remaining: u64) -> Result<[u8; 32]> {
    let mut input = File::open(path)?;
    input.seek(SeekFrom::Start(offset))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..take])?;
        hasher.update(&buffer[..take]);
        remaining -= take as u64;
    }
    Ok(hasher.finalize().into())
}

#[test]
#[ignore = "requires NSL_QA_ROOT and NSL_QA_BASE pointing to a private sub-1GiB NSZ sample"]
fn isolated_real_ncz_staged_xcz_matches_streamed_decompression() -> Result<()> {
    let qa_root = fs::canonicalize(std::env::var("NSL_QA_ROOT")?)?;
    let source = fs::canonicalize(std::env::var("NSL_QA_BASE")?)?;
    anyhow::ensure!(
        source.starts_with(&qa_root),
        "QA source is outside the private root"
    );
    anyhow::ensure!(
        source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("nsz")),
        "expected isolated NSZ sample"
    );
    anyhow::ensure!(
        fs::metadata(&source)?.len() < (1u64 << 30),
        "QA source too large"
    );
    let index = Pfs0Index::parse(&source)?;
    let compressed = index
        .entries
        .iter()
        .find(|entry| entry.name.to_ascii_lowercase().ends_with(".ncz"))
        .context("no compressed NCZ within isolated NSZ")?;
    anyhow::ensure!(compressed.size < (1u64 << 30), "NCZ source too large");
    let private = tempfile::Builder::new()
        .prefix("stage9-real-xcz-")
        .tempdir_in(&qa_root)?;
    let original_sha = sha256_file(&source)?;
    let compressed_at = index
        .data_start
        .checked_add(compressed.offset)
        .context("NCZ offset overflow")?;
    let ncz_path = private.path().join("real_payload.ncz");
    let mut source_file = File::open(&source)?;
    source_file.seek(SeekFrom::Start(compressed_at))?;
    let mut ncz_output = BufWriter::with_capacity(2 * 1024 * 1024, File::create(&ncz_path)?);
    let cancel = AtomicBool::new(false);
    let mut scratch = vec![0u8; 2 * 1024 * 1024];
    copy_exact(
        &mut source_file,
        &mut ncz_output,
        compressed.size,
        &cancel,
        &mut scratch,
    )?;
    ncz_output.flush()?;
    drop(ncz_output);
    anyhow::ensure!(fs::metadata(&ncz_path)?.len() == compressed.size);
    assert_eq!(
        hash_region(&source, compressed_at, compressed.size)?,
        sha256_file(&ncz_path)?
    );

    let secure = private.path().join("secure.hfs0");
    let root_hfs0 = private.path().join("root.hfs0");
    hfs0::build_from_paths_layout(&[(compressed.name.clone(), ncz_path)], &secure, 0x200, 1)?;
    hfs0::build_from_paths_layout(&[("secure".into(), secure)], &root_hfs0, 0x200, 1)?;
    let gamecard = private.path().join("synthetic_source.xcz");
    let mut image = BufWriter::new(File::create(&gamecard)?);
    let mut header = [0u8; 0x200];
    header[0x100..0x104].copy_from_slice(b"HEAD");
    header[0x130..0x138].copy_from_slice(&0x200u64.to_le_bytes());
    image.write_all(&header)?;
    let mut root_reader = File::open(&root_hfs0)?;
    std::io::copy(&mut root_reader, &mut image)?;
    image.flush()?;
    drop(image);

    let staged = private.path().join("staged_restored.xci");
    let streamed = private.path().join("streamed_restored.xci");
    let start = std::time::Instant::now();
    let staged_stats =
        transform_xci_staged(&gamecard, &staged, None, &cancel, &mut |_| {}, &mut |_| {})?;
    let staged_seconds = start.elapsed().as_secs_f64();
    let start = std::time::Instant::now();
    let streamed_stats = streaming::transform_xci_streamed(
        &gamecard,
        &streamed,
        None,
        &cancel,
        &mut |_| {},
        &mut |_| {},
    )?;
    let streamed_seconds = start.elapsed().as_secs_f64();
    anyhow::ensure!(staged_stats.ncz_count == 1 && streamed_stats.ncz_count == 1);

    let expected_name = replace_extension(&compressed.name, "nca");
    let mut hashes = Vec::new();
    let mut nca_sizes = Vec::new();
    for output in [&staged, &streamed] {
        let leaves = hfs0::collect_leaf_entries(output, false)?;
        let entry = leaves
            .iter()
            .find(|(name, _, _)| name == &expected_name)
            .context("reconstructed synthetic XCI has no NCA")?;
        hashes.push(hash_region(output, entry.1, entry.2)?);
        nca_sizes.push(entry.2);
    }
    assert_eq!(hashes[0], hashes[1], "restored NCA differs between paths");
    assert_eq!(nca_sizes[0], nca_sizes[1]);
    assert_eq!(original_sha, sha256_file(&source)?, "source NSZ changed");
    println!(
        "STAGE9_REAL_NCZ_XCZ_PATHS_PASS compressed_bytes={} restored_nca_bytes={} staged_seconds={staged_seconds:.3} streamed_seconds={streamed_seconds:.3}",
        compressed.size, nca_sizes[0]
    );
    Ok(())
}
