# NSL Stage 21 - Genuine librqbit error recovery and durable deletion intents

Date: **2026-11-21**. Repo `D:\NSW.dev\NSL` (`https://github.com/UzuCore/NSL.git`). Stage 20 baseline `6e7113f9ed45ff5695746ea958cd4df6d73da3c9`. Source commit: `88a4d414bca4a2a08d1fd790e226a017cb9d2106`.

## Findings

- `librqbit 9.1.0` writes its JSON persistence state through a temporary file and `rename`. Its `Session::delete` removes the torrent from the in-memory database before flushing JSON and **logs, but swallows, persistence delete failures**. A sharing violation appearing after Stage 20's preflight therefore could resurrect a deleted torrent on the next launch.
- A real `ManagedTorrentState::Error` can be triggered by a Windows sharing violation on the verified-piece bitfield (`*.bitv`) during asynchronous initial integrity checking. This is an actual underlying librqbit error, not NSL's older terminal-watcher fault injection. The installed production session has `fastresume=false`; the bitfield loading path is selectively enabled in one isolated `#[cfg(test)]` fixture only. Existing production full-check settings remain unchanged.
- A live payload write `pwrite_all` disk failure was **not** injected; the actual error tested here is a **read failure of the resume bitfield**, which uses the same real underlying error state and NSL retry path. Do not conflate the two mechanisms.

## Production implementation

**Files:** `src/torrent.rs`, new `src/torrent_removal.rs` (isolated removal journal), `src/torrent_e2e_tests.rs` (test-only Windows sharing-violation injection).

1. **Durable deletion intent:** after safety preflight but before `Session::delete`, write a `nsl-deletion-intents/<id>-<hash>.json` record with the exact torrent ID, info hash and output path. Use a unique temporary file, `sync_all`, then rename. If a record cannot be written, deletion is refused instead of risking a ghost entry.
2. **Post-delete persistence check:** query the real in-memory session **and** check `session.json` for the **same ID and hash**. If the torrent disappeared from memory but is still serialized, keep the pending record and return an actionable Korean warning (no false success).
3. **Startup reconciliation:** before adopting restored torrents into download watchers, load pending records and match restored handles against **ID + hash + output folder**. Remove only exact matches from the librqbit session, with `delete_files=false`, so original payload files and unrelated torrents are never deleted by recovery. Once the JSON entry is absent and memory no longer has the torrent, remove the intent record.
4. **Failure remains recoverable:** if a session-only cleanup cannot succeed, retain the intent and warn; pause the matching live handle rather than silently continuing an unwanted download. Reused IDs or changed hashes/paths never authorize deletion of another torrent.
5. **Malformed journal safety:** check journal directory type and refuse symlink; ignore unrelated file extensions; reject malformed records and filenames whose ID/hash do not match the content. Do not silently delete user data on error.
6. **Test-only real race hook:** hold a Windows file handle with no delete-sharing permission *after* preflight while librqbit attempts to replace `session.json`. This forces a genuine OS rename failure inside librqbit without touching any user file or changing the installed dependency.

## Test evidence

### Stage 21 generated loopback E2E (2 explicit ignored tests)

**`stage21_real_librqbit_error_from_locked_resume_storage_recovers` — PASS**

- Generated 6,291,456-byte file, localhost-only seeder at 768 KiB/s, and a dedicated temporary profile.
- Downloaded real pieces, exited the downloader, then opened its synthetic `*.bitv` with an exclusive Windows handle during a resumed integrity check.
- The actual librqbit error was `error loading have_pieces` caused by **OS error 32**, and NSL showed it in a failed transfer row.
- Released the lock, issued Retry, confirmed command acceptance and completion; **all 6,291,456 output bytes matched the generated seed**, including original partial pieces. Dismissed the session with payload preservation.
- The temporary `fastresume=true` is available **only in `#[cfg(test)]`** and only for the disposable fixture with `.stage21-enable-test-fastresume`. The production executable retains full hash checking.

**`stage21_late_session_json_rename_failure_recovers_without_ghost_or_lost_files` — PASS**

