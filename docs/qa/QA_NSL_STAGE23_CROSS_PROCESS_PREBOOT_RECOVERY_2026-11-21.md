# NSL Stage 23 - Cross-process torrent session ownership and pre-restore deletion recovery

Date: **2026-11-21**. Root `D:\NSW.dev\NSL`. Baseline Stage 22 final `0dbb3cb164cda3ec592e3c97b54aba31e7a811a2`. GitHub: `https://github.com/UzuCore/NSL.git`.

**Verified Stage 23 implementation commit:** `2a9aea68432812b3b532399e11dcccd1261f96a4` (the subsequent documentation-only commit does not alter Release source).

## Scope and safety controls

Stage 23 prioritizes correctness and crash-safety of the persisted BitTorrent session (`torrent-session\session.json`). The test cases use **generated files, a localhost-only BitTorrent peer/tracker, and temporary isolated profiles**. No code opens `E:\NSW` original games, production keys, saves, or unrelated emulators for writing. The distribution files under `dist` are protected.

### One owner across Windows processes

- Added `src/torrent_session_lock.rs`, an exclusive `fs2` OS-level file lock on the stable **parent directory sibling** `.nsl-torrent-session.lock`, not inside the deletable `torrent-session` directory. Its Windows handle disallows delete-sharing while held to avoid lock-file name replacement.
- `TorrentEngine::new` acquires the lock **before constructing librqbit's session** and holds it for the full Tokio runtime lifetime. A second process with the same NSL base directory receives a Korean warning and cannot restore or mutate the shared session.
- The Settings `clear_session_state` operation acquires the identical lock before recursively removing any saved state. It refuses clearing while an existing engine owns the session. The lock file is deliberately retained outside `torrent-session` when clearing succeeds.
- The lock acquisition retries for at most three seconds to accommodate ordinary shutdown handoffs. It does not forcibly terminate any other NSL instance.

### Deleted torrents are suppressed before librqbit resumes

- Added `reconcile_before_restore` to `src/torrent_removal.rs`, run **after acquiring the session lock but before `Session::new_with_opts`**. This closes the Stage 21/22 startup window where librqbit could briefly revive an interrupted deletion and contact peers before NSL removed the stale UI row.
- Only persisted torrents matching a durable intent on **numeric session ID + info hash + exact output folder** are removed. JSON fields and unrelated torrent entries are preserved. Stale intents pointing to unrelated folders are cleared without removing other content.
- If entries must change, NSL writes an adjacent unique temporary JSON file, flushes and `sync_all`s it, checks the source JSON did not change during preparation, then performs an atomic rename; the intent remains until the persisted entry is verifiably absent.
- On damaged JSON, malformed intent, denied rename, or concurrent unrecognized session change, startup fails closed in Korean **before librqbit opens the torrent session**. Original saved data and deletion intents are retained for later repair. Journal recovery never deletes downloaded payload bytes.
- An already-existing Stage 21 reconciliation after librqbit startup remains as a defensive fallback.

## Test evidence

| Test / check | Result |
| --- | --- |
| `stage23_cross_process_lock_preserves_active_session_and_restart` | **PASS**: one real independent Windows child denied while first NSL engine active, session clear denied; 4,194,304-byte torrent subsequently restored exactly |
| `stage23_owned_child_abrupt_exit_releases_lock_and_restores_data` | **PASS**: only a test-owned child process was forcibly killed; OS released its file lock, and a following session recovered all 5,242,880 bytes exactly |
| `stage23_preboot_removes_only_exact_identity_and_keeps_unrelated_json` | **PASS**: exact match removed, unrelated session entries and custom JSON fields preserved |
| `stage23_preboot_atomic_rename_denied_leaves_original_and_retries` | **PASS**: genuine Windows file handle blocked rename, original JSON and journal survived byte-for-byte; retry after unlock succeeded |
| `stage23_corrupt_deletion_journal_fails_closed_without_modifying_session` | **PASS**: startup refused a corrupted disposable deletion intent before restoring session state; original serialized metadata unchanged |
| NSL normal Rust tests | **227 PASS / 0 FAIL / 56 ignored** (283 cases) |
| Vendored NSRP Rust tests | **119 PASS / 0 FAIL / 3 ignored** |
| `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings` | **PASS / PASS** |

### Isolated Eden rendering-only regression — PASS

