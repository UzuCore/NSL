# NSL Stage 26 — real two-DLC restart, cancellation and large-batch performance

Date: **2026-11-22** (Asia/Seoul). Project `D:\NSW.dev\NSL`. Previous Stage 25 main `afe55dddf444121cc410a5a5e0e8c55a26e4d9f7`.

**Verified Stage 26 source commit:** `6ba7e809371679f413efb1873ed3590f8025724d`. The final documentation-only commit does not modify the verified executable's embedded source revision.

## Safety and provenance

- The source game is a **1,372,476,535-byte** existing NSZ already containing the Base CNMT, one Update CNMT and **27 existing DLC CNMTs**. Two real and genuinely missing DLC NSPs, with independently validated CNMT identities, are `010080001592701D` and `010080001592701C` (version 0; each 124,112 bytes). The test verifies content identity, not a separate retail-signature certification.
- Original game and DLC packages under `E:\NSW` / `E:\NSW\_dlc` are **read-only**. Test-only copies are created in a unique temporary directory under the independent `E:\NSL_QA_20261120` root. The QA requires at least 16GiB free in the QA and OS temporary volumes.
- The test copies the game and DLC inputs, hashes each copy against its original, and invokes trusted native/header CNMT validation of both DLC packages. No game library file, key, save, unrelated emulator instance or protected `dist` release file is changed.
- Verified source SHA-256 after E2E: game NSZ `23B3F50B2D2969A6A52FD646FB1FC7C33CFE9BDF222C88ACC8AE2EE9B49D8188`; DLC `010080001592701D` `040232A33243311C6DF1017E8E1256E357D3185299B3834CA3AF51E22BF6107E`; DLC `010080001592701C` `5AD6C140D785F3A209CD7FC1259AB7C27CFAD8124E23FF96782A7F41DB300693`.

## Actual signed DLC cumulative batch and restart test

`stage26_real_two_dlc_batch_cancel_restart_and_cleanup` is an opt-in Rust test in `src/update_merge/stage13_real_dlc_qa.rs`. Its operations are:

1. Write and restore a real production `batch-updates.json` manifest with **two transfer-finished jobs** pointing at private downloaded DLC copies.
2. Merge DLC #1 into the copied NSZ; independently verify the new DLC CNMT, original Base and all prior DLC/Update CNMTs. Save its merged path to the durable batch journal.
3. Request cancellation **before** DLC #2 starts. Check that no second output exists and the first published file is byte-identical. This tests cancellation at a safe job boundary, **not cancellation during a write or power loss**.
4. Drop the running batch and load the durable JSON. Require the recovered second DLC to use the previous merged NSZ as its source, not the unmodified base game. Resume the queued second job.
5. Merge DLC #2 and independently verify **both new DLC CNMTs**, preservation of all earlier CNMTs and real `.ncz` compressed entries inside the final `.nsz` (not a renamed NSP).
6. Call the production `consolidate_finished_dlc_outputs`, which persists final output references before retiring exactly one intermediate NSZ. Verify the original and **both downloaded NSP files survive**, the final output stays hash-identical, and a second cleanup attempt is idempotent.
7. Re-read both original DLC hashes and original base hash. After test execution, automatically delete only its own private QA directory and temporary 1.37GB results.

Final two-DLC E2E result: **PASS**, **441.60 seconds** in the real merge test (441.90 seconds including test harness), **29 → 30 → 31 native CNMTs**, final compressed NSZ **1,372,555,717 bytes**, `cancelled_before_second=true`, `journal_restored=true`, `retired_intermediate=1`, `original_sha_unchanged=true`. Auto-cleaned QA root had **zero remaining `stage26-dlc-batch-*` folders**. Evidence: `docs\qa\QA_NSL_STAGE26_REAL_TWO_DLC_CANCEL_RESTART_LOCAL_2026-11-22.txt`.

## Production correctness and performance changes

- `cumulative_dlc_input` now **blocks** subsequent DLCs of the same original game when a previous DLC is `Verified` but not actually merged and published. It also blocks subsequent DLCs when a previous **checked, catalog-matched** DLC has **no queued job**, e.g. due to a disk-space admission failure. Otherwise later DLC results could falsely omit that earlier item.
- `cumulative_dlc_inputs` computes all per-game/source predecessor states with **one forward-pass HashMap**, preserving sticky failures and independent original game paths. This replaces repeated prefix scans with potential quadratic cost in the GUI download-completion loop.
- `poll_missing_update_audit` constructs the predecessor map only if a merge worker is free and at least one downloaded DLC awaits verification, avoiding extra work during normal UI frames.
- `stage26_three_thousand_dlc_predecessors_are_linear_and_fail_closed` exercises **3,000 synthetic DLC jobs** including a blocker at the midpoint, compares results to the single-row reference and verifies not-yet-queued selected DLCs block later merges.

## Windows process memory and I/O sampling

