# NSL Stage 18 final handoff - 2026-11-21

## Repository and baseline

- Windows source root: `D:\NSW.dev\NSL`
- Remote: `https://github.com/UzuCore/NSL.git`, branch: `main`
- Previous handoff: `HANDOFF_2026-11-21_NSL_STAGE17_FINAL.md`, baseline `cc1abe71fca3f6298e4e3c84ac2afe9467c0e613`.
- **Stage 18 tested source commit:** `83fe2f01fc6c59db273817ba1c52a4d90c65364c`.
- Detailed QA: `docs\qa\QA_NSL_STAGE18_CATALOG_RECOVERY_AND_TORRENT_UX_2026-11-21.md`.
- Raw local output logs are intentionally ignored by Git in `docs\qa\*LOCAL*.txt`.

## Changes delivered

1. Catalog cache checksum (SHA-256 of exact source URL, items and ETag) stored with an atomic JSON update. Old snapshots are readable but cannot send unverified conditional ETags. Corrupted snapshots are rejected.
2. HTTP 304 snapshot race: revalidate disk after an unchanged response; if removed or corrupted during the request, retry one unconditional GET, and never present corrupt JSON as already up-to-date. A failed HTTP 503 leaves the previous verified snapshot unchanged.
3. ETag validation rejects malformed and embedded-quote validators; catalog Title ID mapping only accepts exactly 16 hexadecimal digits before base/update/DLC folding.
4. Store torrent-file selection: in-dialog retry after metadata lookup failure, no payload transfer until confirmation. Guard against duplicate dialogs and preserve Escape/Cancel behavior.
5. Downloads screen: retry only a failed transfer with an existing managed torrent; reuse its persisted output and restart the watcher; reject simultaneous retries. Metadata-only failures require a fresh preview. Distinguish sustained zero *connected* peers from confirmed swarm unavailability.
6. Headless GUI and unit regression cases cover cache damage, HTTP 304 replay, HTTP 503, bad IDs/ETags, retry-eligible rows, no-engine error dialogs and non-overlapping controls.

## Verified outcomes

| Audit | Status |
| --- | --- |
| NSL `cargo test --locked --all-targets --quiet` | **221 PASS, 0 FAIL, 44 ignored** |
| vendored NSRP `cargo test --locked -p nsrp-core --lib --quiet` | **119 PASS, 0 FAIL, 3 ignored** |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| Live public HTTPS catalog metadata read-only | **7,352 entries, 6,915 unique folded base IDs, 0 duplicate BTv1 hashes, 0 invalid nonempty Title IDs** |
| Real 7,344-entry local-cache store filter & two-frame UI paint | 8.4-20.0 ms filter, 4.0-14.5 ms two-frame render |
| Real read-only SQLite title metadata audit | 6,855 / 6,913 base IDs matched, 0.117 s |
| Live catalog save/reload and conditional HTTP 304 unchanged local cache | PASS; 7,352 entries |
| 4 MiB synthetic loopback torrent pause/restart/resume and delete preservation | PASS; exact bytes |
| 16 MiB synthetic loopback seeder outage and resumed transfer | PASS; exact 16,777,216 bytes in 38.92 s |
| Publicly licensed Sintel subtitle-only real peer transfer | PASS; 1,514 bytes match official HTTPS |

Results from `#[ignore]` tests were executed *explicitly*, not included in the normal 221 PASS count. Neither public metadata comparison nor synthetic tests prove an actual commercial game torrent can be resolved, merged or emulated.

## Stage 18 release

- Build location: `D:\NSW.dev\NSL\target\release\nsl.exe`.
- Build source commit embedded in binary: `83fe2f0`.
- Release build SHA-256: `5033E81691AFECB46F3CD92B89CB73B321A7F9D9E23096C733B0F1049D114E16`.
- Release binary size: **22,200,320 bytes** (build succeeded in 5m 10s with LTO).
- Native GUI isolated portable-window smoke: **PASS**; title `NSL  83fe2f0`, responsive at **980x720 / 1280x800 / 1456x900**; temporary profile removed (`GUI_NATIVE_TEMP_CLEAN=YES`).
- GitHub `main`: source commit + this handoff documented on remote; verify the final ref using `git ls-remote origin refs/heads/main`.
- Existing `dist\nsl.exe` and `dist\nsl-win64.zip` are protected; their Stage 18 prebuild hashes were `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423` and `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B`.

## Scope boundaries and next priorities

1. The new error-retry path is covered by unit/headless GUI tests; actual terminal managed-torrent failure recovery needs a dedicated intentionally faulted loopback integration test. Temporary peer outage recovery is already verified.
2. The catalog alone does not identify current patch/DLC payload versions. Bulk update/DLC matching and merge must continue to validate exact downloaded CNMT content/version before touching copies, never guess from display names.
3. Continue licensed real XCI/XCZ read-only compatibility checks where independently sourced isolated fixtures exist. Do not run external emulators concurrently with user instances; no modification of existing emulator profiles, saves or keys.
4. Check repeated network error UX in real Windows GUI with no private torrents, including rapid retry/cancel and slow or missing metadata.
5. If performance regressions arise with substantially larger catalogs, profile real datasets before changing caching or pagination.

## Reproduce safely

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked -p nsrp-core --lib
cargo test --locked stage18_live_catalog_id_and_duplicate_release_audit -- --ignored --nocapture --test-threads=1
cargo test --locked stage18_real_catalog_store_filter_and_lazy_paint_latency -- --ignored --nocapture --test-threads=1
cargo build --locked --release
Get-FileHash 'target\release\nsl.exe' -Algorithm SHA256
powershell -ExecutionPolicy Bypass -File '.\scripts\qa-native-gui-window.ps1' -ExpectedCommit '83fe2f01fc6c59db273817ba1c52a4d90c65364c'
```

**Do not delete/change** `E:\NSW`, local library databases, keys, running emulators or the existing `dist` package. For risky game-file tests use an isolated temporary copy and compare the original file hashes.

Next chat: `D:\NSW.dev\NSL\HANDOFF_2026-11-21_NSL_STAGE18_FINAL.md` 확인하고 Stage 19 남은 과제부터 이어가.
