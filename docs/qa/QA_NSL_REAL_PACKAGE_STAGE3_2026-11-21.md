# NSL Stage 3 — real NSZ update merge / BKTR / Eden interoperability QA

Date: 2026-11-21 KST. Starting revision: `26b0b1d`.

## Scope and safety

- Source files under `E:\nsw` and `E:\nsw\_update` were read only; game packages were copied to `E:\NSL_QA_20261120/{small,ufo50,large}` before merging.
- NSL, NSRP and Eden processes started by this QA were isolated. No existing emulator sessions were terminated. The source `prod.keys` and `title.keys` were not modified.
- Test-only portable Eden profiles had ticket-key settings adjusted **only inside those isolated profiles**; neither those copies nor the actual keys are stored in Git.
- `src/update_merge/real_qa.rs` holds opt-in (`#[ignore]`) real-package tests. The normal unit suite never processes game files.

## Engine changes

1. In `vendor/nsrp-core/src/ops/cnmt.rs` and `ops/merge.rs`, exclude CNMT `content_type=6` (optional `DeltaFragment`) from required NCA selection. Mandatory NCA missing checks remain intact. UFO 50 had four such delta entries that were absent from a valid materialized update NSP.
2. In `ops/merge.rs`, preserve source NSZ/XCZ NCA headers as-is during game merge; implicit restoration of canonical retail Rights IDs can change a working ticketless source into an unbootable one. Canonical/archive restoration remains available separately.
3. In `vendor/nsrp-core/src/keys.rs` and `ops/compress.rs`, reconcile an existing **already-decrypted** title-key DB entry to the ticket's **encrypted** representation only if an AES-ECB check under the available `titlekek` values establishes they are equivalent. This takes effect in the operation's in-memory KeySet only; `title.keys` and `prod.keys` on disk remain unchanged. A regression test verifies exact key-file byte preservation.
4. Keep complete CNMT and NCA content selection checks, original-file preservation, atomic publication, and no-overwrite behavior.

## Actual game tests

| Game | Input | Merge result | Full NCA SHA-256 round-trip | Isolated Eden CLI smoke |
| --- | --- | --- | --- | --- |
| Horror Tale 2, `0100F7301AAB6000` | NSZ base v0 (172,777,547 bytes) + NSP update v65536 (346,119,104 bytes) | NSZ `537,040,546` bytes, PASS | **PASS**: selected Base 4/4 + Update 4/4 | **PASS**: title loaded, process alive 20 seconds |
| UFO 50, `01004F90239BA000` | NSZ base + NSP update v458752 | NSZ `481,544,718` bytes, PASS | **PASS**: selected Base 4/4 + Update 4/4 | **PASS**: title loaded, process alive 16 seconds |
| Five Hearts Under One Roof season2, `0100994026AF6000` | NSZ base `8,798,701,364` bytes + NSP update v65536 `92,516,288` bytes | NSZ `8,889,191,832` bytes, PASS (947.88 sec native merge) | **PASS**: selected Base 4/4 + Update 4/4, merged NCA 8/8 (1356.34 sec full round-trip QA) | **PASS**: title loaded, process alive 24 seconds and shader translation started |

The sizes above are real filesystem bytes, not estimates. The large sample's two input QA copies were compared with corresponding source sizes and seven widely spaced 1MiB SHA-256 samples each; this **does not** substitute for a whole-file source-copy hash. Full NCA round-trip hashes are verified separately against each relevant source NCA, including content selected by CNMT.

The large output was also inspected independently using the pre-existing readonly `qa-native-package-index-readonly.py` structural reader: **12 valid PFS0 entries, 2 block-compressed NCZ entries**. This was independent of the NSRP engine and did not require extracting NCA payloads.

### Emulator failure that exposed the issue

