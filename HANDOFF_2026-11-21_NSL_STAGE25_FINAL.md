# NSL Stage 25 — final handoff (2026-11-21)

## Environment and version

- Root: `D:\NSW.dev\NSL` (Windows); remote: `https://github.com/UzuCore/NSL.git`, branch `main`.
- Stage 24 final starting commit: `e30c4108204e6cd7275c2d3ec844ead00af56f38`.
- Previous handoff: `HANDOFF_2026-11-21_NSL_STAGE24_FINAL.md`.
- **Verified Stage 25 source commit:** `7f7c8d94c287b18be56c100ea59255d131a1bbdc`.
- Final GitHub main points to the subsequent **documentation-only** commit; verify `git rev-parse HEAD` equals `git ls-remote origin refs/heads/main` and `git status --porcelain` is empty. The executable embeds the verified source commit, not the later documentation commit.
- QA: `docs\qa\QA_NSL_STAGE25_MULTI_GAME_CNMT_JOURNAL_2026-11-21.md`.
- Ignored local logs: `docs\qa\QA_NSL_STAGE25_*LOCAL_2026-11-21.txt`.

## Implemented functionality

### Multi-game updates bundled in a single torrent

- `src/app/update_batch.rs`: `grouped_batch_downloads` now groups **both Update and DLC** rows by a single torrent infohash. Differing tracker URLs do not create duplicate handles for librqbit's same infohash.
- `checked_group_selection` validates all selected `only_files` indices, nonzero sizes, paths and base/DLC Title IDs. Conflicting index-to-identity or different-index-to-same-output mappings are refused before queueing. It also computes exact unique selected bytes for the cumulative disk-capacity check.
- `cumulative_dlc_input` checks **both base Title ID and original file path**, preventing two separate original game copies from accidentally sharing a cumulative DLC input/failure chain.
- `src/update_manifest.rs`: batch persistence now validates restored and newly written metadata, including base-aligned 16-digit Title IDs, DLC IDs, strictly newer update versions, positive original and payload sizes and nonempty bounded engine keys. Corrupt records fail closed; previous valid journals and source files remain unchanged.

### Targeted E2E, CNMT and 10k catalog stress

- Generated localhost-only **three-file BitTorrent**: two different games' 1 MiB update NSP files were downloaded byte-exact in **one torrent job**; the unselected third file became only a **0-byte placeholder**; original sentinel files unchanged. Synthetic unsigned NSP data was **not** merged or claimed as a bootable game.
- Independently copied a protected **346,119,104-byte real QA update NSP**, scanned native/header CNMT: correct update base `0100F7301AAB6000`, patch version `65536` accepted; other game Title ID, outdated requested version and already-installed version rejected. Full SHA-256 of original and temporary copy stayed unchanged.
- Grouped **10,000 synthetic update entries** with **500 infohashes** and exactly 10,000 selected unique files in **40ms on one local run**. This is a microbenchmark, not a production speed guarantee.
- Tested eight classes of malformed persisted batch metadata and a failed atomic journal save, without mutating an original-file sentinel.
- Previously merged NSZ GPU loader-only Eden smoke: **PASS**, 24 seconds, 1296×759, 256 sampled unique colors, private emulator profile removed.

## Verification matrix

| Check | Result |
| --- | --- |
| NSL `cargo test --locked --all-targets --quiet` | **234 PASS / 0 FAIL / 59 ignored**, 293 total |
| Vendor NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS / 0 FAIL / 3 ignored** |
| Rust fmt / Clippy with `-D warnings` | **PASS / PASS** |
| Generated multi-game torrent E2E | **PASS**, 2,097,152 bytes exact, one torrent session |
| Real 346MiB trusted CNMT identity/version E2E | **PASS**, source unchanged |
| Synthetic 10k catalog update grouping | **PASS**, one-run measurement 40ms |
| Stage 24 WSL quota overflow → Retry → 12MiB exact | **PASS**, real WSL-backed Windows error **31**, not 112 |
| Stage 23 cross-process crash/restore regression | **3/3 PASS** |
| Stage 22 live pwrite and duplicate remove | **3/3 PASS** |
| Stage 21 bitfield / session JSON failure | **2/2 PASS** |
| Stage 20 locked-delete and journal error | **3/3 PASS** |
| Eden already merged NSZ GPU loader-only | **PASS** |

