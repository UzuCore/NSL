# NSL Stage 16 — real BitTorrent peer to native batch merge

Date: 2026-11-21 KST. Baseline: Stage 15 `003252a7f1881f446c7c68fdeded11efcdc8c269`.

## Objective

Verify the only missing network leg of the Stage 15 batch handoff without contacting public peers, seeding a user's original game file, or changing existing emulator instances. Use authorized read-only game fixtures copied to a disposable test directory; transfer the patch as *real TCP BitTorrent pieces* from a private 127.0.0.1-only seeder, track the download in the application's actual UI/batch runtime, then merge the downloaded NSP into an NSZ and verify content and original-source integrity.

## Findings and fixes

- Existing `src/torrent_e2e_tests.rs` loopback tests already validate pause/resume, session persistence, temporary seeder outage, nested torrent metadata previews and selected-file transfers. All **3/3 passed** in Stage 16 (57.21 seconds). The seeder outage test verified **16,777,216 bytes** round-trip after temporary unavailability, without treating the outage as fatal.
- **Real bug** in `src/app/update_batch.rs`: a 100% torrent `progress.finished` snapshot could launch native verification/merge **before** the downloader emits its real success notice and pauses/releases the Windows file handles. The implementation now waits for `EngineNotice::Download{result: Ok(())}`, which durably sets `transfer_finished=true`, before scheduling verification. Restarted jobs whose journal already records a successful transfer still resume immediately.
- Updated `src/runtime_tests.rs` to verify that a snapshot reporting 100% without the corresponding notice **never** starts verification, and that the actual completion notice correctly schedules one serialized native worker for a grouped DLC batch.
- All test-only `TorrentEngine` listeners now bind to `127.0.0.1:0`, disable DHT and local service discovery, matching the existing isolated localhost seeders. This ensures test fixtures are never advertised or served to other network hosts. Production network settings are **unchanged**.
- `local_file_seeder` chooses a 512 KiB piece size for 64 MiB+ fixtures and retains 32 KiB for the existing small-file QA. The larger piece size avoids tens of thousands of tiny pieces during the authorized 346 MiB NSP transfer.
- New explicitly ignored opt-in E2E `real_patch_loopback_torrent_to_restarted_batch_and_native_merge`: read-only Horror Tale 2 base NSZ (172,777,547 bytes) and update NSP (346,119,104 bytes), both copied to a disposable Stage 16 directory under `E:\NSL_QA_20261120`. Uses real magnet metadata preview, the **actual NSL selected-file queue**, and real TCP loopback peer transfer. The transfer completed in **9.50 seconds**, `real_peer=true`, with a persisted completion journal reloaded before native merger execution.
- **Full real-peer-to-native-merge E2E PASS (1/1).** The received **346,119,104-byte** NSP was handed directly to NSL's post-download worker after the engine success notice, producing **330,474,468-byte** NSZ. Both original game and update source SHA-256 remained unchanged; native output SHA-256 was `68f0da09dce6ab7ed44ad58e5b556ba12f51bdae02db3caaf7788cb5c27d79bc`. Transfer-to-merged-result time was **139.39 seconds**, full fixture, peer, metadata, merge and cleanup test **178.11 seconds**. The resulting NSZ size matches the previously verified Stage 4 / Stage 15 native output. QA-only copied sources, downloaded NSP and native merge output were removed by scoped cleanup, leaving **zero** `stage16-local-peer-real-*` directories.

## Acceptance boundaries

The Stage 16 test exercises *real BitTorrent peer I/O* over loopback, not public internet connectivity or store tracker/seeder availability. The prior Stage 14 test already confirmed a real merged Suika Game 29-DLC NSZ at its Eden GPU-rendered title/menu screen. No gameplay or DLC activation beyond the title is required. Stage 16 tests do not use real public magnet peers or modify any user's original game library.

## Commands and evidence

```powershell
cargo test --locked torrent::localhost_tests:: -- --ignored --nocapture --test-threads=1
cargo test --locked real_patch_loopback_torrent_to_restarted_batch_and_native_merge -- --ignored --nocapture --test-threads=1
cargo test --locked --all-targets
cargo test --locked -p nsrp-core --lib
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

Local QA evidence:

- `docs\qa\QA_NSL_STAGE16_LOOPBACK_PREEXISTING_TORRENT_E2E_LOCAL_2026-11-21.txt`
- `docs\qa\QA_NSL_STAGE16_REAL_PEER_E2E_LOCAL_2026-11-21.txt`
- Subsequent all-target regression, release and GUI integrity logs live in the same local QA directory.

Private QA folders and real-content copies are removed at test completion by scoped tempfile cleanup. Any result marked **PASS** must be grounded in the final test log, not merely a 100% torrent snapshot.

## Source regression

- `cargo test --locked --all-targets`: **211 PASS / 0 FAIL / 40 ignored** (the new real-peer test is explicitly opt-in and passed separately).
- `cargo test --locked -p nsrp-core --lib`: **119 PASS / 0 FAIL / 3 ignored**.
- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `git diff --check`: **PASS**.
- `E:\NSL_QA_20261120` contains **zero** Stage 16 private test directories after the actual 346 MiB peer-to-merge test. Original real-game samples were not modified or replaced.
- Repeated the three previously existing loopback-only tests **after** the test-listener hardening and real-peer E2E: **3/3 PASS, 0 failures**. This covers temporary local seeder outage/recovery, nested selective file transfers, and client pause/restart/resume/deletion isolation.
