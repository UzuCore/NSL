# NSL Stage 17 — external catalog, HTTP 304 cache, licensed public torrent

Date: 2026-11-21 KST. Baseline: Stage 16 `73a0180403ceb03a840f7a7c2f083d60e758b9d9`.

## User acceptance scope

Confirm that NSL can reach the **actual external catalog**, parse/lookup and cache its data, and use **real public BitTorrent trackers/peers** for magnet preview and selective file transfer. Do not download, re-upload, or seed any copyrighted Switch game. The actual internet torrent uses a Creative Commons licensed example; existing original game assets, downloaded library, keys, settings and emulator processes remain untouched. Earlier Stage 14 already verified real native NSZ loading to a game's Eden title/menu screen.

## 1. Actual catalog HTTPS fetch, parsing, and identification

Live source: `https://raw.githubusercontent.com/Langegen/switch-game-collection/main/EN_catalog.json` (only the public JSON metadata is accessed, not its torrent payloads).

| Observation | Stage 17 result |
| --- | --- |
| HTTPS HEAD | HTTP 200 |
| Actual upstream response `Content-Length` | 22,926,273 bytes |
| Full NSL production downloader → parser → private cache → reload | **PASS** |
| Parsed catalog records | **7,352** |
| Entries with a usable base title ID | **7,004** |
| Distinct base title IDs | **6,915** |
| Entries with valid 40-hex-character BTv1 info hashes | **7,352** |
| First private serialized `catalog.json` without ETag optimization | 4,498,839 bytes |
| Updated private serialized `catalog.json` including ETag | 4,498,919 bytes |
| Actual first fetch + second conditional request + parsing verification | **1.29 sec** (live test run) |

The ignored opt-in test `stage17_live_catalog_download_parse_reload_readonly` uses `tempfile::TempDir` under `E:\NSL_QA_20261120` and deletes its private catalog when finished.

## 2. Catalog reload optimization and bug prevention

- New `CatalogState::etag` validator is persisted atomically inside the exact `catalog.json` payload it describes; the old file schema remains readable via defaulted optional fields. No sidecar metadata file is required.
- A refresh sends `If-None-Match` only if the saved validator belongs to the **same catalog URL**, and the ETag is safely bounded, quoted and free of control characters. On HTTP **304**, NSL returns the cached, already parsed catalog; it **does not** re-download the 22.9 MB response, rewrite the 4.5 MB cache or change file modification time.
- Tested both actual raw GitHub HTTP 304 and a deterministic isolated 127.0.0.1 two-request mock server. Both passed.
- Found and fixed an HTTP-library compatibility case: `ureq` 2.x may deliver HTTP 304 in the successful `Ok(response)` branch instead of `Error::Status(304)`. Both paths now share the same validated-cache handler. Without this repair the refresh attempted to parse an empty 304 body as JSON and failed.
- `app/store.rs` sends the **already parsed `CatalogState`** to the GUI instead of a count; `app/frame.rs` moves the state directly into memory instead of reading/parsing a second time on the UI thread. The status line now correctly distinguishes **“카탈로그 이미 최신”** from a new download. A headless GUI regression confirms that a background catalog result can be displayed **even when no saved catalog file exists**, proving the redundant disk reload was eliminated.
- Failed network refresh or unexpected 304 without a validated cache must **not** erase or overwrite the existing saved catalog.

## 3. Legal public torrent: actual magnet and peer

Open-license fixture: *Sintel* (Blender Foundation, CC BY 3.0), listed among WebTorrent's freely redistributable test torrents.

- Official HTTPS `.torrent` metadata: `https://webtorrent.io/torrents/sintel.torrent`, **20,792 bytes**, SHA-256 `4C8FDAD0414B4767546A0F92FE3D660A66EDC32471874E7A1CFA97120317B84A`.
- Native NSL `TorrentEngine::preview_files(magnet)`: public **magnet-only** metadata preview, **11 files** identified; metadata preview did not create payload files.
- Used real public UDP trackers and peer connections. The engine reported a **live peer snapshot** before completion; the complete downloader notice was received.
- Selected **only file index 1**, `Sintel.en.srt`, length **1,514 bytes**. The downloaded bytes matched the official HTTPS reference `https://webtorrent.io/torrents/Sintel/Sintel.en.srt` exactly.
- Public network integration test **1 PASS / 0 FAIL**, elapsed **3.79 seconds** for the final magnet-only preview plus selected peer download and HTTPS comparison.
- Read-only tracker reachability: `bt.t-ru.org` (the catalog's typical tracker hostname) and `tracker.opentrackr.org` both resolved in DNS and accepted TCP port 80 connections. This is a **host/network reachability check only**, not a copyrighted game's tracker announce or peer-availability test.
- Temporary downloads were created within an isolated `stage17-licensed-public-*` directory under the QA root and removed by scoped cleanup. No user's games were transmitted or modified.

**Boundary:** We have proven public tracker/peer connectivity using license-cleared content; no claim is made that a particular copyrighted commercial game's third-party torrent has enough seeds or is authorized for redistribution. The Stage 16 real-game merge used only a private 127.0.0.1 seeder, and the Stage 14 Eden title-loading check was not rerun in this stage.

## 4. Tests and release verification

The following commands are the Stage 17 regression gate; logs and official Release checksums are retained in `docs\qa\QA_NSL_STAGE17_*_LOCAL_2026-11-21.txt`:

```powershell
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo test --locked -p nsrp-core --lib
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage17_live_catalog_download_parse_reload_readonly -- --ignored --nocapture --test-threads=1
cargo test --locked stage17_public_licensed_sintel_selected_subtitle -- --ignored --nocapture --test-threads=1
cargo build --locked --release
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File .\scripts\qa-native-gui-window.ps1 -ExpectedCommit (git rev-parse HEAD)
```

The official preexisting `dist/nsl.exe` and `dist/nsl-win64.zip` must remain byte-identical. Only `target/release/nsl.exe` is rebuilt for the updated commit and GUI smoke-tested.

Stage 17 code regression: **NSL 214 PASS / 0 FAIL / 42 ignored**, **NSRP 119 PASS / 0 FAIL / 3 ignored**; Rustfmt, strict Clippy (`-D warnings`) and Git diff whitespace checks all passed. The two explicitly ignored real-network opt-in tests are exercised and logged separately. The headless UI regression for rendering a cache-hit result without a second JSON parse passed.

Final opt-in reruns after the complete source changes: **live catalog + actual HTTP 304 PASS in 1.50 sec**, **public magnet metadata + actual peer + licensed 1,514-byte subtitle download PASS in 4.42 sec**. All temporary Stage 17 catalog, peer-download and 20,792-byte test-metadata folders were verified and removed. No copied source archive remains.
