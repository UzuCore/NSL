# NSL Rust port

The Rust desktop application is in `src/` and uses the existing `library.db`, `config.json`, `titledb/`, and `bin/` files beside the executable. Debug builds resolve data files from this source directory; release builds resolve them beside `nsl.exe`.

## Build and run

```powershell
cargo run
```

For a packaged release, run `build-rust.bat`. It compiles the release binary, then `package-rust.ps1` assembles a clean staging tree and emits `dist-rust/` plus `nsl-rust-win64.zip`.

### Headless scan

The release binary accepts a scan mode so a build can be verified without the GUI:

```powershell
nsl.exe --scan fast    # incremental pass, reuses cached records
nsl.exe --scan full    # full pass
```

It prints one summary line and exits non-zero when the scan cannot start (no scan folders, missing `prod.keys`) or when any file fails analysis.

```text
scan=done mode=full files=846 base=800 update=710 dlc=3137 title_db=0 failures=0 elapsed=66.5s
```


The package contains only:

- `nsl.exe`
- `bin/nstool.exe`, `bin/hactool.exe`
- `titledb/*.json` (whichever local title database files exist)
- no `library.db`: NSL creates an empty database on first launch, so an update can never overwrite a user's collection
- `nsl-rust-manifest.json`
- empty `icons/` and `screenshots/` folders

Packaging refuses to continue if the archive would contain `prod.keys`, `title.keys`, **any database**, or any local configuration. The Rust updater accepts only the project's HTTPS GitHub release URL and checks the per-binary SHA-256 hashes in the package manifest. It atomically replaces **only `nsl.exe`, `bin/nstool.exe`, `bin/hactool.exe` and the manifest**, retaining settings, SQLite database, local title database, download session and caches. The update helper attempts rollback if a replacement fails. Attach the ZIP to the corresponding GitHub release to enable in-app updating. This checksum guards package integrity but is not a publisher signature.

## Port coverage

- The SQLite schema matches the Python application's `game_files`, `file_contents`, and `title_db` tables, including the NACP-column migration.
- The library view resolves database/localized names, content grouping, versions, owned/missing DLC, searchable and sortable columns, optional paging, list/grid modes, per-folder tabs, missing-update/DLC tabs, and row context actions.
- Installed and latest versions are shown as the raw Nintendo update number with a `v` prefix (`524288` → `v524288`), matching the original library convention. The number is Nintendo's `N << 16` update ordinal and is deliberately **not** decoded into `x.y.z`: doing so produces versions the game never shipped.
- Folder scanning supports fast and full passes, the original fast MD5 sampling scheme, reuse of persisted scan results for unchanged files, configurable parallel scanning, progress and cancellation, ignored folders, NSP PFS0/HFS0 entries, XCI root/secure partition parsing, tickets, CNMT/XML, direct NACP entries, `nstool` CNMT/NACP extraction, hactool CNMT fallback for NSP/NSZ, and hactool NACP fallback for NSP/NSZ/XCI/XCZ. Multi-game XCI bundles that expose only update/DLC records derive the base title id, matching the Python build. A cancelled pass keeps existing records instead of pruning files it has not reached.
- Pruning is scoped: records are only deleted underneath scan folders that were reachable during the pass. A disconnected drive or unmounted share keeps its library entries instead of silently wiping them.
- Local title database parsing/merge, remote title database download, CSV export, detail display and Google-backed Korean description translation, missing-update/missing-DLC filters and exception IDs, single-file and filtered bulk NSRP-style name normalization, icon/screenshot downloads and image caching, manual local icon extraction, unused media cache cleanup, all 14 source palette colors, configurable emulator launching, and optional NSZ/XCZ conversion/auto-conversion through the external `nsz` command are implemented.
- The title database keeps the catalog's nameless patch rows: they carry only a version, and folding them into the matching application row recovers the newest update for 195 games that previously reported "no update" or an older one.
- The updater checks the latest GitHub release and installs only the Rust-specific ZIP after manifest validation and explicit confirmation.
- Duplicate base titles are listed for review, and library reset clears database records only after confirmation while preserving game and media files.
- Settings changes are persisted to `config.json`; the scrollable settings drawer supports up to five scan folders, name priority, title database source, theme, launcher options, media prefetch, scan worker count, optional file-list paging, start maximized, sash behavior, and update/DLC exception IDs. The Python app's six settings pages are combined into one scrollable drawer.
- The port includes the Python keyboard shortcuts, confirmation before closing with active work, cache status, an empty-library welcome panel, and application information.
- The selected-game panel has the four file/game/screenshots/DLC tabs and remembers the last tab. A log window records scan/cache hits, title database operations, media work, conversion, and update actions.
- The status bar shows the same RAM/CPU meter as the Python build and the window uses the bundled application icon.

## Verification

Run the automated tests with:

```powershell
cargo test
```

The standard suite covers:

