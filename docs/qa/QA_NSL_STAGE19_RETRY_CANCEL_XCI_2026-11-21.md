# NSL Stage 19 - Transfer retry/cancel race and game-card regression

## Safety and provenance

- Source root: `D:\NSW.dev\NSL`, starting from Stage 18 handoff commit `d9a76eec3f4f6d781a5774132be61e28c8557d23`.
- The network integration test uses a generated 6 MiB binary fixture, an in-process `127.0.0.1` tracker and peer, and its own disposable download directory. No public torrent tracker, commercial title or user's existing download queue is contacted.
- The fault injection is **NSL-watcher-level** and `#[cfg(test)]` only. It terminates the NSL watcher after pausing a genuine transfer, while retaining the verified data and managed torrent. It **does not** claim to reproduce librqbit's internal `ManagedTorrentState::Error` or a real disk-write failure.
- Real NSP/NSZ game-card tests are read-only on sources in `E:\NSL_QA_20261120\small`; all generated XCI and extracted content stay within a disposable test workspace.
- Do not touch `E:\NSW`, user keys/saves/settings, existing emulator processes, or the protected `dist\nsl.exe` and `dist\nsl-win64.zip` artifacts.

## Implementation

1. An opt-in test-only fault facility and an end-to-end generated transfer case exercise the existing managed-torrent retry path: partial download, terminal watcher error, retained progress, retry, full-payload byte equality, and safe delete-without-deleting-payload.
2. Two rapid retry commands are accepted at most once; the duplicate command must be rejected by the engine. A second interrupted transfer is retried and then immediately cancelled; its removed row must not reappear.
3. The production watch loop checks the cancellation flag while holding the progress-map lock, preventing a canceled row from being reinserted between `session.delete` and UI polling.
4. The retry path detects cancellation before and after the asynchronous unpause; cancellation removes its reservation without overwriting any later retry token.
5. User-visible failure messages use Korean pause/resume/retry/delete labels. A harmless duplicate-retry rejection does not overwrite the actual transfer's success status.

## Loopback E2E result

- `cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1`: **PASS**.
- Repeat after the production cancel/race fixes: **PASS**, previous transfer progress **1,507,328 bytes** at the injected error; accepted retry 1, rejected duplicate 1, no resurrected cancellation.
- Actual generated transfer: **6,291,456 bytes**, identical to seed after retry.
- Observed prior progress at injected failure: **1,474,560 bytes**.
- Retry commands: **1 accepted / 1 safely rejected**.
- Recovered download remains byte-identical after session removal with `delete_files=false`.
- Immediate retry+cancel of an independent row: row disappears and does not resurrect; previously completed file remains byte-identical.

## Reproduction

```powershell
Set-Location 'D:\NSW.dev\NSL'
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo test --locked -p nsrp-core --lib
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked stage19_terminal_watcher_failure_retry_duplicate_and_cancel_e2e -- --ignored --nocapture --test-threads=1
$env:NSL_QA_ROOT  = 'E:\NSL_QA_20261120\small'
$env:NSL_QA_BASE  = 'E:\NSL_QA_20261120\small\base.nsz'
$env:NSL_QA_PATCH = 'E:\NSL_QA_20261120\small\patch.nsp'
cargo test --locked isolated_real_nsz_patch_to_xci_without_intermediate_nsp -- --ignored --nocapture --test-threads=1
```

## Real isolated NSZ + NSP -> XCI QA

- `cargo test --locked isolated_real_nsz_patch_to_xci_without_intermediate_nsp -- --ignored --nocapture --test-threads=1`: **PASS**.
- Selected from independently verified CNMT metadata: **10 files, 745,610,688 bytes**.
- Final unsigned synthetic-template XCI: **745,615,360 bytes**, direct selected-file build **without an intermediate merged NSP**.
- Rust QA test elapsed: **123.24 s** (selected-file builder reported 68.113 s).
- QA original `base.nsz` SHA-256 before and after: `5CE8716B34565639ED93CFEB121CEE35DFAF89841CA566C2467EBDF47312D40F` (unchanged).
- QA original `patch.nsp` SHA-256 before and after: `E16091858ECECFB7A6563566340790DB22F03BFF9B097A5B293017842EAFD942` (unchanged).
- This is structural/card metadata QA, **not** retail signature or emulator boot verification.
- Read-only library DB real-card scan (`cargo test --locked native_xci_xcz_samples_from_library_database -- --ignored --nocapture --test-threads=1`): **PASS**. Two XCI samples contained 1 and 6 contents respectively; two XCZ samples contained 1 and 2. All four had a base title and native metadata, with Korean NACP titles present. User files were opened read-only.
- Independent **loading-only** emulator smoke of the pre-existing merged Horror Tale 2 NSZ: **PASS** in a disposable portable Eden profile. Observed **24 s**, GPU window **1296x759**, **261** sampled unique colors, loader+renderer logs present, no bad NCA header, **0 critical errors**. Original NSZ SHA-256 unchanged. No menu actions or gameplay attempted. Only the own QA emulator process was terminated; temporary copied key/profile directory removed. Evidence: `E:\NSL_QA_20261120\small\eden_loader_stage19_horrortale_boot.json` and same-name `.png`.
- That Eden sample is an earlier merged NSZ, **not** the temporary unsigned XCI created by the direct selected-file test. The new XCI has not been qualified for retail-signature or emulation boot.

## Additional checks to record

- [x] Read-only NSZ+NSP -> XCI selected-file integration and before/after input SHA-256.
- [x] Read-only real XCI and XCZ scanner test.
- [x] NSL: `cargo test --locked --all-targets --quiet` **221 PASS / 0 FAIL / 45 ignored** (266 total).
- [x] NSRP: `cargo test --locked -p nsrp-core --lib --quiet` **119 PASS / 0 FAIL / 3 ignored**.
- [x] `cargo fmt --all` and `cargo clippy --locked --all-targets -- -D warnings` **PASS**.
- [x] Isolated native GUI startup and three viewport sizes: **PASS**, `NSL  717d847`, **980x720**, **1280x800**, **1456x900**, isolated temporary profile deleted.
- [x] Release EXE SHA-256 **`F47433767F63E59C588AB96C1428A836F03135317477ABD89AF90C29D2B6C47D`** (22,201,344 bytes); original `dist\\nsl.exe` and `dist\\nsl-win64.zip` checksums remained unchanged.
- [x] Final GitHub `main` sync and clean local tree: verify with `git ls-remote origin refs/heads/main`, `git rev-parse HEAD`, `git status --porcelain=v1`.

## Unresolved boundary

- A genuine **librqbit internal fatal disk I/O error** cannot currently be injected through its public storage API: version 9.1.0 does not expose the underlying `ManagedTorrentShared` and `TorrentMetadata` implementation types needed for a third-party storage-factory fault wrapper. This remains a separate integration test, not a PASS claimed here.
- An unsigned synthetic gamecard test verifies packaging and CNMT consistency, **not** retail signature validity or successful emulator loading. Any emulator qualification needs an isolated user-owned bootable image and non-interference with other emulator instances.
