//! Verified transaction boundary around the optional external XCI builder.
//!
//! An unsuccessful builder must never delete or truncate an existing user
//! output. Generate the XCI in a private sibling directory, validate its
//! HFS0 content index, and only then atomically replace the destination.
//! Both NSP->XCI conversion and multi-title XCI merge share this one path.

use crate::formats::hfs0::{self, Hfs0Index};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

/// Verify at least one identical physical NCA for every source NCA.
///
/// This is a structural, *not* cryptographic Nintendo signature check.
/// The generated HFS0 must also contain a readable secure partition.
fn validate_xci_payload(path: &Path, expected: &[(String, u64)]) -> Result<()> {
    let size = fs::metadata(path)?.len();
    ensure!(size >= 1024 * 1024, "XCI output is too small");
    let root = hfs0::find_xci_root(path)?;
    let secure = root
        .entry("secure")
        .context("generated XCI has no secure HFS0 partition")?;
    ensure!(secure.size > 0, "XCI secure partition is empty");
    let mut reader = BufReader::new(File::open(path)?);
    let secure_at = root
        .data_start
        .checked_add(secure.offset)
        .context("XCI secure partition offset overflow")?;
    let _ = Hfs0Index::parse_at(&mut reader, secure_at, size)
        .context("XCI secure partition HFS0 index invalid")?;
    let all_files = hfs0::collect_leaf_entries(path, false)?;
    let mut found = BTreeMap::<String, Vec<u64>>::new();
    for (name, _, len) in all_files {
        if name.to_ascii_lowercase().ends_with(".nca") {
            found
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(len);
        }
    }
    ensure!(!found.is_empty(), "generated XCI has no NCA content");
    for (name, len) in expected {
        ensure!(
            found
                .get(&name.to_ascii_lowercase())
                .is_some_and(|sizes| sizes.contains(len)),
            "generated XCI is missing or truncates source NCA {name}"
        );
    }
    Ok(())
}

/// Build in a same-volume temporary directory, validate, then publish.
/// On failure, the previous output remains byte-for-byte untouched.
pub(crate) fn build_xci_verified<F>(
    output: &Path,
    expected_ncas: &[(String, u64)],
    cancel: &AtomicBool,
    mut builder: F,
) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    ensure!(
        !expected_ncas.is_empty(),
        "XCI package has no expected NCAs"
    );
    ensure!(!cancel.load(Ordering::Relaxed), "작업이 중단되었습니다");
    let parent = output.parent().context("XCI destination has no parent")?;
    fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix("nsrp_verified_xci_")
        .tempdir_in(parent)
        .context("XCI same-volume temporary directory creation failed")?;
    let staged = staging.path().join("validated_build.xci");
    builder(&staged)?;
    ensure!(!cancel.load(Ordering::Relaxed), "작업이 중단되었습니다");
    validate_xci_payload(&staged, expected_ncas)?;
    ensure!(!cancel.load(Ordering::Relaxed), "작업이 중단되었습니다");
    // Windows rename within the same volume replaces an existing destination
    // atomically; errors do not remove the user's prior output. No helper
    // EXE sees the final path before the file has passed validation.
    fs::rename(&staged, output).with_context(|| {
        format!(
            "verified XCI atomic publication failed: {}",
            output.display()
        )
    })?;
    Ok(())
}

/// Publish an already verified XCI from the operation workspace. A rename
/// on the same volume replaces the old output atomically. Across volumes,
/// copy to a private sibling in the destination directory, validate that
/// complete copy, and only then atomically publish it.
fn publish_xci_transactionally(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    validate_structure: bool,
) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "작업이 중단되었습니다");
    let expected = if validate_structure {
        let expected = hfs0::collect_leaf_entries(source, false)?
            .into_iter()
            .filter(|(name, _, _)| name.to_ascii_lowercase().ends_with(".nca"))
            .map(|(name, _, size)| (name, size))
            .collect::<Vec<_>>();
        ensure!(!expected.is_empty(), "XCI commit source contains no NCA");
        validate_xci_payload(source, &expected)?;
        expected
    } else {
        // The XCZ->XCI path has already reconstructed the original container.
        // Some older gamecards legitimately contain no NCAs in the synthetic
        // secure partition; do not introduce a new format restriction here.
        ensure!(
            fs::metadata(source)?.len() > 0,
            "reconstructed XCI is empty"
        );
        Vec::new()
    };
    ensure!(!cancel.load(Ordering::Relaxed), "작업이 중단되었습니다");
    if fs::rename(source, destination).is_ok() {
        return Ok(());
    }
    let parent = destination
        .parent()
        .context("XCI destination has no parent")?;
    fs::create_dir_all(parent)?;
    let staged = tempfile::Builder::new()
        .prefix("nsrp_xci_publish_")
        .tempfile_in(parent)?;
    let mut src = BufReader::with_capacity(1024 * 1024, File::open(source)?);
    let mut dst = BufWriter::with_capacity(1024 * 1024, staged.as_file());
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "작업이 중단되었습니다");
        let bytes = src.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        dst.write_all(&buffer[..bytes])?;
    }
    dst.flush()?;
    drop(dst);
    if validate_structure {
        validate_xci_payload(staged.path(), &expected)?;
    } else {
        ensure!(
            fs::metadata(staged.path())?.len() == fs::metadata(source)?.len(),
            "reconstructed XCI copied length mismatch"
        );
    }
    ensure!(!cancel.load(Ordering::Relaxed), "작업이 중단되었습니다");
    fs::rename(staged.path(), destination)
        .with_context(|| format!("verified XCI publication failed: {}", destination.display()))?;
    Ok(())
}