- This is a **real multi-GB local merge I/O workload**, not a multi-GB BitTorrent network-speed benchmark. A separate read-only monitor samples only the exact test process's Windows `ReadTransferCount`, `WriteTransferCount` and working set. Cache and filesystem metadata effects can influence these counters.
- During **109 samples covering part of the real E2E**, the Windows test process reported **11.617 GiB cumulative reads**, **3.835 GiB cumulative writes** and **29.52 MiB maximum observed working set**. Sampling did not cover the entire run, so these are **partial-process observations**, not complete workload totals or the peak memory of production GUI NSL. Evidence: `docs\qa\QA_NSL_STAGE26_REAL_TWO_DLC_IO_LOCAL_2026-11-22.txt`.
- The 3,000-row predecessor workload is a synthetic CPU/memory benchmark, not proof of production many-terabyte library performance.

## Verification matrix

| Item | Status |
| --- | --- |
| Real two-DLC batch, cancellation at boundary, restart, final cleanup | **PASS**, 441.60s, CNMT 29→31, one intermediate retired, private QA cleaned |
| 3,000-row cumulative DLC selection and blocked predecessor | **PASS**, 5 ms one synthetic local run |
| NSL full Rust test suite | **235 PASS / 0 FAIL / 60 ignored** (295 cases) |
| Vendored NSRP Rust test suite | **119 PASS / 0 FAIL / 3 ignored** |
| Rust fmt / Clippy `-D warnings` | **PASS / PASS** |
| Stage 25 two-game selected-file local BitTorrent E2E | **PASS**, 2,097,152 bytes exact, one torrent, originals unchanged |
| Stage 24 real WSL tmpfs exhausted volume → Retry | **PASS**, 12,582,912 bytes exact, real I/O error 31, isolated mount removed |
| Stage 23 cross-process lock + forced owned-child restart | **3/3 PASS** |
| Stage 22 real payload `pwrite`, journal and duplicate deletion | **3/3 PASS** |
| Stage 21 real bitfield I/O and session JSON file errors | **2/2 PASS** |
| Stage 20 locked file, deletion and journal retry | **3/3 PASS** |
| Stage 19 watcher failure/retry/cancel | **PASS** |
| 4MiB stopped/restarted transfer, unrelated output preserved | **PASS** |
| 16MiB local peer outage / restart / exact-byte recovery | **PASS**, 16,777,216 bytes, 35.83s observed |
| Native XCI 2 + XCZ 2 read-only metadata scans | **4/4 PASS**, Korean locale flags present |
| Isolated Eden GPU-only previously merged NSZ loader | **PASS**, 24 seconds, 1296×759, 249 sampled colors, separate private profile |
| Optimized Release, native GUI 3 sizes and protected source hashes | **PASS**, 3m27s compile, 3/3 GUI sizes, `dist` unchanged |

### Final Windows Release

- Optimized executable: `D:\NSW.dev\NSL\target\release\nsl.exe`, **22,340,608 bytes**; SHA-256 `6146C76F5263208FB8EC89A132891261721770162B1B0D665F90FED1DE06F410`; embedded verified Stage 26 source commit `6ba7e80`.
- Native Windows GUI title `NSL  6ba7e80` passed **980×720**, **1280×800** and **1456×900**. Private GUI smoke profile was cleaned.
- Preexisting `dist\nsl.exe` SHA-256 remains `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`; `dist\nsl-win64.zip` SHA-256 remains `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`. These protected files were never overwritten.
- Independent, already existing merged NSZ GPU-only loader smoke evidence: `E:\NSL_QA_20261120\small\eden_loader_stage26_dlc_journal_regression.{json,png}`; **PASS** 24s at 1296×759 with 249 sampled colors. This is **not** a boot test of the transient new Stage 26 two-DLC output, which was auto-cleaned.

## Repeatable commands (Windows PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage26_three_thousand_dlc_predecessors_are_linear_and_fail_closed -- --nocapture
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa_stage26_two_dlc_recovery.ps1'
cargo test --locked stage25_two_games_one_torrent_selected_files_and_originals_preserved -- --ignored --nocapture --test-threads=1
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa_stage24_quota_e2e.ps1'
cargo test --locked stage23_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage22_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage21_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo build --locked --release
```

The Stage 26 script checks source/QA isolation, the unique read-only base and DLC inputs and free disk headroom; it does not fill or mount an existing user drive.

## Explicit unverified scenarios

- **Native Windows NTFS `ERROR_DISK_FULL` 112 and SMB disconnect/reconnect**: cannot be safely reproduced with the current non-elevated user and no dedicated isolated NTFS quota volume or SMB server. The Stage 24 real WSL tmpfs ENOSPC was delivered as Windows **error 31**, not 112. Do not consume space on the user's C:, D:, or E: drives to force it.
- **Actual power loss during a native DLC write**, and full-gameplay/newly generated signed XCI/XCZ loading: not established by this isolated cancellation-at-boundary or a preexisting merged NSZ GPU smoke.
