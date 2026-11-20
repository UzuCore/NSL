# NSL Stage 9 - XCZ compatibility no-change fast path and cancellable XCI merge I/O

2026-11-21 KST. Stage 8 baseline: `b8d9404` on `UzuCore/NSL` `main`.

## Scope and safety

Optimize unnecessary staging I/O in the optional XCZ compatibility route, and make the native XCI merged-output writer cancellable within large individual NCA/gamecard regions. Keep all operations confined to a disposable workspace. The user's goal is **successful game loading only**; do not claim gameplay or long-term FPS stability.

The authorized `E:\NSL_QA_20261120` fixture set contains no independently verified **Nintendo-signed retail XCI/XCZ with a matching update**. Synthetic HFS0/XCI wrappers are *not bootable retail test evidence*. No original game under `E:\nsw`, key file, or preexisting emulator process may be modified.

## Implementation

- `vendor/nsrp-core/src/ops/xcz.rs` and `xcz_streaming.rs`: consolidate the small-index/NCA-header **transform eligibility preflight** in `preflight_xci_change`. Both streaming and compatibility routes now agree on which gamecard content actually needs to be transformed, including optional keys and encrypted NCAs.
- `NSRP_XCZ_COMPAT_STAGING` compatibility path previously copied every NCA/NCZ to intermediate files and reconstructed nested/root HFS0, even when no usable NCAs or NCZ were present. It now bypasses all intermediate extraction and HFS0 rebuilding for this **unchanged** case; copies the original gamecard **once** into a unique destination-side temporary, verifies exact length and cancellation, then publishes via the Stage 8 no-clobber transaction. It preserves noncanonical card-header bytes exactly and emits progress during the copy.
- The default streamed XCZ path already had a no-change fast path; this stage **does not claim** a new speedup for that default case. The improvement is specifically the compatibility fallback, consistency of preflight logic and removal of unnecessary disk writes.
- `src/xci_merge.rs`: replace opaque `std::io::copy` of large `.nca`/non-secure partitions with **cancellation-aware, range-checked 2MiB blocks** using one reusable buffer for the entire nested XCI output. Reject pre-cancelled jobs before the output file is created; check cancellation after nested HFS0 building and before syncing the HEAD. The caller continues to own and clean its private XCI staging directory on failure.

## Regression tests

- Compare default and compatibility no-change XCZ output byte-for-byte with the source, even with a noncanonical synthetic HEAD.
- Confirm a compatibility compression job with no usable keys retains the source, reports zero compressed NCAs, and leaves no intermediate files.
- Validate native XCI copy of a >7MiB deterministic source with multiple 256KiB blocks, cancellation without extra writes, out-of-bounds failure before writing, and pre-cancel rejection without creating an output.
- The full NSL/NSRP test suites, strict Clippy, format checks, actual release GUI smoke and commit publication are final acceptance gates.

## Opt-in actual-data QA

`vendor/nsrp-core/src/ops/xcz_stage9_real_qa.rs` constructs **a synthetic nested gamecard** from an isolated original NCZ in `E:\NSL_QA_20261120\small\base.nsz`, using private temporary files only. It decompresses the same real NCZ through **both** the compatibility and streaming XCZ implementations and compares full SHA-256 of the resulting reconstructed NCA payload, counts and lengths. It also checks that the source NSZ remains SHA-256 unchanged, and deletes all private temporary files via RAII. It never invokes Eden with an unsigned synthetic XCI.

```powershell
cd D:\NSW.dev\NSL
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
cargo test --locked -p nsrp-core isolated_real_ncz_staged_xcz_matches_streamed_decompression --lib -- --ignored --nocapture --test-threads=1
```

## Outstanding constraints

No measured **overall** faster real game merge is claimed without a controlled before/after benchmark, and no signed-XCI emulator loading is claimed until a matching, authorized sample exists. The existing NSZ game loading acceptance from Stage 7 remains applicable because Stage 9 does not change the NSZ loader path.

## Final QA and deployment

### Isolated real-NCZ XCZ comparison

The `isolated_real_ncz_staged_xcz_matches_streamed_decompression` opt-in test completed with **1 PASS, 0 FAIL**, using **171,859,243 bytes** of actual NCZ from the isolated Horror Tale 2 NSZ. Both reconstruction paths produced an NCA of **398,606,336 bytes**, with **full reconstructed NCA SHA-256 equality** and the source NSZ SHA-256 unchanged. Compatibility staging elapsed **127.930 seconds**, and the default streaming path **63.584 seconds** in this sequential fixture (entire opt-in test 253.26 seconds). This is a **comparison of two existing paths**, *not* a Stage 9 before/after speedup, retail XCI game boot, or a hardware FPS result. The unique private test workspace and all its copied gamecard/NCZ/NCA files were removed on completion.

### Full test and static-analysis results

| Gate | Result |
| --- | --- |
| NSRP `cargo test --locked -p nsrp-core --lib -- --test-threads=4` | **115 passed, 0 failed, 3 ignored** (118 registered) |
| NSL `cargo test --locked --all-targets -- --test-threads=4` | **201 passed, 0 failed, 30 ignored** (231 registered) |
| XCZ no-change compression with missing keys, byte-preservation | **PASS** |
| XCZ no-change cancellation triggered during bounded copy | **PASS**, no output or private partial survives |
| Native XCI >7MiB copy, cancellation, invalid regions and pre-cancel | **PASS** |
| Nested XCI and XCZ BKTR modes (existing regression suite) | **PASS** |
| NSL and NSRP `cargo fmt ... --check` | **PASS** |
| NSL and NSRP strict Clippy `-- -D warnings` | **PASS**, zero warnings |

### Optimized release and GUI acceptance

- Pre-commit `cargo build --locked --release`: **PASS**, optimized compilation completed in 4m 17s. A SHA-matched portable copy launched its own Windows GUI, survived eight seconds, and showed a valid main-window handle; its private directory and test-owned process were removed.
- **Important:** `build.rs` embeds `NSL_BUILD_COMMIT` from the Git HEAD into the executable version label. Therefore the **pre-commit binary is not the release**. Rebuild the executable **after** committing the Stage 9 code and this document so the version label matches its committed source.
- The final post-commit binary location is `D:\NSW.dev\NSL\target\release\nsl.exe`. Record the *post-commit* executable's SHA-256, size, Git HEAD, actual GUI window title, smoke test verdict and origin remote-HEAD match in the ignored local integrity report `D:\NSW.dev\NSL\docs\qa\QA_NSL_STAGE9_RELEASE_INTEGRITY_LOCAL_2026-11-21.txt` (not included in the Git commit to avoid a self-referential SHA/commit cycle).
- File-safety check **PASS**: `E:\NSL_QA_20261120` had **zero** Stage 9 temporary XCZ directories, **zero** `.partial` remnants and **zero** temporary key files. All original library files and emulator instances were left untouched.
- GitHub: the final Stage 9 revision should be the `UzuCore/NSL` `main` HEAD. Check `git rev-parse HEAD` against `git ls-remote origin refs/heads/main` after pushing; avoid a self-referential commit hash inside this document.

## Suggested next stage

Stage 10 can target the larger *merged NSP intermediate* in the XCI/XCZ update pipeline and the additional full-container pass required for XCZ recompression. Do not remove the intermediate until CNMT selection, title keys, gamecard content hashes and safe no-overwrite publication can be verified end to end. Retail signed XCI/XCZ game loading remains unverified because no matching authorized sample exists in the QA corpus.