- Generated 2,097,152-byte file and downloaded it to completion in a private localhost-only session.
- Snapshot `session.json`. After passing Stage 20 preflight, test-only hook opened a Windows guard preventing the JSON rename; **librqbit returned without persisting deletion**.
- NSL detected a still-serialized torrent, produced a deletion warning, kept a durable exact-match journal and cleared the now-removed in-memory row.
- `session.json` was **byte-for-byte unchanged** after the injected failure, proving the stale session really existed on disk (not a mocked response).
- Restarting the isolated engine cleared the exact-matching entry via the public librqbit API, removed the journal record and **never restored a ghost UI row**. Original downloaded bytes remained identical.

### Automatic and prior-stage regression

| Check | Result |
| --- | --- |
| `cargo test --locked --all-targets --quiet` | **223 PASS, 0 FAIL, 50 ignored** (273 total) |
| `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS, 0 FAIL, 3 ignored** |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| Stage 21 real librqbit and late JSON flush failure E2E | **2/2 PASS** |
| Removal journal: exact persisted identity, corrupt JSON, filename mismatch | 2 new automatic tests PASS |
| Stage 20 file deletion lock, JSON lock, missing-ID delete recovery | **3/3 PASS** (explicit tests) |
| Stage 19 terminal watcher fault / duplicate retry / quick cancel | PASS (explicit test) |
| Original pause/restart/resume/selective deletion | PASS (explicit test) |
| Real metadata scans, XCI 2 + XCZ 2 | PASS (explicit test) |

### Separate Eden GPU loading-only regression — PASS

- Reused the **previously merged** isolated NSZ `E:\NSL_QA_20261120\small\[NSL Merged] [0100F7301AAB6000][v65536].nsz`, not a newly synthesized unsigned XCI.
- Prior to launching the private emulator instance, checked that no other `eden`, `eden-cli`, `yuzu` or `ryujinx` process was active. The launcher uses only an owned private Eden binary, profile and temporary key copies and terminates only its own verified child process.
- **24 s observed**, true GPU window **1296×759** with **256 sampled unique colors**, renderer/loader checks met, no critical errors, and the original NSZ SHA-256 remained unchanged. No menu interactions or gameplay.
- Evidence: `E:\NSL_QA_20261120\small\eden_loader_stage21_session_recovery.{json,png}`; the private profile and copied keys were destroyed by the QA script.
- Recomputed SHA-256 of the original QA source packages **after** Stage 21: `base.nsz` = `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F`; `patch.nsp` = `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942`. Both are identical to the Stage 19/20 baseline hashes; neither was modified.

The original game library `E:\NSW`, prod.keys, encrypted titles, source NSP/NSZ, user scan data, saves and unrelated emulator instances were not modified. Test data and test-specific keys/profile are isolated in QA folders; real torrent tests use no public peers or trackers.

## Reproduce safely

```powershell
Set-Location 'D:\NSW.dev\NSL'
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
```

## Optimized Release and Windows GUI

- Source: `88a4d414bca4a2a08d1fd790e226a017cb9d2106`, embedded window title `NSL  88a4d41`.
- `cargo build --locked --release` **PASS** in **3m 36s**.
- `target\release\nsl.exe`: **22,267,904 bytes**, SHA-256 `25C9ECD21FE253178D09359F1DCFB5C9A1426526BD5D03F7E785AE01DDABB6B9`.
- Isolated Windows GUI **PASS** at **980×720 / 1280×800 / 1456×900**; temporary app profile fully removed.
- The protected distribution artifacts were **not** overwritten: `dist\nsl.exe` SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`, and `dist\nsl-win64.zip` SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.

## Explicit limitations and future work

- The Stage 21 actual internal error is a **resume-bitfield I/O read failure**, not an induced fatal live payload write or a failing physical disk. A real `pwrite_all` error still needs an isolated faulting storage backend or virtual filesystem; don't weaken security or mutate shared registry crates to fake it.
- Crash consistency is improved, but a power loss between the engine's deletion, operating-system cache flushes and journal updates still has small durability windows. Separate-process concurrent writes after an intent is committed must not be assumed safe without further integration tests.
- On startup librqbit may briefly restore a stale session before NSL's journal reconciliation. Recovery is performed before the NSL watcher is adopted; do not promise zero network or storage activity during that underlying library startup window.
- The tests do not prove that newly synthesized unsigned XCI templates are retail-signed or emulator-bootable. Confirm any additional NSZ/XCI/XCZ loading only in isolated emulator profiles without gameplay or interference.
