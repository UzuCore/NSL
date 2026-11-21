# NSL Stage 20 - Torrent deletion safety, recovery and Windows lock QA

## Baseline and scope

- Date: 2026-11-21. Source: `D:\NSW.dev\NSL` at Stage 19 handoff commit `3a9916a9c3608dcfd0170a71d36d9a3ac1ebbfbc`.
- Modified production module: `src/torrent.rs`; isolated E2E tests: `src/torrent_e2e_tests.rs`.
- All Stage 20 transfer fixtures contain **generated random-looking test bytes**, served by an in-process localhost-only tracker and seeder. **No commercial torrent, public BitTorrent tracker, user download folder, user keys, emulator profile or source games are used.**
- Stage 19 already verified real NSZ+update NSP selected-file XCI packaging and isolated Eden loader-only behavior. Stage 20 does not modify merge or emulator code.

## librqbit 9.1.0 hazards confirmed by source audit

1. `Session::delete` first removes the torrent from its in-memory database, then attempts to release storage and delete persistence. If later cleanup fails, the session entry may already be gone even when the API returns an error.
2. JSON persistence `delete()` can fail to write/rename `session.json`; `Session::delete` **logs but swallows** that failure, potentially resurrecting a stale session after restart.
3. `remove_files_and_dirs` **logs but swallows per-file deletion errors**, meaning `Session::delete(..., true)` can report `Ok(())` while downloaded files remain on disk.
4. Prior NSL behavior set the cancel flag *before* the delete result was known, which could cause the watcher to drop the only visible progress row after a recoverable deletion failure.

## Implemented fixes

- **Session-aware completion:** attempt session removal first, then query the actual `Session::get(id)` to decide whether the torrent was removed, independent of `Result` alone. Signal watcher cancellation and clear in-memory maps only after the session entry truly disappears.
- **Failure retention:** if the actual torrent remains registered, preserve the watcher, progress, existing downloaded bytes, and retryable session metadata. A later deletion request can succeed without restarting the transfer.
- **Windows file preflight:** for `delete_files=true`, derive validated in-torrent filenames and attempt a non-mutating `DELETE`-access open (`0x10000` with share mode `READ|WRITE|DELETE`). Denied deletion rights, incompatible file locks and filesystem inspection errors cancel removal **before** any session mutation. Reject symlink file entries and paths containing unsafe components.
- **Windows persistence preflight:** inspect the actual `torrent-session\session.json` before deletion; reject read-only, nonregular or locked session files with an actionable Korean message, preventing known silent persistence-flush failures.
- **Post-deletion verification:** check the exact torrent-owned relative file paths with `symlink_metadata`; if files remain despite an engine `Ok`, report a partial cleanup instead of false success. When the engine has already removed the session, never leave a dead/retry-eligible download row behind.
- Real program behavior is unchanged on downloads, torrent metadata resolution, original source games and other application state. Windows preflights do not open files for writing and never remove unrelated files.

## Isolated E2E tests (explicitly run ignored tests)

### 1. Failed `Session::delete` while transferring — PASS

`stage20_failed_session_delete_preserves_progress_restart_and_files`

- Generated data: **6,291,456 bytes**; localhost-only seeder, 768 KiB/s upload limit.
- Injected a *real librqbit `Session::delete` lookup error* by passing a guaranteed absent torrent ID **only for the first attempted delete**. This is a deterministic missing-ID failure, **not a disk I/O fault or corrupted torrent**.
- On delete error, the original downloading row remained visible and progressed (observed **1,114,112 → 1,507,328 bytes** in the final pre-persistence test).
- Re-created the engine from its saved resume folder: the transfer resumed to 100%, and the output matched all **6,291,456 bytes** of the seed.
- `delete_files=false` removed the session but preserved all payload bytes. A separate `delete_files=true` failure was retried successfully; only its own generated output was deleted. Sibling file and sentinel remained unchanged.

### 2. Genuine Windows file-delete lock — PASS

`stage20_locked_file_delete_reports_partial_cleanup_without_stranded_session`

- Generated data: **2,097,152 bytes**.
- Held a real Windows handle opened without `FILE_SHARE_DELETE` against the isolated downloaded file.
- Preflight rejected deletion before removing the session. Existing payload, queue row and unrelated sentinel remained intact.
- Released the handle; a repeat delete succeeded, and the selected payload was removed. App restart did **not** restore a ghost row.
- A prior diagnostic intentionally tested Windows read-only attributes alone; the engine did not reproduce a stable failure in that scenario. It is **not** counted as a successful fault-injection result. The real no-delete handle is the final tested case.

