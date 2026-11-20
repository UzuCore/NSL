# NSL real-library scan, incremental cache and offline torrent regression

Date: 2026-11-20 (KST). Initial source commit: `041d7c2`. Verification runs locally on Windows with isolated test databases. Neither the game library `E:\NSW` nor the project's existing `library.db`, configuration, prod.keys, torrents, or downloads was modified. No GitHub Actions were dispatched and the published official release remains r22.

## Actual library scan and metadata parity

Executed the opt-in `runtime_tests::full_scan_of_real_corpus_matches_existing_library` test with `NSL_TEST_SCAN_THREADS=4` on the existing `E:\NSW` directory; outputs went exclusively to the `seeded_app()` temporary database.

| Check | Result |
| --- | --- |
| Files scanned | **846** |
| Content records | **4,647** |
| File paths compared against read-only production DB | **846** |
| Localized NACP names / Korean names | **815 / 542** |
| Differences from existing DB in localized names | **0** |
| Differences from existing DB in content IDs, versions, kinds | **0** |
| Failed file analyses | **0** |
| Cold full-test runtime (debug/test profile) | **62.35 seconds** |
| Aggregate worker scan / CNMT / NACP durations | 168.476 / 87.251 / 74.695 seconds |

The accumulated worker durations are **not** elapsed wall time and must not be added to the total. Read-only game data was not copied or altered. The deep scan tests a real corpus but **not** every possible XCI/XCZ format or Switch gameplay behavior.

## Incremental scan (before / after robustness fix)

Executed `runtime_tests::incremental_scan_reuses_cache_after_full_scan` in a new isolated scratch database for each run:

| Test condition | Full scan | Immediate incremental scan | Total files |
| --- | ---: | ---: | ---: |
| Before fix, warmed filesystem cache | **789.9683 ms** | **3.0974 ms** | 846 |
| After fix, warmed filesystem cache | **825.9446 ms** | **2.1844 ms** | 846 |

All game/base/update/DLC totals were preserved in both runs. These are **two observations** from warm test runs, not proof of a precise general speedup. In particular, comparing the above times to the 62-second earlier cold scan is inappropriate because the OS disk cache and competing load differ.

### Actual defect fixed: subsecond same-size modification

Previously the incremental scan skipped an existing file whose size matched and whose modification time differed by **less than one second**, without even checking the fast hash. A ROM rewritten to different same-size content 250 ms later could therefore keep stale metadata. The same threshold appeared twice in `src/app/scanning.rs`.

The two checks now use one `unchanged_scan_metadata` helper and require **exactly the same stored finite `f64` modification timestamp and size** to take the no-I/O cache path. Otherwise the existing fast-hash / re-analysis logic runs. This keeps the fast path for truly unchanged files while detecting observable subsecond filesystem timestamp changes. Filesystems that do not update modification time at all still require a manual full scan to detect same-size content changes; this change does not claim cryptographic content verification on every incremental scan.

Added `app::scanning::metadata_cache_tests::subsecond_timestamp_changes_require_fast_hash_verification` and a full isolated app integration `runtime_tests::incremental_scan_detects_same_size_rom_rewrite_within_one_second`. The integration creates a PFS0 fixture containing a `.cnmt.xml`, scans it, rewrites **the same number of bytes** with a different title ID and an explicitly set **250 ms later timestamp**, then checks that the incremental scan updates its SQLite title ID. Neither test uses a production ROM.

## Torrent/download and catalog checks

The two opt-in localhost-only torrent E2E tests passed in **17.06 seconds** combined. They generate their own bytes and use an isolated loopback seeder and HTTP tracker, with DHT disabled; no public trackers or user download folders are used:

- `nested_magnet_preview_downloads_only_selected_files`: torrent metadata preview, nested file selection, selected-only transfer and restart verification — **PASS**.
- `transfer_pause_restart_resume_delete_preserves_other_files`: 4 MiB transfer with real peer counts, pause, resume, restart from saved session, completion, deletion with and without downloaded-file removal, and neighboring-file preservation — **PASS**.

The local real catalog test succeeded: **7,344** catalog items, **6,913** distinct base title IDs, **6,855** matched SQLite metadata records, and **0.094 s** metadata query time. Title DB tests succeeded: **51,777** records, initial local parse **7.194 s**, isolated initial import **13.293 s**, unchanged verified-cache check **0.0076 s**, and three unchanged JSON file hashes correctly skipped. These are debug-test measurements, not release-profile benchmarks.

The opt-in updater check fetched the **already published manual r22 GitHub Release ZIP**, verified its embedded manifest/commit, and prepared it entirely in a private temporary directory: **PASS**. It confirmed no `library.db` or `prod.keys` was bundled. **No update was installed and no user data was changed.**

## Local verification and remaining work

```powershell
cd D:\NSW.dev\NSL
cargo test --locked --all-targets -- --test-threads 4
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --bin nsl full_scan_of_real_corpus_matches_existing_library -- --ignored --nocapture --test-threads 1
cargo test --locked --bin nsl incremental_scan_reuses_cache_after_full_scan -- --ignored --nocapture --test-threads 1
cargo test --locked --bin nsl torrent::localhost_tests:: -- --ignored --nocapture --test-threads 1
cargo build --locked --release
```

Build must be performed again **after committing source** so the executable includes the final `NSL_BUILD_COMMIT` value. The user's local test executable is always `D:\NSW.dev\NSL\target\release\nsl.exe`. The currently published r22 is unchanged. Remaining nonautomated checks include real desktop click/keyboard workflows, longer public-peer downloads in a dedicated isolated environment, a controlled disk-full case, cross-device network failures, independent full-file game/decompressor comparisons and a formally controlled repeated benchmark.
