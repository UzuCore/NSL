# NSL Stage 10 — Direct CNMT-selected XCI/XCZ assembly without merged NSP

Date: 2026-11-21 KST. Baseline: Stage 9 `4c2e2707177d30bb62941f7683ebbd0154386627` on `UzuCore/NSL` `main`.

## Scope and user acceptance

Remove one large, redundant **merged `.nsp` package** from the **XCI/XCZ update-merge path** while keeping all native CNMT/content selection, individual NCA/ticket materialization, safe temporary workspaces, final metadata verification, and no-overwrite publication. Game acceptance remains **successful loader start only**, not gameplay or prolonged FPS tests. The supplied authorized QA corpus contains **no Nintendo-signed XCI/XCZ with a matching update**, so synthetic gamecard verification is clearly distinct from a retail boot check. Neither original games/keys nor other emulator processes are modified.

## Architecture changes

1. `vendor/nsrp-core/src/ops/merge.rs`: new `merge_packages_selected()` shares the **same private `merge_packages_impl()` CNMT selection path** as the existing NSP merger. The callback receives the sorted, materialized selected `(filename, path)` entries plus the **already prepared (decompressed) first input**, and runs *before* the NSRP temporary workspace is deleted. When a callback is used, the full-size intermediate PFS0/NSP is **not created**. Existing `merge_packages()` callers retain their previous output behavior.
2. `src/xci_merge.rs`: share one `build_merged_xci_from_sources` HFS0/HEAD writer between the original intermediate-NSP reader (retained **for test parity only**) and the new `build_merged_xci_from_selected_files` direct reader. Both use exactly the same partition layout, SHA-256 HFS0 header updates, gamecard HEAD recalculation, 2MiB buffer reuse, length/bounds checks and cancellation. The direct builder opens only the materialized selected file when writing that entry; no large merged NSP needs to be read.
3. `src/update_merge.rs`: for an XCI/XCZ target, use `merge_packages_selected()` and construct the XCI within the selected-file callback. **XCZ original gamecards reuse the `prepared_base` XCI** that NSRP already decoded: the previous extra `decompress_xcz(original, original.xci)` is removed. For XCZ output, the built XCI is then compressed and its NCZ count is verified as before. NSP/NSZ pathways and the already vetted NSZ fast path are unchanged.
4. Final published output still requires a nonempty package, the original game's CNMT plus exactly one sufficiently recent update CNMT, and the existing fail-closed no-clobber publication. A failing callback or canceled job never publishes the result. Selected files and any decrypted input remain in isolated temporary workspaces, not user-facing library locations.

## Disk-I/O consequences and limits

- Each XCI/XCZ merge no longer writes and re-reads a **second full-size merged NSP** after NSRP already materializes the selected raw NCA/ticket files. The size of that avoided NSP is approximately `sum(selected file lengths) + PFS0 header`, which is measured in the opt-in QA.
- XCZ source files no longer undergo the **extra full-container decompression** in the NSL wrapper: the same already prepared original-XCI instance is reused while the callback runs.
- The selected NCA files and, for compressed inputs, NSRP's **pre-decompressed input package** are *still* materialized. The final XCI remains a complete output and XCZ conversion still needs its own compression pass. Do not describe this as zero-copy or claim that all temporary I/O is eliminated.
- The existing conservative free-disk-space preflight is kept, rather than lowering safety headroom without observing representative real-world filesystems.

## Verification gates

| Gate | Expected evidence |
| --- | --- |
| Original merged-NSP XCI vs direct-selected XCI | Byte-for-byte equality with synthetic original, same HFS0/HEAD SHA-256 |
| Core and GUI full Rust regression suites | 0 failures |
| Strict Clippy/format across both crates | 0 errors/warnings |
| Opt-in actual isolated NSZ+NSP CNMT selector | Correct 2 CNMTs, no intermediate merged NSP, every selected NCA/ticket SHA-256 equal to final XCI leaf |
| Actual XCI/XCZ retail game loading | **Not verified** until a matching authorized signed sample is available |
| Optimized release executable | Build from **committed** source, isolated Windows GUI smoke, exact embedded HEAD, SHA-256, remote `main` match |
| QA hygiene | 0 private game duplicates, `.partial` files, copied key files or foreign emulator interactions |

## Opt-in actual-data QA

The new test `src/update_merge/stage10_real_qa.rs` uses the read-only **Horror Tale 2** game copies in `E:\NSL_QA_20261120\small`, creates a **synthetic unsigned XCI HEAD template**, selects the real Base+Update NCA/ticket files with NSRP, builds the output directly, verifies all selected file SHA-256 hashes against the final XCI leaves and validates both CNMT records. All full-size XCI test outputs are deleted via a private RAII temporary workspace. No synthetic XCI is passed to Eden.

```powershell
cd D:\NSW.dev\NSL
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
$env:NSL_QA_PATCH='E:\NSL_QA_20261120\small\patch.nsp'
cargo test --locked isolated_real_nsz_patch_to_xci_without_intermediate_nsp -- --ignored --nocapture --test-threads=1
```

**Actual-data selected-file test: PASS**, 1 passed / 0 failed. NSRP selected **10 entries / 745,610,688 bytes**; the unsigned XCI was **745,615,360 bytes**. Every selected file (NCA/ticket/cert) matched its final HFS0 region **SHA-256 exactly**, and the real Base and requested Update CNMTs were present. The callback observed **no extra merged NSP in the selected-entry directory**. Direct selected-CNMT/XCI assembly took **70.574 seconds**; complete QA including before/after hashes and cleanup took **123.90 seconds**. This is single-run timing, not a controlled previous-vs-new performance benchmark.