- The first Horror Tale 2 output had a canonical retail Rights ID but showed Eden `Error 12`, `0007-000C: NCA bad header`. The original NSZ loaded in the identical environment. Copying only 32 changed header bytes back was **not** sufficient to recover loading.
- An independent repack of the original NSP using the same PFS0 writer successfully loaded for 14 sec. By contrast, an early plain merged NSP failed, excluding PFS0 serialization and NSZ encoding as the sole cause.
- The embedded ticket's encrypted title key disagreed with the stored value in `title.keys` for all three tested updates. However, AES decryption of the embedded ticket key under the matching `titlekek` yielded the stored value (small / UFO: `titlekek_12`; large: `titlekek_00`).
- The original key representation failed NCA Program BKTR plan verification, whereas ticket-key representation succeeded for all three real update NSPs (Horror Tale 2: 3 crypto spans; UFO 50: 427; large: 3). Replacing **only the isolated Eden profile's** corresponding title key made both small merged cases load; the large merged NSZ also loaded using its own isolated profile.
- The final Horror Tale 2 output was regenerated under the preserved-source-header policy and also loaded for 20 seconds.

## Regression and release validation

- Vendored NSRP core: 104 registered tests, including targeted `DeltaFragment` and in-memory title-key reconciliation regression cases; Clippy `-D warnings` passed.
- NSL main suite: **193 PASS, 0 FAIL, 24 ignored** (217 registered, including opt-in real-package cases), post-refactor. `cargo fmt --check` and Clippy `-D warnings` both passed with no diagnostics.
- Final `cargo build --locked --release` passed. The final standalone executable is `D:\NSW.dev\NSL\target\release\nsl.exe` (**21,957,632 bytes**, SHA-256 `481E807AF4FB24C3669E518DE54BF10409AE412E6CC7FC72BBAA4093CAF149EF`) and does not require an external NSRP installation.
- An isolated portable copy of the release executable matched the SHA-256 of the release binary, created a responsive Windows GUI with a nonzero main-window handle, and remained running after 8 seconds. Only the QA-launched instance was terminated. This is a startup smoke test, not a manual interactive UI workflow verification.
- No retail XCI/XCZ source was present in `E:\nsw` or the checked NSRP sample corpus. The synthetic XCI/XCZ regression tests are not a substitute for a real signed-card boot test.
- Eden CLI loading and process survival do **not** establish menu interaction, full-game play, long-run performance, or actual Switch-hardware compatibility. Those remain separate acceptance criteria.

## Reproduce (requires authorized local game copies and original keys)

Set `NSL_QA_ROOT`, `NSL_QA_BASE`, `NSL_QA_PATCH`, `NSL_QA_ID` and `NSL_QA_VERSION` to the appropriate isolated QA sample. Run selectively:

```powershell
cd D:\NSW.dev\NSL
cargo test --locked --bin nsl real_package_preflight_readonly -- --ignored --nocapture --test-threads 1
cargo test --locked --bin nsl real_package_merge_in_isolated_qa_folder -- --ignored --nocapture --test-threads 1
cargo test --locked --bin nsl real_package_nsz_roundtrip_nca_hashes -- --ignored --nocapture --test-threads 1
```

The merge test intentionally refuses output overwrites. Preserve or move previous QA results before re-running; never direct it into `E:\nsw`.

## Remaining work

All three NSZ cases passed **native game-update merge → NSZ recompression → full selected NCA SHA-256 comparison → isolated Eden loader smoke**. The large sample's extra full hash regression finished with `1 passed, 0 failed`; the SHA-256 comparison covered 4 Base, 4 Update and 8 merged NCA entries.

This stage's runtime and integrity acceptance is complete. Follow-up topics outside this stage's validated corpus are:

1. Optimize large NSZ compression throughput and output ratio (the 8GiB+ merge and round-trip QA are time-consuming).
2. Obtain an independent real XCZ/XCI plus matching update to check signed game-card boot and hash invariants on a physical-size source.
3. Perform longer interactive play and, when available, Nintendo Switch hardware acceptance. Eden CLI load alone cannot certify those behaviors.

## Temporary-file policy

- Failed legacy merged output repro cases, source/merged diagnostic NSP duplicates, and temporary exploration files (**4,526,985,467 bytes**) were removed from the dedicated small QA folder after observations were recorded.
- The independently compiled vendored-core target directory (~0.63GiB) and temporary vendored Cargo.lock were deleted; root project build files and its Cargo.lock were preserved.
- Isolated Eden key copies were removed after completing loader tests; the core project's keys and original game sources were not changed.
