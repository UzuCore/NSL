# NSL Stage 25 — Multi-game torrent selection, verified CNMT, and fail-closed batch journals

Date: **2026-11-21**. Root: `D:\NSW.dev\NSL`. Baseline Stage 24 final `e30c4108204e6cd7275c2d3ec844ead00af56f38`. GitHub `https://github.com/UzuCore/NSL.git`.

**Verified Stage 25 source commit:** `7f7c8d94c287b18be56c100ea59255d131a1bbdc`. Documentation is committed separately after the optimized release is verified.

## Production correctness improvements

### A single torrent holding multiple games' updates

- Changed `src/app/update_batch.rs::grouped_batch_downloads` to group **both updates and DLC** by magnet infohash rather than only DLC. Torrents differing in tracker URL but sharing an infohash now form **one** selected-files job; librqbit only supports one live handle per infohash. A missing infohash uses the full magnet as fallback.
- `checked_group_selection` validates every selected file index, path, positive size, and intended game/DLC identity, then computes the **exact sum of unique selected files** with `checked_add` for the existing download-space preflight. Identical indices with conflicting paths/sizes/Title IDs and different indices mapping to one output name are rejected before queuing anything for that group.
- Additional protection in `cumulative_dlc_input`: DLCs may form a cumulative merge chain only when **both base Title ID and original game path** match. Two independent copies of one game must never borrow each other's previous DLC merge output or failure state.

### Native multi-file BitTorrent E2E — **PASS**

The opt-in test `stage25_two_games_one_torrent_selected_files_and_originals_preserved` used a localhost-only tracker, generated multi-file torrent and temporary originals. No public tracker, game package, key, emulator, or other session was involved.

- Generated **two different game patch file names** (`.nsp`, 1 MiB each), plus an **unselected third 1 MiB file** in the same torrent.
- Real metadata preview and NSL's **production** missing-updates queue grouped the two game rows into **one torrent job** despite differing tracker URLs. The two jobs shared a torrent key and destination folder; `only_files` selected only the two intended indices.
- Both selected files downloaded byte-exact (**2,097,152 bytes** total), the actual completion notice completed both batch rows, and both private original-file sentinels remained byte-identical.
- The unselected third file was a **zero-byte placeholder**, not a downloaded payload. The test checks for the absence of seeded bytes, not absence of a placeholder path.
- Synthetic file contents have **no valid Nintendo CNMT** and were not merged or presented as loadable games. The test proves selected transfer behavior, *not* native merge or emulator boot of synthesized content.

### Real update CNMT from a protected source — **PASS**

Opt-in test `stage25_real_patch_cnmt_rejects_cross_game_and_nonnewer_version` uses explicit `NSL_STAGE25_PATCH_QA` and `NSL_STAGE25_BASE_ID` environment variables. Source was the already-authorized **346,119,104-byte** isolated QA `patch.nsp` under `E:\NSL_QA_20261120\small`, with base Title ID `0100F7301AAB6000` and requested version `65536`.

- Read-only source is fully copied into a disposable sandbox below the QA source folder; source and copy SHA-256 must match before scanning. The original **never receives writes**.
- `verify_downloaded_patch` accepts the real trusted native/header CNMT with exact patch Title ID `0100F7301AAB6800` and v65536.
- An unrelated base Title ID, an excessive required version v131072, and an installed version already at v65536 **all cause rejection**.
- Original and copy SHA-256 remain identical at the end; the temporary full copy is deleted automatically. This validates exact identity matching, not a full NSZ/XCI merge.

### Batch journal validation and many-item catalog grouping

- `src/update_manifest.rs` now checks saved **and restored** batch entries for a base-aligned 16-digit Title ID, kind-appropriate DLC ID, newer update version, nonzero source/payload sizes, bounded nonempty engine key, and consistent records. Invalid metadata stops recovery instead of restoring a stale merge or silently damaging user files.
- The deterministic `stage25_corrupt_batch_identity_and_lengths_fail_closed_without_original_loss` verifies eight classes of corrupt JSON and failed saves; original-file sentinel and prior valid durable journal survive every rejection. A valid file can subsequently be restored.
- `stage25_ten_thousand_catalog_matches_group_in_one_pass`: **PASS**. In one local synthetic test, **10,000 update rows** sharing **500 infohashes** were grouped into 20 files/torrent, all 10,000 indices and sizes checked in **40 ms**. This is an isolated one-run CPU observation, **not** a guaranteed production speedup or network/merge throughput measurement.

## Tests and regression evidence

