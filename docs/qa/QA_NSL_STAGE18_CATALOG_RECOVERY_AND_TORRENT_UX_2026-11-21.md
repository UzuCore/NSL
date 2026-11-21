# NSL Stage 18 - Catalog resilience, torrent retry and metadata QA (2026-11-21)

## Scope and safety

- Source: `D:\NSW.dev\NSL` (Rust NSL + vendored NSRP core).
- All new HTTP failure tests use an in-process loopback-only server and a disposable `tempfile` directory.
- Live catalog tests fetch *metadata JSON only*. No commercial game torrent downloads, announce requests or public seeding were made by these tests.
- UI tests use synthetic progress and a headless egui context. The native GUI smoke test creates only a disposable portable NSL instance.
- Do not modify `E:\NSW`, user keys, saves, emulators, the existing `dist\nsl.exe` or `dist\nsl-win64.zip`.

## Implemented

1. `src/catalog.rs`: pin saved catalog content/source/ETag to a SHA-256 checksum within the same atomic JSON write; retain backward-compatible legacy reads without trusting their ETag. Reject checksummed snapshots if their contents changed. When an HTTP 304 arrives after the local snapshot changed/corrupted, retry exactly once with an unconditional GET. Reject malformed ETags and invalid-length title IDs.
2. `src/app/frame.rs`, `src/app/store.rs`: metadata error popup supports an in-place retry without transferring files; cancel/Escape still drops the pending request; block duplicate preview popup creation.
3. `src/torrent.rs`, `src/app/downloads.rs`: show a retry action on failed *managed transfers* only, not incomplete metadata-only requests. Reuse the persisted managed torrent, serialize retries by torrent key and restart its completion watcher. Display prolonged zero-*connected-peer* inactivity as a reconnection wait, not an unsupported statement about swarm availability. Keep pause/resume/delete actions.
4. `src/runtime_tests.rs`: protect error/preview UI and download button behavior with headless frame tests; measure local real catalog searching and lazy UI painting without changing library files.

## Local cache audit (read-only)

- Existing `titledb\catalog.json` is an **older** snapshot (7,344 entries) without the new checksum. It remains readable; the next successful refresh upgrades it atomically.
- Valid 16-digit title IDs: 7,002; no title ID: 342; invalid ID: 0; distinct folded base IDs: 6,913.
- Valid distinct BTv1 hashes: 7,344, no duplicates; maximum catalog releases per folded base: 3.

## Live public catalog metadata audit

- Live JSON entries: **7,352**; unique folded base IDs: **6,915**; unique 40-hex infohashes: **7,352**; repeated infohashes: **0**; invalid nonempty title IDs: **0**; missing IDs: **348**.
- No base ID had more than 3 catalog releases. These are catalog records, **not** proof of available peers, downloadable DLCs or the content of a torrent.
- Live metadata audit: 1 PASS / 0 FAIL, 1.554 s.
- Live production fetch + cache reload + unchanged ETag HTTP 304: **1 PASS**, 7,352 entries, saved cache 4,499,004 bytes, ~2.54 s. The file modification time and bytes remained unchanged on 304.

## Performance (warm local cache, isolated synthetic GUI)

| Search | Matches | Filter | 2 UI paint frames |
| --- | ---: | ---: | ---: |
| Empty | 7,344 | 8.412 ms | 14.508 ms |
| `mario` | 43 | 19.545 ms | 5.849 ms |
| `zelda` | 19 | 19.974 ms | 6.291 ms |
| Unmatched | 0 | 17.951 ms | 4.004 ms |

All timings are **single-run observations**, not a guaranteed upper bound or a benchmark against earlier builds.

The read-only SQLite backup and catalog ID matching test found **6,855** metadata matches among 6,913 cached base title IDs in **0.117 s**.

## Regression result

- `cargo test --locked --all-targets --quiet`: **221 PASS / 0 FAIL / 44 ignored**.
- `cargo test --locked -p nsrp-core --lib --quiet`: **119 PASS / 0 FAIL / 3 ignored**.
- Specific HTTP 304 damage/race, 503 preservation, invalid ID/ETag, retry eligibility and GUI retry tests are in the normal suite.
- Additional explicitly ignored network, read-only DB and synthetic torrent tests are documented in the local QA logs, not counted as normal-suite PASS.
- Loopback-only 4 MiB generated transfer: pause, resume, restart from saved session and delete-one-job-without-deleting-other-files **1 PASS**, with identical final bytes.
- Loopback-only 16 MiB generated transfer: pause the independent seeder for 3 s, verify in-progress job stays visible/nonterminal and recovers when seeder resumes, compare all output bytes: **1 PASS**, transfer+outage elapsed **38.92 s**.
- Publicly licensed **Sintel** subtitle-only external peer transfer: **1 PASS**, metadata preview and only 1,514 bytes of subtitle data downloaded, official HTTPS source matched byte-for-byte, actual peer/finished notification observed, **4.66 s**.
- `cargo fmt --all -- --check` **PASS** and `cargo clippy --locked --all-targets -- -D warnings` **PASS**.

## Remaining boundaries

- The catalog identifies potential matches, not the actual update/DLC contents. Auto-merge remains gated by verified package CNMT and version checks. No title is matched from guessed names alone.
- A failed transfer with no existing managed torrent requires a new metadata preview; only persisted managed torrents are retryable.
- Real peer outages, seed availability and the emulation of every XCI/XCZ are not inferred from catalog metadata. Stage 18 does not modify emulator profiles, original game files or releases in `dist`.

## Reproduction

```powershell
Set-Location 'D:\NSW.dev\NSL'
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo test --locked -p nsrp-core --lib
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage18_live_catalog_id_and_duplicate_release_audit -- --ignored --nocapture --test-threads=1
cargo test --locked stage18_real_catalog_store_filter_and_lazy_paint_latency -- --ignored --nocapture --test-threads=1
# The stage17_live_catalog_* test additionally confirms live HTTP 304 in a disposable directory.
```