## Protected data and Release

- Original `E:\NSL_QA_20261120\small\base.nsz` SHA-256: `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F`.
- Original `E:\NSL_QA_20261120\small\patch.nsp` SHA-256: `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942`.
- Eden evidence: `E:\NSL_QA_20261120\small\eden_loader_stage25_multi_update.{json,png}`. Only test-owned process/profile and copied keys were cleaned, not other emulator instances.
- Stage 25 target: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Optimized `cargo build --locked --release`: **PASS**, 3m 37s.
- EXE bytes: **22,332,416**.
- EXE SHA-256: `036785F10FB62D2B9F5E02B59C57580E609E81BC1531372AF2C747275315D5FB`.
- Native Windows GUI: **3/3 PASS** at 980×720, 1280×800 and 1456×900; title `NSL  7f7c8d9`, private test profile cleaned (`GUI_NATIVE_TEMP_CLEAN=YES`).
- Existing protected `dist\nsl.exe` SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423`.
- Existing protected `dist\nsl-win64.zip` SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.
- Only the standalone Release executable in `target\release` is rebuilt, not the protected distribution.

## Reproduction (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
cargo fmt --all -- --check
cargo test --locked --all-targets --quiet
cargo test --locked -p nsrp-core --lib --quiet
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage25_two_games_one_torrent_selected_files_and_originals_preserved -- --ignored --nocapture --test-threads=1
$env:NSL_STAGE25_PATCH_QA = 'E:\NSL_QA_20261120\small\patch.nsp'
$env:NSL_STAGE25_BASE_ID = '0100F7301AAB6000'
cargo test --locked stage25_real_patch_cnmt_rejects_cross_game_and_nonnewer_version -- --ignored --nocapture --test-threads=1
cargo test --locked stage25_ten_thousand_catalog_matches_group_in_one_pass -- --nocapture
powershell -NoProfile -ExecutionPolicy Bypass -File '.\scripts\qa_stage24_quota_e2e.ps1'
cargo test --locked stage23_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage22_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage21_ -- --ignored --nocapture --test-threads=1
cargo test --locked stage20_ -- --ignored --nocapture --test-threads=1
cargo build --locked --release
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
```

## Stage 26 limitations and priorities

1. **Native Windows `ERROR_DISK_FULL` 112 and disconnected SMB E2E remain unverified.** Windows user is non-elevated and no isolated quota NTFS VHD/SMB instance was mounted. The genuine WSL tmpfs exhaustion produces Windows **error 31**. Do not fill shared user drives.
2. Test multiple separately signed **real DLC CNMT packages** through cumulative NSZ/XCZ merge, cancellation and restart. Stage 25 covers one real patch's CNMT and generated multi-game *downloads*, not full multi-DLC gameplay.
3. Sustained many-GB throughput and memory/I/O measurements on isolated samples; 10,000-row grouping is a CPU microbenchmark only.
4. New XCI/XCZ loader-only acceptance requires authorized correctly signed retail-compatible inputs; preexisting merged NSZ GPU smoke cannot prove newly generated unsigned XCI bootability.

## Next chat instruction

`D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE25_FINAL.md` 기준 Stage 26을 이어가. 전용 격리 NTFS/SMB 파일시스템에서 오류 112와 연결 장애, 실제 DLC 다중 CNMT 누적 병합·취소 복구, 다수 다운로드 메모리·I/O를 우선 검수해. 기존 게임 원본·키·세이브 및 다른 에뮬레이터 인스턴스는 변경하지 마.
