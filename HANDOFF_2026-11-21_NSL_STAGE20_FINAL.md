# NSL Stage 20 - Final handoff (2026-11-21)

## 1. Repository, baseline and artifacts

- Windows root: `D:\NSW.dev\NSL`
- Remote: `https://github.com/UzuCore/NSL.git`, branch `main`.
- Previous handoff: `HANDOFF_2026-11-21_NSL_STAGE19_FINAL.md`.
- Baseline: `3a9916a9c3608dcfd0170a71d36d9a3ac1ebbfbc`.
- **Stage 20 verified source commit:** `e33443f81d88c7165490a57344e396bc935ba69c`.
- Detailed test design and limitations: `docs\qa\QA_NSL_STAGE20_TORRENT_DELETE_RECOVERY_2026-11-21.md`.
- Uncommitted local raw test logs reside in ignored `docs\qa\*LOCAL*.txt` files; they are not distribution artifacts.

## 2. Production fixes

1. **Deletion failure atomicity at the UI/session boundary:** `src/torrent.rs` now invokes `Session::delete` without preemptively cancelling the transfer watcher. It clears the watcher, progress and handle maps **only if `Session::get(id)` confirms the torrent is no longer registered**. Failures that leave the handle registered preserve the active download, its visible row and persisted resume data.
2. **Windows file-deletion preflight:** before `delete_files=true`, enumerate only non-padding torrent metadata paths, reject unsafe path components/symlinks and probe `DELETE` access without modifying the files. If an external process denies delete sharing, show a Korean error and preserve the torrent for another attempt.
3. **Persistent-session preflight:** check `torrent-session\session.json` read-only/locked status before invoking the librqbit delete API, avoiding known failed persistence flushes and ghost session restoration.
4. **Post-delete reconciliation:** librqbit 9.1.0 can silently ignore individual file-delete errors. Check remaining torrent-owned files after API completion; report partial cleanup instead of a misleading success. If the underlying torrent was already removed, do not leave a dead download row that appears retryable.
5. Fault-injection tests are `#[cfg(test)]` and never built into the production EXE. No NSRP, merge engine, user catalog or game-loading code was changed.

## 3. Verifications

| QA | Verified result |
| --- | --- |
| NSL `cargo test --locked --all-targets --quiet` | **221 PASS / 0 FAIL / 48 ignored** (269 total) |
| Vendored NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| Stage 20 localhost E2E, three explicit ignored tests | **3 PASS / 0 FAIL** |
| Stage 19 terminal watcher fault/retry/cancel regression | PASS, **6,291,456 bytes identical** |
| Original pause/restart/resume/selective delete regression | PASS, **4,194,304 bytes identical** |
| Real read-only XCI 2 / XCZ 2 native metadata scans | **4/4 PASS** |

### 3a. Stage 20 fault scenarios

1. **Actual librqbit `Session::delete` failure (deliberately absent torrent ID, NOT real disk failure):** the active download was retained and continued, the engine was restarted, all **6,291,456 bytes** were verified, and deletion with/without output bytes worked on the correct torrent; unrelated sentinel data was unchanged.
2. **Genuine Windows file-share delete denial:** held a no-delete-sharing handle over an isolated **2,097,152-byte** downloaded file. Deletion refused early and left the session and all bytes intact. Release the handle, retry deletion, and observe complete cleanup with no stale session after restart. PASS.
3. **Genuine Windows `session.json` delete denial:** locked the isolated JSON session state during an attempted session-only delete. The API was blocked before mutation, and the session index remained byte-identical. Unlock/retry succeeded; payload remained identical; restart had no ghost torrent. PASS.

All three were tested against **generated localhost-only data** in disposable folders. Public trackers were not contacted; no user-owned games or running emulators were touched.

## 4. Release build and safety verification

- Stage 20 executable: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Embedded source commit: **`e33443f`** (documentation-only commit created after executable build).
- Size: **22,214,144 bytes**, optimized `cargo build --locked --release` **PASS** in 3m 51s.
- SHA-256: `F833BCC686F454B8D26274A31736CC01BA085254205D8146BD764DC41630E432`.
- Native Windows GUI smoke **PASS**: title `NSL  e33443f`, resized and verified at **980×720 / 1280×800 / 1456×900**, private profile cleaned (`GUI_NATIVE_TEMP_CLEAN=YES`).
- GitHub `main`: Stage 20 source + this handoff pushed together; verify the final local+remote refs with `git rev-parse HEAD` and `git ls-remote origin refs/heads/main`.
- Protected preexisting `dist\nsl.exe` SHA-256: `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Protected preexisting `dist\nsl-win64.zip` SHA-256: `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Original QA base, patch, source games, encrypted keys, saves, `E:\NSW` and non-test emulator instances were never modified.

## 5. Reproduction (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1
cargo test --locked transfer_pause_restart_resume_delete_preserves_other_files -- --ignored --nocapture --test-threads=1
cargo test --locked native_xci_xcz_samples_from_library_database -- --ignored --nocapture --test-threads=1
cargo build --locked --release
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa-native-gui-window.ps1' -ExpectedCommit 'e33443f81d88c7165490a57344e396bc935ba69c'
```

## 6. Remaining scope for Stage 21

1. **Real asynchronous librqbit fatal write error** (`ManagedTorrentState::Error`) still requires a safe, deterministic isolated fault mechanism, e.g. test-only patched dependency or controlled faulting sandbox filesystem; a one-shot missing-ID delete error is NOT proof of this condition. Never modify installed shared crate caches or user volumes for such testing.
2. **TOCTOU persistence races:** external locks or I/O failures can appear after the preflight. librqbit 9.1.0 sometimes swallows persistence flush errors even when session removal succeeds; investigate safe reconciliation without overwriting concurrent session updates.
3. Preserve original downloaded files during missing-update/missing-DLC CNMT validation, bulk merge/recompression and any queue failure. Compare exact Title IDs and versions; never infer patch/DLC content from display names.
4. Newly generated Nintendo-signed/retail-compatible XCI/XCZ emulator loading is NOT established by the synthetic gamecard tests. Stage 19 confirmed only structure and a **previously merged NSZ** Eden loader-only sample. Use isolated profiles, non-interference checks and loading only for future acceptance.
5. Revisit deletion preflight overhead and large-torrent file counts with profiling before making expensive concurrency/cache changes. Do not reduce original data safety for speed.

## Next chat instruction

`D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE20_FINAL.md` 확인하고 Stage 21을 이어가. 실제 librqbit 내부 오류 검증과 세션 기록 실패의 일관성부터 해결하되, 원본 파일 및 기존 에뮬레이터 인스턴스는 보호해.
