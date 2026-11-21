# NSL Stage 22 — final handoff (2026-11-21)

## 1. Repository baseline and source commits

- Windows root: `D:\NSW.dev\NSL`.
- Remote: `https://github.com/UzuCore/NSL.git`, branch `main`.
- Starting Stage 21 documentation commit: `1544179e6ab26b12ec7aabe2c7c1ff426cd4ac3d`.
- **Stage 22 verified implementation commit: `daca18d52839381c694213ccbe484b1db2d3b51d`.**
- Stage 21 reference: `HANDOFF_2026-11-21_NSL_STAGE21_FINAL.md`.
- Detailed Stage 22 test evidence and limits: `docs\qa\QA_NSL_STAGE22_PWRITE_CRASH_CONCURRENCY_2026-11-21.md`.
- Raw local logs: `docs\qa\QA_NSL_STAGE22_*_LOCAL_2026-11-21.txt` (ignored, not shipped).

## 2. Production changes and root causes

### Confirmed real downloaded-file write error

- **True Windows OS error 33 in librqbit's live payload `FileOps::write_chunk` / `pwrite_all` path**, not a fake watcher error. The E2E acquires a real exclusive Windows byte-range lock on a disposable torrent output while pieces are arriving. The internal fatal errors receiver transitions the engine into an actual failed state.
- After releasing the lock, the normal NSL Retry returns success, revalidates previously downloaded data and completes all **8,388,608** bytes byte-for-byte. The resulting file survives session-only removal intact.
- The installed `fs2` API performs the lock only in a `#[cfg(windows)]` test. **No production write interception, dummy storage, Cargo registry patch, or user-data modification** was needed. The existing recovery logic handles this true underlying fatal write error.

### Same-torrent Remove command serialization

- `src/torrent.rs`: active-removal reservation keyed by underlying session ID when available, otherwise by the queue key, implemented with a small mutex-protected `HashSet<String>`.
- Two immediate Remove commands cannot simultaneously write the same journal or delete the same session/output. Exactly one succeeds and the duplicate receives a Korean rejection. The reservation is released after the progress/handle cleanup, reducing late concurrent removals.
- Different torrent identities are not globally blocked from processing unrelated user operations.

### Journal crash and identity hardening

- `src/torrent_removal.rs`: factored a testable `begin_intent` operation from `begin`, retaining atomic temporary-file serialization and `File::sync_all`.
- `safe_journal_dir` now accepts a legitimate concurrent directory-creation `AlreadyExists` race only after a second type/symlink check.
- `still_persisted` confirms **all three fields** (session ID, info hash, output path). This avoids treating an unrelated same-ID/same-hash torrent in another output directory as the pending deleted entry.
- A dedicated E2E simulates the restart boundary with a durable journal already written but the session not yet deleted; it verifies that a mismatched folder stays untouched while an exact match is removed at startup without touching the 2 MiB payload.
- Unit tests verify 16 threads can create 16 separate durable journal records concurrently and leave no dangling `.tmp` files.

## 3. Verification summary

| Validation | Verified result |
| --- | --- |
| NSL `cargo test --locked --all-targets --quiet` | **224 PASS / 0 FAIL / 53 ignored** (277 total) |
| Vendored NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| Rust formatting / Clippy `-D warnings` | **PASS / PASS** |
| Stage 22 opt-in localhost E2E | **3/3 PASS** |
| Actual live Windows file write fault | **PASS**, OS error 33, 8,388,608 bytes exact after Retry |
| Durable journal before session delete, changed folder guard | **PASS**, 2,097,152 bytes exact |
| Concurrent duplicate Remove (one accepted, one safely rejected) | **PASS**, 2,097,152 bytes preserved |
| Stage 21 true resume bitfield error + JSON rename race | **2/2 PASS** |
| Stage 20 deletion failure + file lock + session JSON lock | **3/3 PASS** |
| Stage 19 terminal watcher fault + duplicate Retry/Cancel | PASS |
| Original 4 MiB pause/restart/resume/remove | PASS |
| Read-only real XCI 2 / XCZ 2 metadata scans | **4/4 PASS** |
| Concurrent 16 independent journal unit test | **PASS** |
| Isolated Eden GPU NSZ loader-only regression | **PASS**, 24 s, window 1296×759, 256 sampled colors |

### Local-file overhead sanity check

- Created 4,096 disposable 16-byte files under Windows `%TEMP%`, then opened each with the same `DELETE`-access preflight operation used in production. **208.52ms total / 50.91µs per file** on this local workstation; all generated files and benchmark EXE removed.
- This is **not** a full torrent deletion benchmark nor evidence of network-share, mechanical drive or antivirus performance.

## 4. Release and source protection

- Stage 22 EXE: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Embedded source hash: **`daca18d`**, not the subsequent documentation-only final commit.
- `cargo build --locked --release`: **PASS**, **3m 46s**.
- Output size: **22,282,752 bytes**.
- Output SHA-256: `CC159017885E51D4C23CE70CE7868F96AB9A59F286E5E8CA9FDE940EE80D412A`.
- Native Windows GUI resize smoke: **PASS**, title `NSL  daca18d`, 980×720 / 1280×800 / 1456×900, private test profile removed.
- GitHub `main` is updated with Stage 22 implementation and this final documentation; confirm `git rev-parse HEAD` equals `git ls-remote origin refs/heads/main` and `git status --porcelain` is empty after push.
- Protected preexisting `dist\nsl.exe` SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Protected preexisting `dist\nsl-win64.zip` SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Original QA files, games, keys, saves and unrelated emulator instances are never modified. Separate NSZ loader-only evidence lives under `E:\NSL_QA_20261120\small\eden_loader_stage22_write_recovery.{json,png}`; private emulator profiles are cleaned.

## 5. Reproduction (isolated PowerShell session)

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
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

## 6. Remaining priorities for Stage 23

1. **Out-of-space, failed-volume and SMB write behavior** using a dedicated disposable virtual disk or isolated share. Stage 22 proves a genuine Windows byte-range lock error in live `pwrite_all`, but not every possible write failure.
2. **Power-loss and cross-process persistence consistency.** A restarted engine recovers a journaled deletion, but an actual forced kill or a concurrent separate NSL process was not tested. `librqbit` can start restored torrents before NSL reconciles journal records; investigate a safe pre-start suppression strategy or upstream session APIs.
3. **Large-library profiling**, many-file deletes and long-duration transfers. Do not extrapolate the 4,096 tiny-file access microbenchmark to large games.
4. **CNMT strict identity/version checks** for missing updates and DLC, including cancellation and source-package preservation in batch merge/compression.
5. **New retail-compatible XCI/XCZ loader-only compatibility checks** when a legal correctly signed isolated test source is available. The previously merged NSZ Eden smoke test does not prove the newer unsigned synthetic XCI is bootable.

## Next chat instruction

`D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE22_FINAL.md` 확인 후 Stage 23을 이어가. 별도 격리 디스크에서 디스크 부족·I/O 쓰기 오류 검증과 두 프로세스 간 삭제 저널 안전성을 우선 강화해. 원본 게임과 키/세이브를 보호하고 기존 에뮬레이터 인스턴스는 건드리지 마.