- synthetic PFS0/CNMT and NACP parsing, plus the CNMT size window that hid encrypted metadata archives
- update-number formatting that shows the raw `v<N>` value
- nameless patch rows folding their version into the matching application entry
- XCI base-title derivation for bundles that only contain update/DLC records
- title database merging and updater version ordering
- database grouping, DLC status, rename/prune behavior, scoped pruning that keeps unreachable folders, and config persistence
- Windows path normalization and cached-record reuse without localized NACP names
- a headless egui run of the real application: library load, grid/list switching, all six settings categories, all four detail tabs, dialogs, search by name/filename/title id, and the folder/DLC filter paths

Three checks are ignored by default because they need the local corpus:

```powershell
cargo test scans_small_real_nsp_and_nsz_against_library_records -- --ignored
cargo test full_scan_of_real_corpus_matches_existing_library -- --ignored
cargo test incremental_scan_reuses_cache_after_full_scan -- --ignored
```

The first compares the smallest registered NSP and NSZ on-disk fast hashes and parsed title id, version, and content type against the existing library. The second performs a real full scan of every scan folder into a scratch database and asserts the exact reference totals (800 base, 710 update, 3,137 DLC content rows across 846 files) with zero analysis failures and a populated fast hash/cache entry for every file.

### Historical Rust port checklist (prior to November 18 hardening)

- `cargo build --release`: clean, no warnings.
- `cargo clippy --all-targets`: no warnings.
- `cargo test`: 24 passed, 3 ignored (corpus checks).
- `cargo test full_scan_of_real_corpus_matches_existing_library -- --ignored`: passes against the local corpus in ~63 s and asserts exact reference totals.
- `cargo test incremental_scan_reuses_cache_after_full_scan -- --ignored`: cold 69.1 s vs incremental 0.37 s, with identical totals and no dropped records.
- The packaged `nsl.exe` was extracted to an empty folder and run headlessly: `--scan full` reported `files=846 base=800 update=710 dlc=3137 failures=0` in 66.5 s, and `--scan fast` re-ran in 0.3 s with 0 pruned records.
- Running `--scan` without scan folders or without `prod.keys` exits 1 with a specific error message.
- Adding an unreachable `Z:/OfflineDrive` to the scan folders and running a full pass kept all 846 records, confirming scoped pruning.
- A `scan_schema` value in `config.json` records which scan metadata version produced the cached rows; bumping `SCAN_SCHEMA_VERSION` forces one full re-analysis so new columns get populated, then cache hits are trusted again (96.9 s once, 0.3 s after).
- The ZIP from `build-rust.bat` opens the empty-library welcome screen on first launch, creates `icons/` and `screenshots/`, and renders Korean text correctly.
- An idle instance reports `RAM: ~150 MB | CPU: ~0%`, confirming the icon missing-cache avoids the per-frame retry loop.

The Rust port still differs in presentation details, including the Python app's rich log formatting and theme-card settings page; settings are combined into a scrollable drawer.

### Desktop usability

Windows GUI smoke testing on 2026-11-18 exercised the actual application with a separate 846-file SQLite backup: library search/paging/sorting, list/grid and detail modes, seven settings categories, theme changes and persistence, store search/paging, download session settings, keyboard shortcuts, compact window layout, and restart. Pressing Escape clears both the edit box and committed search filters. The window defaults to 1280×760 and supports resizing down to 920×620; statistics move beneath the toolbar when space is constrained. Background activity no longer opens an intrusive floating log window automatically; use the **로그** toolbar control to view logs. See `QA_GUI_2026-11-18.md` for the detailed test matrix and untested scenarios.

### Reliability and performance hardening (November 18, 2026)

- Full-folder scan pruning is disabled for roots containing unreadable descendants; empty/truncated packages fail analysis instead of being reported as successful. Schema changes also trigger re-analysis during incremental scans.
- The headless scanner no longer persists disabled screenshot/icon download preferences. Title DB replacement uses one SQLite transaction; JSON config, catalog, title DB JSON files, and image caches are replaced atomically.
- Torrents use validated per-infohash subfolders, restore completed/paused jobs from child folders on restart, distinguish command acknowledgements from verified transfer completion, propagate transfer errors, and support cancelling the magnet metadata handshake. Clearing torrent session metadata is scheduled for the next launch before the engine starts, never while it is writing.
- The library render cache shares data with an `Arc` rather than cloning every result per frame; the list view and grid view virtualize visible rows. Store search indexes every catalog entry (no 2,000-row cap) and cover textures have a 128-entry LRU limit.
- Image downloads use a 32 MB network limit and a predecode pixel cap. Converting a new NSZ/XCZ no longer silently deletes previously converted files while a game may still be running.
- Unit tests cover automatic-update safeguards, config recovery, transactional DB rollback, schema re-analysis, corrupt archives, and download path containment. See `QA_FIXES_2026-11-18.md` for current execution results and unverified external tests.
