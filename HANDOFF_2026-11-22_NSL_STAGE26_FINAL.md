# NSL Stage 26 — final handoff (2026-11-22)

## 1. Workspace, version, safety

- Windows source root: `D:\NSW.dev\NSL`, remote `https://github.com/UzuCore/NSL.git`, branch `main`.
- Stage 25 final starting commit: `afe55dddf444121cc410a5a5e0e8c55a26e4d9f7`.
- Stage 25 handoff: `HANDOFF_2026-11-21_NSL_STAGE25_FINAL.md`.
- **Stage 26 verified source commit:** `6ba7e809371679f413efb1873ed3590f8025724d`.
- The Stage 26 documentation-only commit follows the source commit on GitHub `main`; use `git log -1 --format=%H` and `git ls-remote origin refs/heads/main` to check their agreement. The Release EXE embeds the **source** commit, not the documentation-only commit.
- Detailed evidence: `docs\qa\QA_NSL_STAGE26_REAL_DLC_RESTART_IO_2026-11-22.md`.
- Raw opt-in/automated logs: `docs\qa\QA_NSL_STAGE26_*LOCAL_2026-11-22.txt` (locally ignored).
- Production files under `E:\NSW` and `E:\NSW\_dlc`, other emulator processes, prod.keys, user data and protected `dist` binaries were never modified. All generated full-sized DLC merge outputs were placed under a unique temporary `E:\NSL_QA_20261120\stage26-dlc-batch-*` directory and **automatically removed** at the end of the test.

## 2. Actual real-content two-DLC E2E — PASS

- A previously verified base NSZ contained **29 CNMTs**: Base 1, Update 1, DLCs 27. The test revalidates real content identity rather than certifying retail signatures separately.
- Read-only original game `0100800015926000` file size **1,372,476,535 bytes**, source SHA-256 `23B3F50B2D2969A6A52FD646FB1FC7C33CFE9BDF222C88ACC8AE2EE9B49D8188`.
- Two missing real DLC package IDs: `010080001592701D`, original SHA256 `040232A33243311C6DF1017E8E1256E357D3185299B3834CA3AF51E22BF6107E`, and `010080001592701C`, original SHA256 `5AD6C140D785F3A209CD7FC1259AB7C27CFAD8124E23FF96782A7F41DB300693`. Each DLC is **124,112 bytes**, version 0.
- The isolated QA copies both DLC NSPs to separate private download folders and validates their **actual trusted CNMT**, not catalog or filenames alone. It writes and restores the production `batch-updates.json` journal.
- DLC #1 yielded **30 CNMTs**, then cancellation **between merge jobs** prevented DLC #2 output from being published. The first merged result remained SHA-identical. After loading the persisted journal, DLC #2 resumed from the first merged result, yielding **31 CNMTs** while preserving the existing Base, Update and all DLC metadata.
- Final output **1,372,555,717 bytes**, genuine compressed NSZ (`.ncz` entries). The production cleanup first persisted references to the final file, then removed exactly **one prior intermediate NSL-generated NSZ**. Both private downloaded DLC NSPs and the original copy survived, the cleanup operation was idempotent, and the private QA directory was removed by the fixture.
- `STAGE26_REAL_TWO_DLC_BATCH_RESTART_PASS` **1/1 PASS**, real E2E elapsed **441.60 seconds**. This is cancellation at a boundary; no power-loss mid-write claim.

## 3. Production code and performance improvements

- `src/app/update_batch.rs`: DLC prior-job selection now **blocks** a later DLC when a preceding matching DLC has status `Verified` but no published merge, or when an earlier checked/catalog-matched DLC was **not queued** (e.g. due to disk space). This closes a potential gap where a later DLC would overwrite the cumulative result without the earlier selected DLC.
- Added `cumulative_dlc_inputs` calculating the entire batch's `(base Title ID, original source path)` prefix dependency in a single **O(n)** forward pass. Sticky unresolved predecessor state prevents following DLC work from skipping a failed/incomplete item. `poll_missing_update_audit` builds this table only when a completed DLC awaits verification and a worker slot is free.
- Kept the old single-item prefix function **test-only**, so the new bulk implementation can be checked against a simpler reference without carrying quadratic scans in Release.
- `stage26_three_thousand_dlc_predecessors_are_linear_and_fail_closed`: **PASS**, **3,000** synthetic DLC jobs, one-run dependency map **5 ms**. Cross-source separation and selected-but-not-queued blocking preserved. This is **not** a production multi-TB throughput measurement.
- An optional reproducible PowerShell harness `scripts\qa_stage26_two_dlc_recovery.ps1` checks input uniqueness, distinct read-only source and QA roots and at least 16GiB scratch headroom before running the long real-content test. Its PowerShell syntax parser passed. The handoff's first real E2E was executed with equivalent explicit environment variables.

