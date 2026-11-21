# NSL Stage 13 — real DLC cumulative merge and native CNMT / NCZ compatibility

Date: 2026-11-21 KST. Stage 12 baseline `f8288e87f4bd9c42529a07aec917b58a0377fdc2`.

## Purpose and isolation

Stage 12 integrated the Missing DLC tab with the existing selective torrent queue and native merger, but a matching real-data cumulative merge had not been exercised. This stage uses **read-only** game archives from `E:\NSW` with an isolated QA root at `E:\NSL_QA_20261120`, RAII temp directories, and a *no-external-tool* NCA/CNMT reader. The real game archive and 29 DLC package files must never be overwritten or moved. The official `dist` release, user database, keys and other emulator instances remain untouched.

### Real fixture

- Base game `0100800015926000`, NSZ, 1,372,476,535 bytes. It contains a Base CNMT, an Update CNMT, and **27 existing DLC CNMTs**.
- DLC directory contains **two genuinely missing DLC IDs** selected by native CNMT inspection: `010080001592701D` and `010080001592701C`, each an independent 124,112-byte NSP at version zero.
- A full cumulative test isolates a copy of the existing NSZ, merges DLC 1 into the isolated copy, then merges DLC 2 into the first published result. At every stage the result must remain a compressed NSZ, include Base + Update + all older DLC + new DLC CNMTs, and never overwrite an existing path. It also reopens already published outputs to exercise the production crash-recovery check. Sources are hashed before and after. This does **not** claim a signed Switch game-loader boot or live torrent peer download.

## Finding 1: native CNMT parser rejected a valid hierarchical PartitionFS layout

- Before: **13/29** real CNMT NCAs parsed natively. The other 16 were incorrectly sent to the disabled external-tool fallback.
- Cause: the native reader checked only section offsets `0` and `0x20`. In a hierarchical SHA-256 NCA filesystem, the FsHeader's declared PartitionFS offset and length can place PFS0 elsewhere inside the encrypted section.
- Fix: `vendor/nsrp-core/src/ops/native_meta.rs` reads the NCA FsHeader's declared PartitionFS extent (`FsHeader+0x40` and `+0x48`), bounds-checks the range, and uses legacy section-start probes only in the absence of a declared hierarchy. Unsupported/encrypted layouts fail closed.
- Result: **29/29** real CNMT NCAs parsed natively, no external process. Added synthetic tests for noncanonical offsets `0x2000` and `0x4000`, including rights-managed NCA and rejection of a malformed, overflowing declared extent.

## Finding 2: NCZ decoder rejected valid NCA encryption sections crossing the raw-header boundary

- Before: **14/29** NCZ entries passed the structural preflight; 15 small NCZ entries failed the span-overlap check during NSZ decompression.
- Real NCZ descriptor: one AES-CTR section starting at **`0xC00`**, size **`0x18000`**, crossing the NCZ raw NCA header boundary at **`0x4000`**. It does *not* overlap another section. NCZ's first 0x4000 bytes are stored raw and only its later bytes are decoded/crypt-processed. The old validator incorrectly required every declared *NCA crypto section* to begin at 0x4000 or later.
- Fix: `vendor/nsrp-core/src/formats/ncz.rs` allows a section beginning before 0x4000 to continue into the compressed body, still rejects actual inter-section overlap, integer overflow, unsupported crypto, and sections wholly inside the raw header. `formats/ncz_stream.rs` already applies encryption only to the intersecting section bytes, so no unsafe crypt shortcut was added.
- Result: **29/29** real NCZ layouts pass structural preflight. **27** small NCZ entries were actually decompressed, with reconstructed NCA length and raw header preservation checked. Added a positive synthetic crossing-header section test and negative true-overlap, header-only and arithmetic-overflow tests. Previously malformed NCZBLOCK cases remain rejected.

## Finding 3: native NSZ passthrough also supports missing DLC

