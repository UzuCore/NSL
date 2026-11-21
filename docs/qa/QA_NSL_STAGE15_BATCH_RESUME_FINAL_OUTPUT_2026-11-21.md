# NSL Stage 15 — batch recovery, grouped DLC completion and final-only results

Date: 2026-11-21 KST. Baseline: Stage 14 `d4ce70beacf41bf119f843d11cea54058d08dbd8`.

## User intent

After checking a catalog's missing updates/DLCs, a download should complete into the queue, trigger native verification, and produce a merged package in the original format. A crash or transient peer failure must not silently detach the batch journal. Multiple DLCs for one game must be cumulative and leave one final published result after success. Original games, source downloads, user-managed packages and other emulator instances remain untouched.

## Repair 1 — restore and reconnect only to the correct download

- `src/app/update_batch.rs` now selects a *restored* torrent using its dedicated `output_folder`, not the old transient `tNN` runtime key, which could belong to an unrelated torrent after a restart.
- During an active session a same-key match is only accepted when its folder is absent (newly queued placeholder) or matches the expected folder. Otherwise the stable output folder wins.
- A download previously marked `Failed` while `transfer_finished=false` can recover automatically if the matching live torrent resumes or completes. When paused it returns to `Queued`; when active it becomes `Downloading`; an actual completion callback marks every matching job with a durable `transfer_finished=true`.
- A persisted `Failed` *post-download* native merge (`transfer_finished=true`) still needs an explicit retry. It is **not** silently retried in a loop.
- Test `restored_grouped_dlcs_reconnect_by_folder_and_resume_after_download_error`: two DLC jobs sharing one torrent key, stale reused key with a foreign output folder, correct restored folder, pause, failure, resume, completion, durable journal and serialized native merge scheduling. **PASS**.

## Repair 2 — retain only final output after all selected DLCs succeed

- A completed DLC job is still merged sequentially by the existing Stage 13 verified native engine. Once every queued DLC for one original game has status `Merged`, the batch consolidator redirects all completed job journal references to the **last fully published** result and saves the crash-recovery manifest atomically **before** deleting older intermediates.
- Files are only eligible for cleanup when their entire path exactly matches the native naming function for one of the *same batch's* DLC Title IDs, expected versions, source output format and original game folder. Symlinks, unexpected destinations, missing/empty final packages and forged unrelated paths are refused. Neither the original game nor a downloaded patch file is eligible.
- Any still queued/downloading/verifying/failed DLC prevents cleanup of that game's earlier outputs. Other games are grouped independently.
- Group discovery runs in O(N) via a hash map rather than scanning the entire DLC batch again for each title.
- `src/runtime_tests.rs::fully_merged_dlc_chain_preserves_only_final_file_and_crash_safe_journal` covers unresolved prior job, malicious path, simulated journal save failure and rollback, removal of two obsolete output files, final file plus original and unrelated file preservation, journal reload, idempotence, and subsequent DLC dependency. **PASS**.

The finalization deliberately runs only when a native merge completes in the current application session; untrusted stale journal files are not automatically purged at startup. Rare Windows file-lock failures can leave a redundant intermediate behind; the journal already points to the verified final package and the app logs a cleanup warning. The code never deletes a package merely because its filename begins with `[NSL DLC]`.

## Real-content handoff E2E (authorized local fixtures)

An opt-in ignored test `isolated_real_finished_transfer_restart_runs_native_nsz_merge` uses read-only authorized `E:\NSL_QA_20261120\small\base.nsz` (172,777,547 bytes) and `patch.nsp` (346,119,104 bytes). It copies the source into temporary QA workspaces, simulates a completed local torrent file, fires NSL's **real download completion handler**, reloads the **actual crash journal**, and waits for the **actual post-download native verification/merge worker** to create an NSZ. Source SHA-256 and final Base/Update CNMT are rechecked, then all temporary packages and app state are removed by RAII.

**Result: 1 PASS / 0 FAIL**. Native NSZ output **330,474,468 bytes**, **2 CNMTs** (Base + Update), transfer-complete journal recovered and both original-source SHA-256 hashes unchanged. Actual merge-worker stage elapsed **107.12 seconds**; entire isolated test elapsed **149.66 seconds**. Output size matches the independently checked Stage 4 fast-merger reference (330,474,468 bytes), for which 8/8 selected NCA round-trip hashes and Eden loading were separately verified. No source game, library file or user setting was overwritten.

**Important boundary:** This is a genuine completed-file → app event → crash-recovery → native merge integration test, but the BitTorrent peer exchange is **not** simulated. It does not prove catalog WAN connectivity, available seeds or real peer transfer performance. Earlier Stage 14 separately confirmed Eden's GPU-rendered Suika Game title screen with 29 DLC CNMTs. Gameplay and Switch hardware are intentionally outside scope.

## QA and release

Full regression after the changes: **NSL 211 PASS / 0 FAIL / 39 ignored**, **NSRP 119 PASS / 0 FAIL / 3 ignored**. `cargo fmt --all -- --check`, strict `cargo clippy --locked --all-targets -- -D warnings`, and Git diff checks all passed. The three Stage 15 tests are two ordinary regression tests (all-targets suite) and one explicitly ignored real-content E2E run (executed separately and passed).

The Stage 15 test results, release identity, GUI smoke and final integrity are recorded in local `QA_NSL_STAGE15_..._LOCAL_2026-11-21.txt` files. The final GitHub commit contains this tracked handoff plus source and test changes. Do not overwrite the older official `dist` files.
