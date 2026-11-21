# NSL Stage 19 - Final handoff (2026-11-21)

## 1. Baseline and version

- Root: `D:\NSW.dev\NSL`
- Git remote: `https://github.com/UzuCore/NSL.git`, branch `main`.
- Stage 18 final handoff: `HANDOFF_2026-11-21_NSL_STAGE18_FINAL.md`.
- Baseline Stage 18 handoff commit: `d9a76eec3f4f6d781a5774132be61e28c8557d23`.
- **Stage 19 tested source commit:** `717d847d8832868074e108506846ef6dcf513890`.
- Detailed analysis and reproducibility: `docs\qa\QA_NSL_STAGE19_RETRY_CANCEL_XCI_2026-11-21.md`.
- Text log files under `docs\qa\*LOCAL*.txt` are local-only; `docs` is excluded by `.gitignore` unless explicitly force-added for report artifacts.

## 2. Production changes

- `src/torrent.rs`: post-cancellation progress updates now check the cancel flag under the existing mutex, preventing a row deleted by the user from being resurrected by a late watcher update.
- `src/torrent.rs`: terminal-transfer recovery checks cancellation both before and after asynchronous unpause; only the same retry reservation token may be removed during cancellation.
- `src/app/frame.rs`: torrent command failure statuses are readable Korean. A successfully deduplicated rapid retry no longer overwrites the successful operation's status with a false failure message.
- `src/torrent.rs` and `src/torrent_e2e_tests.rs`: **test-only** terminal watcher fault injection, embedded `#[cfg(test)]`. Release never injects errors.
- No changes to NSRP source, core merge selection, production keys, local emulator profiles or packaged user library.

## 3. Full validation

| Test | Result |
| --- | --- |
| `cargo test --locked --all-targets --quiet` | **221 PASS / 0 FAIL / 45 ignored**, 266 cases |
| `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| Stage 19 6 MiB loopback retry & immediate cancel | PASS twice, final exact bytes |
| Rapid double retry | 1 accepted, 1 safely rejected |
| Retry+cancel worker race | No queue-row resurrection, prior download intact |
| Isolated real NSZ+NSP -> direct XCI | PASS, no intermediate merged NSP |
| Read-only XCI 2 / XCZ 2 native metadata scans | PASS |
| Prior merged Horror Tale 2 NSZ isolated Eden GPU loading only | **PASS**, 24 s, no gameplay |

### 3a. Real generated torrent transfer

- Dedicated loopback tracker/peer; no public BitTorrent activity or commercial payloads.
- Test fixture: 6,291,456 bytes, real partial transfer before test-injected terminal NSL watcher failure.
- Real torrent is paused, retained piece data is subsequently reused, and transfer completes byte-for-byte from local seeder.
- On two immediate retry requests, exactly one is accepted and the other safely rejected; the user-visible log does not falsely mark the recovery as failed.
- A new partially transferred job is faulted, retried, and immediately cancelled. It does not reappear; the first completed output is byte-identical after download-session removal.
- **Scope limitation:** this is *not* a real fatal `librqbit::ManagedTorrentState::Error` or device I/O error. The installed librqbit 9.1.0 does not expose the private storage types needed to inject a failing custom storage backend through the public API. That remains unverified.

### 3b. Isolated genuine package QA

- Read-only source folder: `E:\NSL_QA_20261120\small`.
- `base.nsz` original SHA-256 before and after: `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F`.
- `patch.nsp` original SHA-256 before and after: `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942`.
- Native CNMT selected **10** NCA and associated files; selected content **745,610,688 bytes**; generated XCI **745,615,360 bytes**. No intermediate merged NSP.
- Direct selected-file builder elapsed **68.113 s**; entire isolated QA test **123.24 s**. Test workspace deleted automatically.
- A separate read-only native scanner QA selected two small XCI and two XCZ from the existing local database, checking base metadata, Title IDs and localized names; all four passed.
- **Do not confuse unsigned synthetic-template XCI validation with Nintendo retail signatures or successful emulator boot.** Game execution of a newly generated retail-compatible image remains a separate verification.
- Independent Eden loader-only verification of the **previously merged** NSZ passed with an isolated portable profile (not the unsigned XCI): **24 s**, GPU window **1296×759**, **261 distinct sampled colors**, no NCA header error, **0 critical errors**, existing source SHA-256 unchanged. No menu interactions or gameplay. QA-owned process was stopped and copied keys/profile removed. Evidence: `E:\NSL_QA_20261120\small\eden_loader_stage19_horrortale_boot.json` and matching PNG. User emulator profiles and other processes remained untouched.

## 4. Release artifacts and source preservation

- Stage 19 executable: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Embedded source commit: `717d847` (build before final documentation-only commit).
- Release SHA-256: `F47433767F63E59C588AB96C1428A836F03135317477ABD89AF90C29D2B6C47D`.
- Release byte size: **22,201,344 bytes**, optimized `cargo build --locked --release` completed successfully in 3m 51s.
- Isolated native Windows GUI: **PASS**. Window title `NSL  717d847`; window responsiveness and resizing at **980x720**, **1280x800**, **1456x900**; test-only portable profile deleted (`GUI_NATIVE_TEMP_CLEAN=YES`).
- GitHub `main` final source + handoff push: source and this handoff included in the final `main` sync; verify with `git ls-remote origin refs/heads/main` and compare to `git rev-parse HEAD`.
- Preserve existing `dist\nsl.exe` SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Preserve existing `dist\nsl-win64.zip` SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Do not delete `E:\NSW`, prod.keys, title-key files, saves, configured scans, user data or other emulator instances.

## 5. Reproduce safely (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo test --locked -p nsrp-core --lib
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1
$env:NSL_QA_ROOT  = 'E:\NSL_QA_20261120\small'
$env:NSL_QA_BASE  = 'E:\NSL_QA_20261120\small\base.nsz'
$env:NSL_QA_PATCH = 'E:\NSL_QA_20261120\small\patch.nsp'
cargo test --locked isolated_real_nsz_patch_to_xci_without_intermediate_nsp -- --ignored --nocapture --test-threads=1
cargo test --locked native_xci_xcz_samples_from_library_database -- --ignored --nocapture --test-threads=1
cargo build --locked --release
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
powershell -ExecutionPolicy Bypass -File '.\scripts\qa-native-gui-window.ps1' -ExpectedCommit '717d847d8832868074e108506846ef6dcf513890'
```

## 6. Stage 20 remaining priorities

1. Exercise **real librqbit fatal storage failure** via a safely supported/testable fault boundary, without exposing private types in the production dependency or touching user files. The Stage 19 watcher fault is not sufficient proof of disk-I/O recovery.
2. Verify delete-session failures preserve recoverability and progress state. A failed `session.delete` while a watcher is already cancelled is not covered by a deterministic integration test.
3. Continue authorized, isolated XCI/XCZ boot compatibility verification in an emulator. Only check ability to launch/loading; do not play through. Never interfere with other user emulator instances, profiles, keys or save data.
4. Profile large libraries and sustained transfer workloads for UI and memory regressions; do not assume small loopback samples represent every torrent or every remote peer.
5. Keep strict CNMT version/Title ID verification for missing updates/DLC and preserve original NSP/NSZ/XCI/XCZ before any optional merge/recompression.

## Next chat instruction

`D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE19_FINAL.md` 확인하고 Stage 20 작업 이어가. 기존 데이터와 에뮬레이터 인스턴스는 보호하고, 실제 librqbit 오류·취소 실패 원자성 검증부터 진행해.
