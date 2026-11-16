# NSL Rust port

The Rust desktop application is in `src/` and uses the existing `library.db`, `config.json`, `titledb/`, and `bin/` files beside the executable. Debug builds resolve data files from this source directory; release builds resolve them beside `nsl.exe`.

## Build and run

```powershell
cargo run
```

For a packaged release, run `build-rust.bat`. It creates `dist-rust/` with the Rust executable, bundled scan tools, local title database, a Rust manifest, and `nsl-rust-win64.zip`. Attach that ZIP to the corresponding GitHub release to enable the in-app updater. The updater ignores the Python `.exe`/ZIP assets and validates the Rust package manifest before staging replacement. The original PyInstaller build files remain available for the Python application.

## Port coverage

- The SQLite schema matches the Python application's `game_files`, `file_contents`, and `title_db` tables, including the NACP-column migration.
- The library view resolves database/localized names, content grouping, versions, owned/missing DLC, searchable and sortable columns, optional paging, list/grid modes, per-folder tabs, missing-update/DLC tabs, and row context actions.
- Folder scanning supports fast and full passes, the original fast MD5 sampling scheme, reuse of persisted scan results for unchanged files, configurable parallel scanning, progress and cancellation, ignored folders, NSP PFS0/HFS0 entries, XCI root/secure partition parsing, tickets, CNMT/XML, direct NACP entries, `nstool` CNMT/NACP extraction, hactool CNMT fallback for NSP/NSZ, and hactool NACP fallback for NSP/NSZ/XCI/XCZ. A cancelled pass keeps existing records instead of pruning files it has not reached.
- Local title database parsing/merge, remote title database download, CSV export, detail display and Google-backed Korean description translation, missing-update/missing-DLC filters and exception IDs, single-file and filtered bulk NSRP-style name normalization, icon/screenshot downloads and image caching, manual local icon extraction, unused media cache cleanup, all 14 source palette colors, configurable emulator launching, and optional NSZ/XCZ conversion/auto-conversion through the external `nsz` command are implemented.
- The updater checks the latest GitHub release and installs only the Rust-specific ZIP after manifest validation and explicit confirmation.
- Duplicate base titles are listed for review, and library reset clears database records only after confirmation while preserving game and media files.
- Settings changes are persisted to `config.json`; the scrollable settings drawer supports up to five scan folders, name priority, title database source, theme, launcher options, media prefetch, scan worker count, optional file-list paging, start maximized, sash behavior, and update/DLC exception IDs. The Python app's six settings pages are combined into one scrollable drawer.
- The port includes the Python keyboard shortcuts, confirmation before closing with active work, cache status, an empty-library welcome panel, and application information.
- The selected-game panel has the four file/game/screenshots/DLC tabs and remembers the last tab. A log window records scan/cache hits, title database operations, media work, conversion, and update actions.

## Verification

Run the automated tests with:

```powershell
cargo test
```

The standard suite covers synthetic PFS0/CNMT and NACP parsing, title database merging, updater version ordering, database grouping and DLC status, file rename/prune behavior, config persistence, Windows path normalization, and reuse of cached records without localized NACP names. The corpus check is ignored by default because it uses the local `library.db` and game files. Run it explicitly with:

```powershell
cargo test scans_small_real_nsp_and_nsz_against_library_records -- --ignored
```

That check scans the smallest registered NSP and NSZ, then compares their on-disk fast hashes and parsed title ID, version, and content type with the existing library records. It passed against the current local corpus.

The packaged `dist-rust/nsl.exe` was also launched against a temporary copy of the local database. Korean text rendered correctly; search, list/grid switching, game detail tabs, and the settings drawer responded. A quick scan of all 846 registered files completed with 0 missing records removed. The database still contained 846 files and 4,647 content rows afterward (800 base, 710 update, and 3,137 DLC rows). This run exposed and fixed mixed slash handling in Windows scan paths and cache reuse that depended on localized NACP names. Network-backed update/download actions were not exercised.

The Rust port still differs in presentation details, including the Python app's rich log formatting, resource-use meter, and theme-card settings page; settings are combined into a scrollable drawer.
