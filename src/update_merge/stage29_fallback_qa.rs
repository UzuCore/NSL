//! Stage29 hardlink-denied fallback publication QA, isolated/test-only.
//! This test never opens the game library, keys, real torrent sessions,
//! emulators or user files. It kills only its own child after a real 8MiB write.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const OUTPUT_FILE: &str = "[NSL Merged] [0100D2C0115B6000][v65536].nsz";

fn stream_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = [0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut block)?;
        if count == 0 {
            break;
        }
        hash.update(&block[..count]);
    }
    Ok(format!("{:X}", hash.finalize()))
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Child-only: the harness supplies a private Stage29 root. Actual game
/// publishing code is forced through the hardlink-unsupported copy branch,
/// with the stage and marker in a single owner-tagged workspace.
#[test]
#[ignore = "child-only synthetic 64MiB fallback writer, parent owns subprocess"]
fn stage29_owned_fallback_child() -> Result<()> {
    let Some(root) = std::env::var_os("NSL_STAGE29_FALLBACK_QA_ROOT") else {
        return Ok(());
    };
    let root = fs::canonicalize(root)?;
    anyhow::ensure!(
        root.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("stage29-fallback-")),
        "refusing any non-private Stage29 root"
    );
    let original = fs::canonicalize(root.join("original-unchanged.bin"))?;
    anyhow::ensure!(original.parent() == Some(root.as_path()));
    anyhow::ensure!(fs::metadata(&original)?.len() == 64 * 1024 * 1024);
    let destination = root.join(OUTPUT_FILE);
    let work = tempfile::Builder::new()
        .prefix(".nsl-merge-")
        .tempdir_in(&root)?;
    let lock = hold_merge_workspace_lock(work.path(), &destination)?
        .context("Stage29 NTFS temp workspace lock unavailable")?;
    let staged = work.path().join("ready.nsz");
    fs::copy(&original, &staged)?;
    let cancel = AtomicBool::new(false);
    publish_without_overwrite(&staged, &destination, &cancel)?;
    anyhow::ensure!(
        fs::metadata(&destination)?.len() == 64 * 1024 * 1024,
        "synthetic published fallback was not full length"
    );
    drop(lock);
    drop(work);
    println!("STAGE29_OWNED_FALLBACK_PUBLISH_COMPLETE bytes=67108864");
    Ok(())
}

