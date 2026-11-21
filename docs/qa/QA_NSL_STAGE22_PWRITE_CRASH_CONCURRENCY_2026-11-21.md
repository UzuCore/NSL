# NSL Stage 22 - Real payload-write faults, deletion journal concurrency, and regression

Date: **2026-11-21**. Repository: `D:\NSW.dev\NSL`, `https://github.com/UzuCore/NSL.git`. Stage 21 starting commit `1544179e6ab26b12ec7aabe2c7c1ff426cd4ac3d`. **Stage 22 verified source commit: `daca18d52839381c694213ccbe484b1db2d3b51d`.**

## Scope and original data protection

- Production changes: `src/torrent.rs` and `src/torrent_removal.rs`. New opt-in isolated E2E tests in `src/torrent_e2e_tests.rs`.
- All torrent fault and concurrency tests create their own generated binary payload and serve it through a localhost-only tracker/seeder, with DHT and public network discovery disabled by the existing test harness.
- No test opens any original Switch game, original key, library `E:\NSW`, save, user torrent download, or user emulator profile for writing. All test fixtures are disposed with their private temporary directory.
- Original `dist\nsl.exe`, `dist\nsl-win64.zip` are explicitly protected and never overwritten. Build output goes only to `D:\NSW.dev\NSL\target\release\nsl.exe`.

## Actual Windows payload-write failure and successful recovery

Test: `stage22_real_librqbit_live_payload_write_failure_recovers` (**PASS**, explicit `--ignored`).

- Generate **8,388,608 bytes** in a private `seed` directory and begin a real localhost BitTorrent download, restricted to a 768 KiB/s seeder.
- Once at least 256 KiB of valid pieces have been downloaded, use the existing `fs2` crate's genuine Windows exclusive byte-range `LockFileEx` lock against the **downloader's output file**. The separate librqbit file handle remains open; no file permissions, user file paths or mock storage implementation are changed.
- The next actual `FileOps::write_chunk` / Windows positional write (`pwrite_all`) attempt fails with **OS error 33**, and the real librqbit fatal error receiver transitions to its terminal error state. NSL's watcher reports `error writing to file 0` (not an injected NSL-only watcher message).
- Release the lock, issue the normal user Retry action, wait for positive retry acceptance and complete the remaining pieces. Final output is **byte-identical to all 8,388,608 source bytes**. Remove the session without deleting the payload and verify that all bytes remain.
- This proves an actual Windows byte-range lock error in the live payload-write path and user retry. It does **not** demonstrate a physically failing disk, out-of-space volume, SMB outage or permission revocation; those can have different recovery characteristics.

## Durable deletion record/crash boundary

Test: `stage22_journal_crash_boundary_rejects_wrong_folder_and_recovers_exact_match` (**PASS**, explicit `--ignored`).

- Generate/download a private 2,097,152-byte file and read its serialized session identity (numeric ID, info hash, output folder).
- Write a synced deletion record with the correct ID/hash but an **unrelated output folder**, restart the engine, and confirm that the real torrent remains available and the stale mismatching record is removed. No unrelated payload files are deleted.
- Write an exact three-field journal record through the same `begin_intent` code used in production, then stop the test engine **before** issuing any `Session::delete`. Restart the engine to simulate a process exiting at the intent/deletion boundary; verify matching session-only cleanup, removal of the intent, no ghost UI row, and byte-identical payload preservation.
- Important limitation: this is a **deliberate restart at the crash boundary**, not a power-cut or forcibly killed process. Journal writes are synchronized with `File::sync_all`, but full power-loss durability depends on the storage device and OS flush semantics.

## Concurrent deletion behavior

Test: `stage22_duplicate_remove_commands_serialized_without_ghost_or_data_loss` (**PASS**, explicit `--ignored`).

- Generate/download a private 2,097,152-byte payload. Send **two immediate Remove commands** for the same torrent.
- Confirm exactly **one successful deletion and one safe rejection**. No race recreates an output row; the journal is empty, the original payload is unchanged, and a new engine instance does not resurrect the removed torrent.
- Production `spawn_engine` now has an active-removal reservation keyed by the actual session ID where available (otherwise the queue key). Different torrent IDs can continue handling independent operations without a global mutex around downloads or file I/O.

## Deletion journal correctness and concurrency

- `safe_journal_dir` handles a concurrent directory creation (`AlreadyExists`), then rechecks that the actual result is a nonsymlink directory.
- `begin_intent` is a shared, durable write helper for real managed handles and crash-boundary tests. Production calls it only through `begin`; no new public user operation was added.
- `still_persisted` now validates **ID + info hash + output folder**, rather than only ID/hash; reused IDs with an identical hash but a different output path are not conflated.
- Automatic unit tests include **16 concurrent unique fsynced deletion intents** against one disposable journal folder; all 16 records are readable and all cleanup operations succeed. Stale/incorrect filenames and corrupt JSON remain rejected.