## 4. Runtime I/O observation and regression

| Check | Result |
| --- | --- |
| Actual two-DLC compressed NSZ, stop/restart and cleanup | **PASS**, final 31 CNMTs, temporary files cleaned |
| 3,000 DLC forward-pass selection & source isolation | **PASS**, one synthetic run 5 ms |
| Full NSL Rust suite `cargo test --locked --all-targets --quiet` | **235 PASS / 0 FAIL / 60 ignored** (295 total) |
| Vendored NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| `cargo fmt --all -- --check`, Clippy `-D warnings` | **PASS / PASS** |
| Stage 25 real selected-only two-game loopback torrent | **PASS**, 2MiB, originals untouched |
| Stage 23 cross-process lock and owned-child restart | **3/3 PASS** |
| Stage 22 real `pwrite` error, delete and journal recovery | **3/3 PASS** |
| Stage 21 real bitfield/JSON I/O errors | **2/2 PASS** |
| Stage 20 real locked file deletion and journal errors | **3/3 PASS** |
| 16MiB local transfer interruption/restart | **PASS**, 16,777,216 bytes exact, 35.83s observed |
| Read-only XCI 2 / XCZ 2 native metadata | **4/4 PASS** |
| Stage 24 actual WSL tmpfs exhaustion and exact Retry | **PASS**, 12,582,912 bytes exact, real OS error 31, isolated mount removed |
| Existing isolated Eden NSZ GPU loader-only smoke | **PASS**, 24s, 1296×759, 249 sampled colors |

- Read-only Windows test-process sampler (109 samples over part of the real E2E) reported **11.617GiB ReadTransferCount**, **3.835GiB WriteTransferCount**, and a **29.52MiB maximum observed working set**. These Windows counters can include cache effects, represent only the test process and do not certify sustained native disk bandwidth or the Release GUI's peak memory. Log: `docs\qa\QA_NSL_STAGE26_REAL_TWO_DLC_IO_LOCAL_2026-11-22.txt`.
- The test-owned base/DLC copied inputs, final output and journals were auto-cleaned. No other emulator processes were terminated.

## 5. Release, GUI and GitHub verification

- Optimized EXE destination: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- `cargo build --locked --release`: **PASS**, **3m 27s** optimized compilation.
- EXE length: **22,340,608 bytes**.
- EXE SHA-256: `6146C76F5263208FB8EC89A132891261721770162B1B0D665F90FED1DE06F410`.
- Native Windows GUI at **980×720 / 1280×800 / 1456×900: 3/3 PASS**; title `NSL  6ba7e80`. Private GUI test profile deleted (`GUI_NATIVE_TEMP_CLEAN=YES`).
- Existing protected `dist\nsl.exe` SHA-256 must remain `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Existing protected `dist\nsl-win64.zip` SHA-256 must remain `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Push the final source and docs to `main`, ensure local HEAD == remote main, and Git worktree **CLEAN**. Optimized binary embeds the Stage 26 **source** commit, before the later documentation-only commit.

## 6. Reproduction commands (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
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
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
```

## 7. Stage 27 remaining work and explicit limitations

1. **Native Windows NTFS `ERROR_DISK_FULL` 112** and **SMB disconnect/reconnect** still require a separately authorized isolated NTFS quota/VHD and private SMB environment. Current Windows user is non-elevated; the Stage 24/26 genuine WSL tmpfs capacity-exhaustion case manifests Windows error **31**, not 112. Never fill C:, D: or E: with real user files to simulate this.
2. **Mid-write power cut / process crash during actual DLC publication** was not tested: the Stage 26 cancellation is **between first and second merge**. A durable journal with full end-to-end power-loss guarantees requires additional controlled failure testing.
3. Profile sustained many-GiB **BitTorrent network** throughput, memory and cancellation, distinct from the actual multi-GiB local compressed NSZ merge I/O measured here.
4. New authorized signed **XCI/XCZ** outputs need independent emulator loader-only compatibility verification; the existing NSZ GPU smoke does not prove new XCI bootability or gameplay.

## Next-chat instruction

`D:\NSW.dev\NSL\HANDOFF_2026-11-22_NSL_STAGE26_FINAL.md` 기준 Stage 27을 이어가. 관리자 권한이 있는 별도 격리 NTFS·SMB 테스트 볼륨에서 실제 오류 112와 연결 장애를 검증하고, 실제 DLC 병합 중단·재개 및 대용량 토렌트 전송의 메모리/I-O를 개선해. 원본 파일, prod.keys, 세이브, 다른 에뮬레이터는 변경하지 마.
