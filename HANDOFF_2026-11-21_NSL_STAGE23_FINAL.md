# NSL Stage 23 - final handoff (2026-11-21)

## 1. Baseline, repository and code

- Root: `D:\NSW.dev\NSL`.
- Remote: `https://github.com/UzuCore/NSL.git`, branch `main`.
- Stage 22 baseline final main: `0dbb3cb164cda3ec592e3c97b54aba31e7a811a2`.
- Stage 22 handoff: `HANDOFF_2026-11-21_NSL_STAGE22_FINAL.md`.
- **Stage 23 verified implementation commit: `2a9aea68432812b3b532399e11dcccd1261f96a4`.**
- Stage 23 final GitHub main is synchronized after committing this handoff; verify `git rev-parse HEAD` equals `git ls-remote origin refs/heads/main` and `git status --porcelain` is empty.
- Detailed QA and test limitations: `docs\qa\QA_NSL_STAGE23_CROSS_PROCESS_PREBOOT_RECOVERY_2026-11-21.md`.
- Raw local logs under ignored `docs\qa\QA_NSL_STAGE23_*LOCAL_2026-11-21.txt`.

## 2. Production improvements

### Cross-process session ownership

- Added `src/torrent_session_lock.rs`, an `fs2` OS-level file lock on the stable `.nsl-torrent-session.lock` file next to (not inside) the session directory. Windows lock handle denies deletion of the lock file while owned.
- `src/torrent.rs` acquires the lock before calling `librqbit::Session::new_with_opts` and holds it until the torrent runtime exits. A second NSL process using the same profile gets an actionable Korean warning instead of racing updates to the same session database.
- `TorrentEngine::clear_session_state` now acquires the exact same lock; an active external NSL instance blocks Settings from deleting its session metadata. After that instance exits, clearing succeeds without touching downloaded payload data.
- Uses a bounded three-second retry on competing lock ownership; never kills unrelated processes or removes their files.

### Pre-restore journal cleanup and fail-closed startup

- `src/torrent_removal.rs`: added `reconcile_before_restore` to inspect durable deletion intents **before the torrent library can restore a deleted download or contact peers**.
- Only serial entries matching the exact session ID, content info hash and output directory are removed. Other serialized torrents and extra JSON fields are preserved.
- Commit changes via a unique same-directory temporary JSON file, flush, `sync_all`, read-back check against concurrent external edits, then atomic rename. Keep the deletion record until absence is confirmed. No payload files are deleted by this reconciliation.
- Malformed journals, corrupt JSON, filesystem lock/rename denial and unexpected concurrent edits cause a Korean startup error **without starting the underlying torrent session**. User files are preserved for manual intervention/retry.
- Prior Stage 21 fallback post-restore reconciliation remains as a defensive secondary check.

## 3. Verified tests and data protection

| Test | Result |
| --- | --- |
| NSL `cargo test --locked --all-targets --quiet` | **227 PASS, 0 FAIL, 56 ignored** (283 total) |
| Vendor NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS, 0 FAIL, 3 ignored** |
| Rust fmt / Clippy `-D warnings` | PASS / PASS |
| Stage 23 separate child process owns session, second process denied | **PASS**; 4,194,304-byte completed payload exact |
| Stage 23 forcibly killed owned child, recovered persistent session | **PASS**; 5,242,880-byte completed payload exact |
| Stage 23 pre-restore JSON exact matching/unrelated-entry preservation | PASS |
| Stage 23 actual Windows denied JSON rename leaves source unchanged | PASS |
| Stage 23 corrupted journal prevents engine startup | PASS |
| Stage 22 true payload write OS error 33, crash boundary, duplicate removal | **3/3 PASS** |
| Stage 21 true bitfield I/O fault and real JSON rename failure | **2/2 PASS** |
| Stage 20 failed delete, locked file, locked `session.json` | **3/3 PASS** |
| Stage 19 watcher fault and Retry/Cancel | PASS |
| Original 4MiB pause/restart/resume/deletion | PASS |
| Stage 23 larger 16MiB peer outage, exact resume | **PASS**; 16,777,216 bytes exact, 39.33s observed transfer/recovery |
| Native read-only XCI 2 + XCZ 2 metadata | **4/4 PASS** |
| Isolated Eden existing merged NSZ GPU loading only | PASS, 24s, 1296×759, 256 sampled colors, no critical errors |

### Protected sources

- Original QA `E:\NSL_QA_20261120\small\base.nsz` SHA-256 remains `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F`.
- Original QA `E:\NSL_QA_20261120\small\patch.nsp` SHA-256 remains `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942`.
- Eden evidence `E:\NSL_QA_20261120\small\eden_loader_stage23_preboot.json` and corresponding PNG. Only the owned emulator child was terminated, with its private test profile and copied keys cleared; no gameplay or menu interaction. This is not proof that a newly generated unsigned XCI boots.
- No original library `E:\NSW`, user keys, saves, unrelated emulator sessions, or protected preexisting distributions were changed.

## 4. Release, GUI and publishing

- Stage 23 production EXE: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Optimized `cargo build --locked --release`: **PASS**, **3m 36s**.
- EXE length: **22,301,696 bytes**.
- EXE SHA-256: `D08DD7B82C2E0E0B6BC20192842CA092E55666A78FA2339420CC125B9D2E29B4`.
- Native Windows GUI: **PASS**, embedded title `NSL  2a9aea6`, all **980×720 / 1280×800 / 1456×900** sizes verified; owned temporary test profile removed.
- Protected `dist\nsl.exe` SHA-256 (Stage 22 baseline): `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Protected `dist\nsl-win64.zip` SHA-256 (Stage 22 baseline): `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- `dist` EXE and ZIP are not overwritten merely by the Stage 23 isolated release build.

## 5. Reproduction commands in PowerShell

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage23_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage23_preboot_ -- --nocapture --test-threads=1
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

## 6. Explicit limitations and Stage 24 priorities

1. **True out-of-space Windows error 112 is not yet tested.** Windows session is non-elevated, and dedicated quota-limited VHD mount permissions were not verified; do not attempt to fill a shared `C:`, `D:`, `E:` or other user drive. Use a separately authorized dedicated test volume. Stage 22 demonstrated OS error 33 in the true write path, NOT storage exhaustion.
2. **Power failure, cross-process non-NSL writers and SMB/network filesystem faults remain unproven.** The Stage 23 forced shutdown test kills only an explicitly owned child process in a private fixture. JSON `sync_all` and atomic rename cannot guarantee physical power-loss durability or defeat arbitrary hostile writers.
   - Prior Stage 22 and earlier NSL executables do not implement the new cooperative session lock; never use those builds and Stage 23 together on one profile.
3. **Strict missing-update/DLC CNMT identity and version matching** still requires separate queued merge/compression E2E tests, with source packages protected and cancellation atomic.
4. **Multi-gigabyte sustained download/memory profiling**, large numbers of files, and many parallel independent downloads need real workload measurements. An isolated 16MiB peer outage smoke is not a multi-GB performance certification.
5. **New XCI/XCZ emulator load acceptance** requires properly signed/authorized retail-compatible sample images. The previously merged NSZ loader-only smoke is not sufficient evidence.

## Next chat instruction

`D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE23_FINAL.md` 기준 Stage 24를 이어가. 먼저 전용 격리 디스크에서 실제 용량 부족/SMB I/O 실패를 검증하고, 미보유 업데이트·DLC의 Title ID/버전/CNMT 매칭 정확성과 대용량 다운로드 성능을 개선해. 다른 에뮬레이터 인스턴스와 원본 게임·키·세이브는 변경하지 마.