## Automated validation

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo test --locked --all-targets --quiet` | **224 PASS / 0 FAIL / 53 ignored**, total 277 tests |
| `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| Stage 22 explicit real write fault, crash boundary, duplicate Remove E2E | **3/3 PASS** |
| Stage 21 actual bitfield-I/O failure and JSON rename race | **2/2 PASS** |
| Stage 20 genuine Windows file/JSON locks and session delete failure | **3/3 PASS** |
| Stage 19 watcher retry/duplicate retry/cancel | PASS |
| Original 4 MiB pause/restart/resume/delete | PASS |
| Read-only native XCI 2 / XCZ 2 metadata scans | **4/4 PASS** |

### Isolated deletion-access microbenchmark

- On this Windows workstation, generated **4,096 disposable 16-byte files** under `%TEMP%` and performed the same nonmutating Windows `DELETE`-access open used by Stage 20's file-deletion preflight.
- **208.52 ms total, approximately 50.91 microseconds per file.** All temporary files and benchmark executable were removed.
- This is a warm local filesystem microbenchmark, not a measurement of full torrent/file deletion, network shares, antivirus interference, SSD durability, or the cost of hashing large payloads.

## Release and emulator acceptance

- Optimized `cargo build --locked --release` **PASS** in **3m 46s**. `D:\NSW.dev\NSL\target\release\nsl.exe` is **22,282,752 bytes**, SHA-256 `CC159017885E51D4C23CE70CE7868F96AB9A59F286E5E8CA9FDE940EE80D412A` (embedded source revision `daca18d`).
- Isolated Windows GUI **PASS**: title `NSL  daca18d`, verified at **980×720, 1280×800 and 1456×900**. Private test profile removed (`GUI_NATIVE_TEMP_CLEAN=YES`).
- Protected distribution files remain unchanged: `dist\nsl.exe` SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`; `dist\nsl-win64.zip` SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Isolated Eden loader-only regression: **PASS** using the **previously merged NSZ** `E:\NSL_QA_20261120\small\[NSL Merged] [0100F7301AAB6000][v65536].nsz`, not a newly generated XCI. Checked that no other Eden/Yuzu/Ryujinx instance was active before starting a private executable and profile; loader ran for **24 seconds** in a **1296×759** GPU window with **256 sampled unique colors**, no critical errors, and unchanged NSZ SHA-256. No gameplay or menu input was attempted. Private profile and temporary key copies were cleaned.
- Evidence: `E:\NSL_QA_20261120\small\eden_loader_stage22_write_recovery.json` and matching `.png`. This does NOT prove newer unsigned synthetic XCI boot compatibility.
- Original isolated QA game-source SHA-256 recomputed after Stage 22: `base.nsz` = `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F`; `patch.nsp` = `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942`. Both match the Stage 19–21 read-only baselines exactly, and the owned Eden profile has been cleaned.

## Reproduce in an isolated Windows environment

```powershell
Set-Location 'D:\NSW.dev\NSL'
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage22_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage21_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1
cargo test --locked transfer_pause_restart_resume_delete_preserves_other_files -- --ignored --nocapture --test-threads=1
cargo test --locked native_xci_xcz_samples_from_library_database -- --ignored --nocapture --test-threads=1
cargo build --locked --release
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa-native-gui-window.ps1' -ExpectedCommit 'daca18d52839381c694213ccbe484b1db2d3b51d'
```

## Known limitations and priorities for Stage 23

- Because Windows byte-range locking and disk-full/drive-loss errors differ, run safe virtual-disk out-of-space testing only on a dedicated isolated test volume, not on `C:`, `D:` or `E:` user data. Do not modify shared Cargo registry sources to inject errors.
- Underlying librqbit restores torrents **before** NSL's journal reconciliation. There can be a short unobserved period of background torrent activity before an exact-match stale intent is removed; address this using safer upstream APIs or careful pre-start recovery after tests.
- Concurrent distinct-torrent `session.json` writes, multiple NSL processes, filesystem power loss and malicious journal tampering are not fully proven safe. Avoid deleting unrelated payload files on any recovery error.
- Large real torrents and multi-gigabyte library scans require separate sustained resource profiling; the 4,096-file access microbenchmark alone does not demonstrate full-workload performance.
- Missing updates and DLC must be matched by validated Title IDs, content metadata and version. New XCI/XCZ **emulator loader-only** acceptance requires separately authorized real correctly signed images; do not claim it from the existing synthetic unsigned XCI or previous NSZ smoke test.