#[test]
#[ignore = "isolated 64MiB source, test-owned child kill and fallback retry"]
fn stage29_real_fallback_midcopy_kill_and_retry_preserves_original() -> Result<()> {
    let path = std::env::var_os("NSL_STAGE29_QA_ROOT")
        .context("explicit isolated Stage29 QA root required")?;
    let qa = fs::canonicalize(path)?;
    anyhow::ensure!(fs::metadata(&qa)?.is_dir());
    anyhow::ensure!(fs2::available_space(&qa)? >= 2 * (1u64 << 30));
    let started = Instant::now();
    let scratch = tempfile::Builder::new()
        .prefix("stage29-fallback-")
        .tempdir_in(qa)?;
    let root = scratch.path();
    let original = root.join("original-unchanged.bin");
    let unrelated = root.join("user-file.keep");
    fs::write(&unrelated, b"never delete this non-owned source")?;
    let mut out = File::create(&original)?;
    let mut block = vec![0u8; 1024 * 1024];
    for chunk in 0..64u64 {
        for (i, value) in block.iter_mut().enumerate() {
            *value = (chunk as u8).wrapping_mul(37) ^ ((i >> 11) as u8);
        }
        out.write_all(&block)?;
    }
    out.sync_all()?;
    drop(out);
    drop(block);
    let original_hash = stream_sha256(&original)?;
    let destination = root.join(OUTPUT_FILE);
    let exe = std::env::current_exe()?;
    let args = [
        "--exact",
        "update_merge::stage29_fallback_qa::stage29_owned_fallback_child",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ];
    let first_log = File::create(root.join("child-abrupt-stderr.log"))?;
    let child = Command::new(&exe)
        .args(args)
        .env("NSL_STAGE29_FALLBACK_QA_ROOT", root)
        .env("NSL_STAGE29_ABORT_AFTER_8M", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::from(first_log))
        .spawn()?;
    let mut owned = OwnedChild(child);
    let signal = root.join("stage29-fallback-8m.signal");
    let wait = Instant::now();
    while wait.elapsed() < Duration::from_secs(80) {
        if owned.0.try_wait()?.is_some() {
            let diagnostic =
                fs::read_to_string(root.join("child-abrupt-stderr.log")).unwrap_or_default();
            anyhow::bail!("owned child quit before 8MiB fallback write: {diagnostic}");
        }
        if signal.is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::ensure!(
        signal.is_file(),
        "never observed the private 8MiB partial copy"
    );
    anyhow::ensure!(
        !destination.exists(),
        "partial output was published before kill"
    );
    let workspaces = fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".nsl-merge-")
                && entry.path().is_dir()
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        workspaces.len() == 1,
        "expected exactly one owned locked workspace"
    );
    let workspace = &workspaces[0];
    let partial = fs::read_dir(workspace)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".nsl-publish-")
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(partial.len() == 1);
    let partial_bytes = partial[0].metadata()?.len();
    anyhow::ensure!(
        (8 * 1024 * 1024..64 * 1024 * 1024).contains(&partial_bytes),
        "must kill during the real fallback copy, not after completion; observed temp length={partial_bytes}"
    );
    let parent_publish = fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".nsl-publish-")
        });
    anyhow::ensure!(
        !parent_publish,
        "orphan publish temporary leaked beside game files"
    );
    anyhow::ensure!(
        reap_dead_merge_workspaces(root, &destination)? == 0,
        "active workspace was wrongly reaped while child holds ownership lock"
    );
    owned.0.kill()?;
    let _ = owned.0.wait()?;
    anyhow::ensure!(stream_sha256(&original)? == original_hash);
    anyhow::ensure!(!destination.exists());
    anyhow::ensure!(reap_dead_merge_workspaces(root, &destination)? == 1);
    anyhow::ensure!(!workspace.exists());
    anyhow::ensure!(fs::read(&unrelated)? == b"never delete this non-owned source");
    println!(
        "STAGE29_FALLBACK_OWNED_CHILD_KILL_PASS partial_bytes={partial_bytes} \
         output_absent=true active_lock_protected=true orphan_reaped=true \
         original_unchanged=true unowned_publishes=0"
    );
    let second = Command::new(&exe)
        .args(args)
        .env("NSL_STAGE29_FALLBACK_QA_ROOT", root)
        .output()?;
    anyhow::ensure!(
        second.status.success(),
        "owned fallback retry failed: stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    anyhow::ensure!(
        String::from_utf8_lossy(&second.stdout).contains("STAGE29_OWNED_FALLBACK_PUBLISH_COMPLETE"),
        "second child did not confirm it actually finished fallback copy"
    );
    anyhow::ensure!(
        fs::metadata(&destination)?.len() == 64 * 1024 * 1024
            && stream_sha256(&destination)? == original_hash,
        "published fallback data differed from original"
    );
    anyhow::ensure!(stream_sha256(&original)? == original_hash);
    anyhow::ensure!(fs::read(&unrelated)? == b"never delete this non-owned source");
    anyhow::ensure!(
        !fs::read_dir(root)?
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(".nsl-merge-") || name.starts_with(".nsl-publish-")
            }),
        "work folders remained after successful retry"
    );
    println!(
        "STAGE29_FALLBACK_MIDCOPY_KILL_RETRY_PASS copied_bytes=67108864 \
         partial_bytes={partial_bytes} publish_sha256={} original_sha256={} \
         active_lock_protected=true orphan_clean=true no_clobber=true \
         elapsed_s={:.2}",
        stream_sha256(&destination)?,
        original_hash,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