- The original Stage 12 DLC wrapper disabled the optimized NSZ merger, so the first real-data attempt inflated the 1,372,476,535-byte source and assembled an intermediate approximately 1.72 GB NSP before attempting to recompress every unchanged NCA. This is unnecessarily expensive with 27 existing DLCs, especially when two missing DLCs must be merged sequentially.
- Extended `src/update_merge/fast_nsz.rs` to accept an optional **specific DLC Title ID**. The engine preserves Base, Update and all existing DLC CNMTs from the source, selects only the requested DLC from the patch, retains original compressed NCZ ranges verbatim, and compresses only a new NSP patch where necessary. The published result still must pass strict CNMT and prior-content preservation verification.
- A patch containing several DLC CNMTs must not install unselected DLCs. A dedicated unit test validates the exact target, original Update and original DLC retention, and unrelated-DLC exclusion. Unsupported encrypted metadata and malformed NCZ content remain rejected or use the existing checked fallback.
- The slow **isolated** QA process was stopped after confirming this avoidable recompression bottleneck. Its temporary workspace was cleaned, without modifying the original game or DLC files. The new optimized E2E runs separately.

## Test commands

```powershell
cd D:\NSW.dev\NSL
$env:NSL_STAGE13_SOURCE_ROOT='E:\NSW'
$env:NSL_STAGE13_BASE=(Get-ChildItem 'E:\NSW' -File | Where-Object { $_.Name -match '\[0100800015926000\]' -and $_.Extension -eq '.nsz' } | Select-Object -First 1).FullName
$env:NSL_STAGE13_DLC_DIR='E:\NSW\_dlc'
$env:NSL_STAGE13_QA_ROOT='E:\NSL_QA_20261120'
cargo test --locked isolated_stage13_real_missing_dlc_inventory -- --ignored --nocapture --test-threads=1
cargo test --locked isolated_stage13_real_ncz_section_inventory -- --ignored --nocapture --test-threads=1
cargo test --locked isolated_stage13_real_two_dlc_cumulative_nsz_wrapper -- --ignored --nocapture --test-threads=1
cargo test --locked -p nsrp-core --lib
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
```

## Local evidence and outstanding qualification

| Gate | Status |
| --- | --- |
| Real Base/Update/27 DLC inventory | PASS, 29 CNMTs |
| Two additional DLC identities and native v0 inspection | PASS |
| Real NCZ framing and small NCZ decompression | PASS, 29/29 layouts, 27 decoded |
| NSRP regression with strict malformed-layout checks and crossing-header roundtrip | PASS, 119 tests / 3 ignored |
| Fast NSZ selected-DLC-only candidate test and strict Clippy | PASS |
| Two-step full-size cumulative production NSZ merge | **PASS, 1/1, 338.64 sec**, including source copy/hash, two native merges, recovery, final source SHA-256 |
| Stage 1 published NSZ, CNMTs, DLCs | PASS, 1,372,516,150 bytes, 30 CNMTs, 28 DLCs |
| Stage 2 published NSZ, CNMTs, DLCs | PASS, 1,372,555,717 bytes, 31 CNMTs, 29 DLCs |
| Original source and both DLC NSP SHA-256 preserved | **PASS** |
| Private Stage 13 QA workspace automatically deleted | **PASS**, zero leftovers |
| NSL complete Rust regression | **209 PASS / 0 FAIL / 38 ignored** |
| NSRP complete library regression | **119 PASS / 0 FAIL / 3 ignored** |
| Rustfmt, Clippy `--all-targets -- -D warnings`, Git diff whitespace checks | **PASS** |
| Real torrent-peer download and Nintendo-signed loader boot | Not tested |

Local log files: `QA_NSL_STAGE13_REAL_DLC_INVENTORY_LOCAL_2026-11-21.txt`, `QA_NSL_STAGE13_NCZ_LAYOUT_INVENTORY_LOCAL_2026-11-21.txt`, `QA_NSL_STAGE13_REAL_TWO_DLC_E2E_LOCAL_2026-11-21.txt`, and `QA_NSL_STAGE13_NSRP_REGRESSION_LOCAL_2026-11-21.txt`.
