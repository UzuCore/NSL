# NSL Stage 24 — final handoff (2026-11-21)

## 1. Repository and version

- Windows project: `D:\NSW.dev\NSL`.
- GitHub remote: `https://github.com/UzuCore/NSL.git`, branch `main`.
- Previous Stage 23 final main: `9ab825fa0786019e3964020a5ef21522770ae741`.
- Stage 23 reference: `HANDOFF_2026-11-21_NSL_STAGE23_FINAL.md`.
- **Stage 24 verified source commit: `d5f1292ed4d933147d7306da0ae56065695712e3`.**
- The final documentation-only GitHub commit does not alter the embedded source revision. Confirm that `git rev-parse HEAD` matches `git ls-remote origin refs/heads/main` and that `git status --porcelain` is empty after the final push.
- Detailed QA and limitations: `docs\qa\QA_NSL_STAGE24_REAL_QUOTA_CNMT_CAPACITY_2026-11-21.md`.
- Raw local ignored logs: `docs\qa\QA_NSL_STAGE24_*LOCAL_2026-11-21.txt`.

## 2. Production functionality improved

### Precise missing-update / DLC candidate selection

- `src/update_batch.rs`: one exact 16-digit hexadecimal Title ID and no ambiguous second `[TitleID]` per candidate filename. Reject multiple versions, nonnumeric/negative/overflow `[v...]`, zero-sized files, traversal paths and unsupported extensions before scheduling NSP/NSZ downloads.
- Patch candidates must have a valid base-game ID, positive patch version, strictly newer than installed and at least the required title DB version. DLC candidates must use the exact DLC ID; missing version tokens are interpreted as *candidate* v0 only. Both still undergo **independent trusted native/header CNMT validation after downloading**; filenames/catalog text never suffice to publish a merge.
- Corrupt or legacy restored patch jobs with non-base IDs or versions not newer than installed are rejected before the downloaded file is opened. Unknown/ambiguous catalog names fail closed rather than guessing.
- Missing-update exclusions now recognize comma, newline and semicolon separators, matching the DLC exclusion behavior.

### Capacity-aware batch queue

- `src/app/update_batch.rs` measures available bytes on the actual `downloads` drive, sums remaining in-flight torrent data and restored pending job sizes, then reserves selected file sizes for all accepted batch groups cumulatively. It enforces a 64 MiB headroom minimum and checked overflow handling.
- If capacity is inadequate or the disk cannot be queried, the queue refuses the affected download(s), displays Korean storage-shortage information and **does not create a download folder or alter original game files**.
- Actual concurrency with other programs may change free bytes after acceptance; this admission check complements the already-tested real I/O failure and Retry handling, not a guaranteed NTFS disk reservation.

### O(1) metadata-cache accounting

- `UpdateBatch.preview_cache_bytes` replaces repeatedly re-summing the entire `preview_cache` for each new candidate, avoiding quadratic bookkeeping in large catalog scans. It preserves the 128 MiB maximum, Arc-sharing, one-preview-per-frame asynchronous audit, and restored batch compatibility.
- `src/update_manifest.rs`, `src/runtime_tests.rs`, and the isolated E2E fixture initialize the new runtime-only counter consistently.

### Genuine filesystem capacity exhaustion test

- `scripts\qa_stage24_quota_e2e.ps1` creates an isolated disposable WSL Ubuntu **4 MiB tmpfs**, then invokes `stage24_real_quota_exhaustion_download_retry_exact_bytes` with the real Windows librqbit payload writer. It never fills C:, D:, E: or another user drive.
- After **3,964,928 bytes** of real downloaded payload, writing failed due to the exhausted volume. Windows exposes this Linux/WSL failure as **OS error 31**, **not Windows disk-full error 112**. This distinction is important; no test-injected watcher fault was used.
- Resizing only the owned tmpfs to 48 MiB and pressing the normal NSL Retry successfully completed **12,582,912 bytes**, verified byte-identical to the generated original seeder bytes. The test-specific torrent session/lock lives in its own NTFS temp folder because the WSL UNC layer does not implement Windows file locking.
- After the test, the WSL mount was unmounted and the private temporary directory removed. No other WSL mounts or processes were changed.

## 3. Verification results

