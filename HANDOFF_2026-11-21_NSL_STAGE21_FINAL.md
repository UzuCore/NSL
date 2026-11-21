# NSL Stage 21 - Final handoff (2026-11-21)

## 1. Baseline, source and QA

- Repo root: `D:\NSW.dev\NSL`.
- GitHub: `https://github.com/UzuCore/NSL.git` (`main`).
- Stage 20 final handoff: `HANDOFF_2026-11-21_NSL_STAGE20_FINAL.md`.
- Starting main commit: `6e7113f9ed45ff5695746ea958cd4df6d73da3c9`.
- **Stage 21 verified source commit: `88a4d414bca4a2a08d1fd790e226a017cb9d2106`.**
- Detailed QA and exact limitations: `docs\qa\QA_NSL_STAGE21_REAL_STORAGE_AND_SESSION_JOURNAL_2026-11-21.md`.
- Local raw logs are kept under ignored `docs\qa\QA_NSL_STAGE21_*_LOCAL_2026-11-21.txt`; generated executable is in `target\release`.

## 2. Production changes

### Crash-safe deletion journal

- Created `src/torrent_removal.rs`, a standalone deletion-intent module using atomic journal file writes, a sync-to-disk step and exact identity matching.
- Before `Session::delete`, NSL durably records the **session ID, info hash and output folder** under `torrent-session\nsl-deletion-intents\`. If the journal write cannot succeed, deletion is refused rather than risking a stale session. Unsafe or symlinked journal folders are rejected.
- After deletion, NSL checks the true `Session::get(id)` result **and** the corresponding serialized ID/hash in `session.json`. If the original entry remains on disk even though the session no longer owns it, the journal is retained, and a Korean warning is reported rather than false success.
- On the next app start, before adopting restored torrents into UI watchers, NSL matches exactly the journal ID + info hash + output folder and calls the real librqbit **session-only delete**. It never deletes downloaded payload data during repair. If a numeric ID was reused for a different torrent, that unrelated torrent is protected.
- If reconciliation is still blocked, NSL retains the record, attempts to pause the matching download and emits an error notice, rather than silently resuming a torrent the user explicitly deleted.
- `src/torrent.rs` owns the integration. Test-only late persistence fault and test-specific fastresume fixture are compiled only under `#[cfg(test)]`; production still uses its existing default full integrity check, not fastresume.

### Genuine librqbit storage error coverage

- New Windows localhost-only E2E locks a disposable verified-piece bitfield `*.bitv` while the restored session performs its asynchronous integrity check.
- This produces an **actual `librqbit::ManagedTorrentState::Error`**, not a fake NSL watcher error. After unlocking, the normal Retry action completes the 6,291,456-byte payload **byte-for-byte** and allows session removal without deleting the payload.
- The test-specific path enables fastresume for that disposable fixture only so the bitfield read code runs; no production fastresume behavior was changed.
- **Important limitation:** this is a real *resume metadata I/O read error*, NOT a fatal `pwrite_all` disk-write failure. A dedicated isolated failing storage backend would be required to exercise the latter.

## 3. Tests and safety

| Check | Result |
| --- | --- |
| NSL `cargo test --locked --all-targets --quiet` | **223 PASS / 0 FAIL / 50 ignored** (273 total) |
| NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| Stage 21 genuine librqbit bitfield I/O error / Retry | **PASS**, 6 MiB exact |
| Stage 21 late Windows `session.json` rename failure / journaled reboot cleanup | **PASS**, 2 MiB exact, no ghost |
| Stage 20 Windows file lock / JSON lock / real missing-ID delete error | **3/3 PASS** |
| Stage 19 fault/retry/duplicate/cancel | PASS |
| Original pause/restart/resume/delete | PASS |
| Actual read-only metadata: XCI 2 + XCZ 2 | **4/4 PASS** |
| Removal-intent unit tests: reused ID vs hash; malformed data | **2 PASS** |
| Rust fmt / Clippy warnings denied | PASS / PASS |
| Eden separate GPU loader-only regression (previously merged NSZ) | **PASS**, 24 s, window 1296×759, 256 colors |

### Isolated Eden loading

- Sample (read-only): `E:\NSL_QA_20261120\small\[NSL Merged] [0100F7301AAB6000][v65536].nsz`.
- Used private Eden executable/profile/temporary key copies; checked other emulator process names were absent first. QA script safely stopped only its own child.
- 24-second GPU window, renderer and loader log evidence, no NCA bad header, no critical errors, unchanged source SHA-256. **No game/menu interaction or gameplay.**
- Evidence: `E:\NSL_QA_20261120\small\eden_loader_stage21_session_recovery.json` and matching PNG.
- This does **not** certify the synthetic/unsigned newly generated XCI as bootable or retail-signed.

## 4. Release and data protection

- Windows EXE: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Release source embedded: `88a4d41` (before the final docs-only commit).
- Optimized `cargo build --locked --release`: **PASS**, **3m 36s**.
- Size: **22,267,904 bytes**.
- SHA-256: `25C9ECD21FE253178D09359F1DCFB5C9A1426526BD5D03F7E785AE01DDABB6B9`.
- Three-size native GUI smoke: **PASS**; title `NSL  88a4d41`, resizing **980×720 / 1280×800 / 1456×900**; private test profile removed.
- GitHub `main` source and handoff are synchronized in the final documentation-only commit; check `git rev-parse HEAD` and `git ls-remote origin refs/heads/main` to confirm. Git worktree is clean after final push.
- Protected `dist\nsl.exe` SHA-256: `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Protected `dist\nsl-win64.zip` SHA-256: `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Never rewrite the existing `dist` artifacts solely as part of this isolated release build. No original game library, production key, user save, installed emulator profile or unrelated emulator instance was altered by Stage 21.

## 5. Reproduction commands (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage21_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1
cargo test --locked transfer_pause_restart_resume_delete_preserves_other_files -- --ignored --nocapture --test-threads=1
cargo test --locked native_xci_xcz_samples_from_library_database -- --ignored --nocapture --test-threads=1
cargo build --locked --release
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa-native-gui-window.ps1' -ExpectedCommit '88a4d414bca4a2a08d1fd790e226a017cb9d2106'
```

## 6. Stage 22 priorities and known limitations

1. Truly fatal **live payload disk write (`pwrite_all`)** failure: test only with a dedicated disposable/faultable volume or isolated test-only dependency patch; real bitfield load faults and fake watcher faults do not cover it. Do not touch shared Cargo caches or live customer/game volumes.
2. Persistence crash/concurrency hardening: test app termination after journal creation but before deletion, concurrent re-add of the same title/hash, and filesystem rename contention across multiple active torrents. librqbit restores persisted torrents before NSL's reconciliation; a short startup window may still perform background I/O.
3. Large-collection responsiveness and memory: profile long downloads and many-file deletion/preflight scenarios without increasing memory or risking user data.
4. Explicit version/Title ID matching for missing updates and DLC in bulk CNMT decisions; never infer DLC ownership or patch matches from game names. Preserve source packages throughout merge/recompression and cancellation.
5. Verify retail-compatible, correctly signed newly generated XCI/XCZ **loading only** in an isolated Eden instance if legitimate test images and keys are available. The previous NSZ smoke test does not replace that separate acceptance check.

## Next chat instruction

`D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE21_FINAL.md` 확인 후 Stage 22를 이어가. 먼저 실제 저장 파일 쓰기 오류 E2E와 삭제 저널 동시성/크래시 복구를 강화하고, 이후 성능과 CNMT 검증을 진행해. 기존 게임·키·세이브·에뮬레이터 인스턴스는 보호해.