/// Validate fresh NSP->XCI or merged XCI output before publishing.
pub(crate) fn publish_verified_xci(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    publish_xci_transactionally(source, destination, cancel, true)
}

/// Publish a validated XCZ decompression without changing compatibility
/// semantics for legitimate noncanonical source XCI partitions.
pub(crate) fn publish_decoded_xci(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    publish_xci_transactionally(source, destination, cancel, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn synthetic_xci(path: &Path, nca_name: &str, nca_size: usize) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(&[0u8; 0x200])?;
        let cancel = AtomicBool::new(false);
        hfs0::build_to_stream(
            &["secure".to_owned()],
            &mut file,
            0x200,
            1,
            &cancel,
            |_, root| {
                hfs0::build_to_stream(
                    &[nca_name.to_owned()],
                    root,
                    0x200,
                    1,
                    &cancel,
                    |_, leaf| {
                        leaf.write_all(&vec![0x52u8; nca_size])?;
                        Ok(())
                    },
                )?;
                Ok(())
            },
        )?;
        file.set_len(1024 * 1024 + 0x400)?;
        Ok(())
    }

    #[test]
    fn broken_builder_does_not_destroy_existing_xci() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("reusable.xci");
        let before = b"previous finished output".to_vec();
        fs::write(&target, &before)?;
        let cancel = AtomicBool::new(false);
        let expected = vec![("game.nca".to_owned(), 0xC00)];
        let error = build_xci_verified(&target, &expected, &cancel, |staged| {
            fs::write(staged, b"partial builder output")?;
            anyhow::bail!("simulated hacbuild failure")
        });
        assert!(error.is_err());
        assert_eq!(fs::read(&target)?, before);
        assert_eq!(fs::read_dir(temp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn invalid_or_incomplete_xci_is_never_published() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("reusable.xci");
        fs::write(&target, b"verified older output")?;
        let cancel = AtomicBool::new(false);
        let expected = vec![("game.nca".to_owned(), 0xC00)];
        assert!(
            build_xci_verified(&target, &expected, &cancel, |staged| {
                synthetic_xci(staged, "wrong.nca", 0xC00)
            })
            .is_err()
        );
        assert_eq!(fs::read(&target)?, b"verified older output");
        assert_eq!(fs::read_dir(temp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn cancelled_xci_does_not_publish_or_run_builder() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("reusable.xci");
        fs::write(&target, b"verified older output")?;
        let cancel = AtomicBool::new(true);
        let result =
            build_xci_verified(&target, &[("game.nca".to_owned(), 0xC00)], &cancel, |_| {
                anyhow::bail!("builder should not have run")
            });
        assert!(result.is_err());
        assert_eq!(fs::read(&target)?, b"verified older output");
        Ok(())
    }

    #[test]
    fn valid_xci_is_published_only_after_validation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("reusable.xci");
        fs::write(&target, b"verified older output")?;
        let cancel = AtomicBool::new(false);
        build_xci_verified(
            &target,
            &[("game.nca".to_owned(), 0xC00)],
            &cancel,
            |staged| synthetic_xci(staged, "game.nca", 0xC00),
        )?;
        validate_xci_payload(&target, &[("game.nca".to_owned(), 0xC00)])?;
        assert_eq!(fs::read_dir(temp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn verified_xci_commit_keeps_old_output_if_source_invalid() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let old = temp.path().join("existing.xci");
        let bad = temp.path().join("bad.xci");
        fs::write(&old, b"old valid completed output")?;
        fs::write(&bad, b"truncated input")?;
        assert!(publish_verified_xci(&bad, &old, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read(&old)?, b"old valid completed output");
        assert!(bad.exists());
        Ok(())
    }

    #[test]
    fn verified_xci_commit_publishes_valid_candidate() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let old = temp.path().join("existing.xci");
        let next = temp.path().join("next.xci");
        fs::write(&old, b"old valid completed output")?;
        synthetic_xci(&next, "main.nca", 0xC00)?;
        publish_verified_xci(&next, &old, &AtomicBool::new(false))?;
        validate_xci_payload(&old, &[("main.nca".to_owned(), 0xC00)])?;
        assert!(!next.exists());
        Ok(())
    }
}