| Validation | Result |
| --- | --- |
| NSL `cargo test --locked --all-targets --quiet` | **231 PASS / 0 FAIL / 57 ignored**, 288 total |
| NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| Rust code fmt and Clippy `-D warnings` | **PASS / PASS** |
| Stage 24 genuine WSL tmpfs exhausted-volume write + Retry | **PASS**, 12,582,912 bytes exact |
| Stage 24 insufficient-capacity batch queue integration | **PASS**: 32 MiB candidate rejected with 10 MiB available; original untouched, no output folder |
| Stage 24 filename ambiguity and version-validation unit tests | PASS |
| Stage 23 cross-process lock and abrupt owned-child crash regressions | **PASS** |
| Stage 22 actual Windows payload I/O fault / duplicate removal | **3/3 PASS** |
| Stage 21 real bitfield read and late JSON write failures | **2/2 PASS** |
| Stage 20 real locked files and failed session deletion | **3/3 PASS** |
| Stage 19 cancel/retry and 4 MiB restart | **PASS / PASS** |
| 16 MiB local seeder interruption and restored transfer | **PASS**, 16,777,216 bytes exact, 31.81s observed |
| Read-only XCI 2 / XCZ 2 metadata scans | **4/4 PASS** |
| Isolated Eden existing merged NSZ GPU loader-only | **PASS**; 24s, 1296×759 GPU, 258 sampled colors |

## 4. Release, GUI and protected originals

- Stage 24 EXE location: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Optimized `cargo build --locked --release`: **PASS**, **3m 51s**.
- EXE length: **22,310,912 bytes**.
- EXE SHA-256: `BC9ADB56A253B6F54402E94DBBF97D21D5B4165D4314774384773EDAC804CAE8`.
- Native Windows GUI 980×720 / 1280×800 / 1456×900: **3/3 PASS**, title `NSL  d5f1292`; private GUI test profile removed (`GUI_NATIVE_TEMP_CLEAN=YES`).
- Unchanged protected `dist\nsl.exe` baseline SHA256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Unchanged protected `dist\nsl-win64.zip` baseline SHA256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Original game archives, user saves, keys and running emulator instances are never altered. The Eden test, when run, uses a private profile and checks real GPU loading only; it does not demonstrate gameplay or newly signed XCI/XCZ compatibility.
- Verified original QA SHA256: `base.nsz` = `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F`; `patch.nsp` = `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942` (both unchanged).
- Isolated Eden evidence: `E:\NSL_QA_20261120\small\eden_loader_stage24_quota_catalog.{json,png}`. It reported `NSL_EDEN_LOADER_ONLY_PASS` at 24s and 258 colors, no critical loader errors. All test-owned emulator profiles and isolated WSL quota mounts have been removed.

## 5. Reproduce on Windows (opt-in WSL quota test)

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage24_ -- --nocapture --test-threads=1
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa_stage24_quota_e2e.ps1'
cargo test --locked stage23_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage22_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage21_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1
cargo test --locked transfer_pause_restart_resume_delete_preserves_other_files -- --ignored --nocapture --test-threads=1
cargo test --locked larger_transfer_survives_local_seeder_outage_and_restores_exact_bytes -- --ignored --nocapture --test-threads=1
cargo test --locked native_xci_xcz_samples_from_library_database -- --ignored --nocapture --test-threads=1
cargo build --locked --release
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
```

The quota script requires WSL with Ubuntu and the ability to mount a **new, uniquely named** temporary `/tmp/nsl-stage24-quota-...` tmpfs as root inside Ubuntu. It unmounts its own mount in a `finally` block; it must not be adapted to fill real user disks.

## 6. Stage 25 next priorities and explicit limitations

1. On a separately authorized NTFS test VHD or quota volume, verify actual Windows **`ERROR_DISK_FULL` 112**, including errors during session metadata flush and output cleanup. Stage 24 achieved real ENOSPC on WSL tmpfs, but the UNC bridge returned **error 31**.
2. Test SMB disconnect/reconnect and network-drive free-space reporting using only isolated shares. The Stage 24 quota test is not a full SMB reliability certification.
3. Broader independent verified CNMT Title ID/version fixtures, multiple updates and DLCs for one base, cancellation/queue concurrency, recompression, and preservation of all source packages.
4. Sustained multi-gigabyte throughput and memory/heap profiling across many active torrents; O(1) cache accounting does not by itself prove a whole-app speedup.
5. Signed and authorized **new** XCI/XCZ images for *loader-only* acceptance; do not claim this from the previously merged NSZ GPU smoke.

## Instruction for next chat

`D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE24_FINAL.md`를 기준으로 Stage 25를 이어가. 전용 격리 NTFS 볼륨의 Windows 오류 112와 SMB I/O 오류, 미보유 DLC/업데이트 CNMT의 엄격한 매칭 및 대용량 다운로드 최적화를 우선하되 원본 파일·키·설정과 다른 에뮬레이터 프로세스는 건드리지 마.