### 3. Genuine Windows session.json lock — PASS

`stage20_locked_persistence_file_preserves_session_until_retry`

- Generated data: **2,097,152 bytes**.
- Held an actual Windows handle denying `FILE_SHARE_DELETE` on the disposable `torrent-session\session.json` file.
- Deletion was blocked with a clear error while the completed download row, exact payload and **byte-identical session.json** remained intact.
- After releasing the guard, session-only deletion succeeded, downloaded bytes remained, and restarting NSL produced no stale torrent row.

All three use separate `tempfile::TempDir` roots that are cleaned automatically. The new test-only failure hook is compiled only for `cargo test`, not in the optimized production executable.

## Recorded regression verification

| Check | Result |
| --- | --- |
| Stage 20 explicit `cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1` | **3 PASS, 0 FAIL**, real Windows locking & simulated absent-ID failure |
| NSL `cargo test --locked --all-targets --quiet` | **221 PASS, 0 FAIL, 48 ignored** (269 cases) |
| NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS, 0 FAIL, 3 ignored** |
| Rust formatting `cargo fmt --all -- --check` | PASS |
| Clippy `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| Stage 19 6 MiB terminal watcher retry+cancel E2E | PASS, 1 retry accepted and 1 safely rejected |
| Previous 4 MiB pause/restart/resume/selective-delete E2E | PASS, exact output data and unrelated file preservation |
| Read-only native XCI 2 / XCZ 2 scan against existing library | PASS, metadata preserved |

The two earlier Stage 19 hashes for protected distribution files remain the expected baselines: `dist\nsl.exe` = `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`; `dist\nsl-win64.zip` = `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`. Do not overwrite the protected packages merely to publish the new `target\release\nsl.exe` test build.

### Isolated preflight microbenchmark

- Generated **1,024 temporary 16-byte files** under the Windows `%TEMP%` directory and issued the same non-mutating Windows `DELETE`-access probe to each.
- Measured **57.75 ms total / 56.40 microseconds per file** on this workstation. All benchmark files and the standalone measurement executable were removed.
- This is a warm-local-filesystem microbenchmark of the access check only; it does **not** establish performance for network shares, antivirus-locked files, mechanical disks or large real torrent deletion workloads.

## Stage 20 optimized Release verification

- Tested source commit: `e33443f81d88c7165490a57344e396bc935ba69c`.
- `cargo build --locked --release`: **PASS** in **3m 51s**.
- Output: `D:\NSW.dev\NSL\target\release\nsl.exe`, **22,214,144 bytes**.
- SHA-256: `F833BCC686F454B8D26274A31736CC01BA085254205D8146BD764DC41630E432`.
- Isolated native Windows GUI: **PASS**. Title `NSL  e33443f`; sizes **980×720, 1280×800, 1456×900**; temporary app profile completely removed.
- Existing `dist\nsl.exe` and `dist\nsl-win64.zip` remain unchanged at the protected hashes above.

## Validation commands

```powershell
Set-Location 'D:\NSW.dev\NSL'
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1
cargo test --locked transfer_pause_restart_resume_delete_preserves_other_files -- --ignored --nocapture --test-threads=1
cargo test --locked native_xci_xcz_samples_from_library_database -- --ignored --nocapture --test-threads=1
```

## Remaining limits / next stage

- The Windows open-based preflights narrow but cannot eliminate **TOCTOU** races: a lock, ACL change, disk I/O outage or persistence write failure can still occur *after* preflight. Exact post-delete file verification detects remaining payload files; persistence write failures occurring after successful preflight are only logged by librqbit itself.
- A genuine asynchronous `librqbit::ManagedTorrentState::Error` caused by failed live disk writes is still **not proven**. Its internal storage trait cannot be cleanly fault-injected from an external crate with the installed 9.1.0 private types; this should be evaluated via an isolated test-specific dependency patch or an independently controllable faulting volume, without changing production dependencies or user files.
- Do not claim Nintendo-signed XCI boot compatibility based on Stage 20's localhost transfer tests. Stage 19 covers structural XCI packaging and a *different*, previously built NSZ's loader-only success.