| Test / operation | Result |
| --- | --- |
| Stage 25 two-game local torrent, selection, originals and completion notice | **PASS** |
| Stage 25 real 346 MiB CNMT exact identity / wrong ID / outdated version | **PASS** |
| Stage 25 10,000 synthetic catalog matches | **PASS**, one run 40 ms |
| Stage 25 fail-closed manifest corruption cases | **PASS** |
| Native NSL full Rust unit/integration tests | **234 PASS / 0 FAIL / 59 ignored**, 293 total |
| Vendored NSRP Rust tests | **119 PASS / 0 FAIL / 3 ignored** |
| Rust formatting / Clippy `-D warnings` | **PASS / PASS** |
| Stage 24 real WSL tmpfs volume exhaustion and retry | **PASS**, real Windows error 31, 12,582,912 bytes restored exactly; own tmpfs unmounted |
| Stage 23 cross-process lock, owned-child abrupt exit | **3/3 PASS** |
| Stage 22 real payload write lock, journal recovery, duplicate removal | **3/3 PASS** |
| Stage 21 real bitfield read and session JSON write errors | **2/2 PASS** |
| Stage 20 real file lock and failed deletion | **3/3 PASS** |
| Isolated Eden previously merged NSZ loader-only | **PASS**, 24 seconds, 1296×759 GPU frame, 256 sampled distinct colors |
| Optimized Release and native GUI smoke | **PASS**: 3m37s optimized build; 3/3 GUI sizes; private test profile removed |

### Final optimized Release and protected distribution

- `D:\NSW.dev\NSL\target\release\nsl.exe`: **22,332,416 bytes**, SHA-256 `036785F10FB62D2B9F5E02B59C57580E609E81BC1531372AF2C747275315D5FB`, embedded source commit `7f7c8d9`.
- Native Windows GUI title `NSL  7f7c8d9` and **980×720 / 1280×800 / 1456×900** layouts all **PASS**. Only the isolated GUI smoke profile was removed (`GUI_NATIVE_TEMP_CLEAN=YES`).
- Preexisting `dist\nsl.exe` remains SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423` and `dist\nsl-win64.zip` remains SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`; neither was overwritten.

All native update, download and batch records use private temporary paths in tests. Source QA file hashes rechecked during Stage 25: `base.nsz` SHA-256 `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F`, `patch.nsp` SHA-256 `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942`. Eden report: `E:\NSL_QA_20261120\small\eden_loader_stage25_multi_update.{json,png}`. The private Eden process/profile was removed; existing emulators, keys, saves and distribution artifacts were not modified.

## Actual NTFS ERROR_DISK_FULL 112 / SMB limitations

- The current Windows session is **not elevated** (`IsInRole(Administrator)=False`). Hyper-V commands `New-VHD`/`Mount-VHD` and SMB management commands exist but a dedicated writable NTFS quota volume has not been safely mounted or provisioned; no attempt was made to consume space on C:, D:, E: or any user's existing drive.
- Stage 24 had **genuine filesystem ENOSPC** over a private WSL tmpfs and real librqbit payload write, but Windows reported **error 31**, **not ERROR_DISK_FULL 112**. The exact native Windows 112-path and disconnected SMB share handling remain unproven in Stage 25; they require an authorized, independent NTFS/SMB test environment.
- The prior quota script `scripts\qa_stage24_quota_e2e.ps1` always unmounts its own private WSL tmpfs; do not reinterpret its result as proof of NTFS or SMB behavior. No unrelated mount or third-party process is ever force-stopped by this Stage 25 work.

## Reproduction (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage25_multi_game_updates_share_one_infohash_and_reserve_unique_files -- --nocapture
cargo test --locked stage25_ten_thousand_catalog_matches_group_in_one_pass -- --nocapture
cargo test --locked stage25_two_games_one_torrent_selected_files_and_originals_preserved -- --ignored --nocapture --test-threads=1
$env:NSL_STAGE25_PATCH_QA = 'E:\NSL_QA_20261120\small\patch.nsp'
$env:NSL_STAGE25_BASE_ID = '0100F7301AAB6000'
cargo test --locked stage25_real_patch_cnmt_rejects_cross_game_and_nonnewer_version -- --ignored --nocapture --test-threads=1
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa_stage24_quota_e2e.ps1'
cargo test --locked stage23_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage22_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage21_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo build --locked --release
```

## Stage 26 next priorities

1. Independently authorized NTFS quota/VHD for actual Windows 112; private SMB disconnect/write-denial/reconnect recovery with no shared-drive side effects.
2. Multi-DLC **real CNMT + cumulative merge** E2E for more than one independently signed package, plus deliberate cancel/crash between intermediate publications. The Stage 25 two-game torrent test verifies downloads only; the Stage 13 QA fixtures provide prior single-DLC regression coverage.
3. Real many-file/many-GB memory, disk throughput and slow-network profiling; test 10,000-row metadata grouping does not establish sustained throughput.
4. Retail-compatible signed gamecard XCI/XCZ loader-only validation only with authorized properly signed samples; the existing merged NSZ GPU loader is not evidence of new XCI boot compatibility.