### Additional production-wrapper test (unsigned fixture only)

`src/update_merge/stage10_wrapper_qa.rs` constructs a private unsigned XCI from the same real NSZ's decompressed Base NCA/CNMT, then invokes the **actual user-facing `merge_verified_package`** API with the isolated update NSP. It tests the final XCI's CNMT, duplicate-publication rejection, and source-file immutability. No test gamecard is passed to Eden. Its decoded NSP, test template and result are automatically deleted.

```powershell
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
$env:NSL_QA_PATCH='E:\NSL_QA_20261120\small\patch.nsp'
cargo test --locked isolated_native_xci_update_wrapper_end_to_end -- --ignored --nocapture --test-threads=1
```

**Actual NSL production-wrapper test: PASS**, 1 passed / 0 failed. A synthetic unsigned XCI containing real decoded Base NCAs and CNMT was constructed inside the private QA workspace. `merge_verified_package` took **31.306 seconds** to merge the real isolated NSP update and publish a **745,615,360-byte XCI with 10 content entries**, and both Base and requested Update CNMTs were independently validated. A repeated merge of the same update was rejected **without changing the published result SHA-256**; the source XCI SHA-256 was also unchanged. The complete test (including initial 381MiB NSZ decode, synthetic source-card assembly, hashing and cleanup) took **148.75 seconds**. Every private test file was deleted after the test. This validates the real GUI backend on an **unsigned fixture only**, not signed retail XCI loading.

### Supplementary XCZ production-wrapper and recompression QA

`src/update_merge/stage10_wrapper_qa.rs` also includes `isolated_native_xcz_update_wrapper_end_to_end`, an explicitly ignored test of the **actual XCZ branch**. It decodes the isolated real Base NSZ, builds an unsigned gamecard, compresses it to an XCZ, then calls the production `merge_verified_package` with the real Update NSP. It checks final compressed `.ncz` leaves and Base+Update CNMTs, restores the merged XCZ to XCI, rechecks both CNMTs, rejects repeat publication, recovers an already validated output, and verifies source/output SHA-256 immutability. Its output and intermediate packages live only inside one RAII temporary directory; no synthetic gamecard is passed to an emulator.

```powershell
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
$env:NSL_QA_PATCH='E:\NSL_QA_20261120\small\patch.nsp'
cargo test --locked isolated_native_xcz_update_wrapper_end_to_end -- --ignored --nocapture --test-threads=1
```

**Actual XCZ production-wrapper QA: PASS**, 1 passed / 0 failed (468.09 seconds total). Starting from an unsigned synthetic gamecard containing the real decoded base NCA/CNMT files, the test created a **190,956,642-byte XCZ**, directly assembled a **745,615,360-byte merged XCI** inside the production wrapper, and published a **537,044,002-byte XCZ with one compressed NCZ** (the merge call took **205.134 seconds**, including XCZ output compression). Both Base and Update CNMTs validated in the published XCZ and again after XCZ-to-XCI restoration. The same output was protected against duplicate creation, recovery recognized the valid existing result, source/output SHA-256 remained unchanged, and the entire private QA workspace was removed. This **does not** establish retail-signed gamecard loading or a controlled comparative speedup.

## Final outcomes

| Acceptance gate | Result |
| --- | --- |
| Legacy merged-NSP writer vs direct XCI-selected-file writer | **PASS — exact binary equality** on synthetic XCI and two NCA entries |
| Real isolated NSZ+NSP CNMT selection → direct XCI | **PASS — 10 entries / 745,610,688 bytes selected** |
| Resulting unsigned test XCI size | **745,615,360 bytes** |
| SHA-256 of each selected file vs final XCI region | **10/10 PASS** |
| Actual Base and Update CNMT validation | **PASS** |
| User-facing NSL XCI merge wrapper, real content in unsigned template | **PASS — 31.306-second merge, no-clobber and source immutability** |
| User-facing NSL XCZ merge wrapper, real content in unsigned template | **PASS — output XCZ 537,044,002 bytes, one NCZ, restored XCI CNMT, no-clobber/recovery and input hashes** |
| Redundant full-size selected merged NSP | **Not produced on the XCI/XCZ route** |
| NSL normal `cargo test --locked --all-targets` | **201 passed, 0 failed, 33 ignored** (234 registered; includes the new opt-in XCZ case) |
| NSRP normal `cargo test --locked -p nsrp-core --lib` | **115 passed, 0 failed, 3 ignored** (118 registered) |
| Rust format and strict Clippy both crates | **PASS, zero errors/warnings** |
| Nintendo-signed retail XCI/XCZ emulator boot | **Not verified — no authorized matching retail sample** |

The removed merged NSP's estimated payload alone was **745,610,688 bytes (≈711 MiB)** for the real QA title, *in addition* to a small PFS0 header. This removes an unnecessary full-size write and subsequent read on the supported XCI/XCZ path; **do not claim a measured runtime speedup**, nor claim that pre-decompressed input NSPs, raw selected NCA files or final XCI/XCZ compression copies were removed.

To avoid publishing an executable with an outdated version label, the final release must be rebuilt **after committing** this source (NSL `build.rs` embeds Git HEAD). Record the post-commit `nsl.exe` SHA-256, actual GUI window title, release size, full Git commit, remote `main` match and final cleanup in an ignored local integrity report:

`D:\NSW.dev\NSL\docs\qa\QA_NSL_STAGE10_RELEASE_INTEGRITY_LOCAL_2026-11-21.txt`