- Rechecked no other `eden`, `eden-cli`, `yuzu` or `ryujinx` processes were active, then launched an individually owned Eden CLI instance with a private executable, app profile and temporary copied keys.
- Used the **previously merged NSZ** `E:\NSL_QA_20261120\small\[NSL Merged] [0100F7301AAB6000][v65536].nsz` with the isolated patch-ticket data, not a freshly generated/unsigned XCI.
- Actual GPU window **1296×759**, **256 sampled distinct colors**, **24-second observed loading**, critical loader errors **0**, and source NSZ SHA-256 unchanged. No gameplay or menu interaction.
- Evidence: `E:\NSL_QA_20261120\small\eden_loader_stage23_preboot.{json,png}`. The test only terminates its exact owned process and removes its own temporary profile.

The opt-in test filter `stage23_ -- --ignored` includes a **child-only test harness helper** which is deliberately a no-op when invoked without the parent-owned environment marker; it is not counted as an independent cross-process scenario.

## Prior regression and large download smoke

| Explicit prior-stage E2E | Result on Stage 23 source |
| --- | --- |
| Stage 22 live `pwrite_all` failure, duplicate removal and journal crash boundary | **3/3 PASS** |
| Stage 21 true bitfield I/O error + late `session.json` rename failure | **2/2 PASS** |
| Stage 20 delete failure, real file lock and locked JSON persistence | **3/3 PASS** |
| Stage 19 terminal watcher retry/duplicate/cancel | **PASS** |
| Original 4 MiB pause/restart/resume/delete | **PASS**, 4,194,304 bytes exact |
| 16 MiB local-seeder interruption and resume | **PASS**, 16,777,216 bytes exact after peer outage; 39.33s observed recovery |
| XCI 2 + XCZ 2 read-only metadata scans | **4/4 PASS** |

## Optimized Release, three-size GUI and distribution protection

- `cargo build --locked --release` **PASS** in **3m 36s**. The executable `D:\NSW.dev\NSL\target\release\nsl.exe` embeds source revision `2a9aea6` and is **22,301,696 bytes**; SHA-256 `D08DD7B82C2E0E0B6BC20192842CA092E55666A78FA2339420CC125B9D2E29B4`.
- Isolated Windows native GUI smoke **PASS**, title `NSL  2a9aea6`, verified at **980×720, 1280×800 and 1456×900**. Only the test's private profile was removed (`GUI_NATIVE_TEMP_CLEAN=YES`).
- Existing distribution artifacts are unchanged: `dist\nsl.exe` SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`, and `dist\nsl-win64.zip` SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.

## Storage capacity testing limitation

- Verified the Windows session is **not elevated**. Although Hyper-V's `New-VHD`/`Mount-VHD` commands are installed, the permissions needed to mount a dedicated writable quota-limited test volume have **not been verified** in this session. No VHD was mounted, and no attempt was made to consume all free space on a shared C:, D:, E: or other user volume.
- The *real write path* was already tested in Stage 22 using an actual Windows OS error 33 at `librqbit::FileOps::write_chunk`; this does **not** establish correct handling of actual `ERROR_DISK_FULL` / Windows error 112, removed storage, SMB disconnect or NTFS quota enforcement.
- Stage 23 therefore **does not claim an out-of-space E2E pass**. That test remains a priority requiring an authorized dedicated test volume or isolated quota-enforced filesystem.

## Reproduction (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
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
```

## Remaining limitations / Stage 24 work

- The session-level lock coordinates NSL processes that implement this lock. A third-party application that deliberately writes `session.json` outside the lock protocol can still race the last atomic replacement; the source-byte recheck narrows but does not eliminate this adversarial window.
- **Earlier NSL executables from Stage 22 or before do not participate in the new lock.** Do not run an older NSL build and Stage 23 concurrently against the same app profile/session state.
- Journal and JSON file `sync_all` cannot guarantee survival of sudden host power loss without filesystem/device write ordering and directory durability guarantees. The forced-kill E2E intentionally affects **only** one owned, disposable child test process.
- Disk-full/SMB/remote-volume failures, sustained many-gigabyte torrent performance, strict Title ID/version matching for missing updates/DLC, and newly generated retail-signed XCI/XCZ loader-only acceptance require separate focused testing. No source games or externally hosted downloads were modified during Stage 23.
