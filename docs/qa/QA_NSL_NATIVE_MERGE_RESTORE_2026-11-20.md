# Stage 2: NSRP-native merged NSP/NSZ/XCI/XCZ output and batch recovery

Date: 2026-11-20 KST. Base NSL source revision: `d8e13ed`.

## Implementation inventory

| Workstream | Result |
| --- | --- |
| Native NSRP package-processing modules | Vendored MIT source in `vendor/nsrp-core/` as a path dependency. No NSRP GUI or bundled helper executables; root NSL builds without an installed sibling NSRP directory |
| Content merge | NSRP `merge_packages` performs CNMT-driven base/update/DLC selection and writes a temporary NSP. No filename-only union, no dropping previous DLC blindly |
| NSP original | Publish a newly merged NSP, never overwrite the original |
| NSZ original | Compress merged NSP to real NCZ-containing NSZ (`compress_nsp`); reject zero actual compressed NCAs |
| XCI original | Build native nested HFS0 secure partition from selected NSP contents, preserving original gamecard template and other partitions. Recompute nested hashes and HEAD root hash/size fields |
| XCZ original | Decompress XCZ to private XCI template, build native merged XCI, recompress to NCZ-containing XCZ (`compress_xci`) |
| Output gate | Re-read selected base and patch CNMT from finished NSP/NSZ/XCI/XCZ; require exactly one patch ID at/above requested version. No output is published if validation fails |
| Atomic publication | Same-directory private work, separate output filename, hard-link publication when supported or `persist_noclobber` fallback. Existing destination and original packages remain unchanged |
| Disk and concurrency | Require free-space headroom before merge. Run **one merge/NCZ recompression worker at a time** to limit scratch I/O and memory contention |
| Batch state | Save a bounded `batch-updates.json` journal atomically without magnets, torrent metadata, key material or payloads |
| Crash/restart recovery | Restore incomplete jobs, reconcile librqbit's new session key by its stable output directory, re-verify true completed files, resume one merge at a time. Detect stale/failed restore and offer a user-triggered merge retry |
| Library refresh | Defer an incremental scan until any already-running scan completes, so newly published merged files are included |

## Strict safety and operating conditions

- This implementation **never patches or deletes existing original game archives**. It also preserves downloaded update files. A successful file is distinct (`[NSL Merged] [BaseTitleID][vVersion].{nsp,nsz,xci,xcz}`) and is only published once verified.
- When an installed game is `.nsz` or `.xcz`, the output is genuinely compressed via NSRP's NCZ writer; NSL does not relabel `.nsp` or `.xci` bytes.
- An update filename and the catalog's version alone are insufficient: NSL reads actual post-download CNMT before considering a merge. On restart it also checks that the original and downloaded update file lengths match the original queue manifest.
- NSRP's native readers handle supported key/encryption layouts. The optional external `hactool`, `hactoolnet` and `hacbuild` fallback is deliberately **disabled** in the batch pipeline. Unsupported or rights-managed NCAs requiring unavailable keys produce a failure and preserve the inputs.
- XCI/XCZ builds preserve source gamecard signatures rather than generating new Nintendo signatures. The new nested HFS0 and ROOT hash are structurally verified, **not** a substitute for hardware acceptance.
- A journal is not a proof of download completion. The user's actual librqbit completion report, or a restored finished session entry, is required before merge. Crashed workers can re-verify completed package contents; incomplete transfers do not trigger merge.
- Scratch/output space is checked conservatively; large games may require substantial free space. No temporary file/keys are placed in the user's game library except final same-directory staging needed for atomic publication. Staged workspaces are automatically cleaned on drop.

## Test matrix

| Validation | Evidence / outcome |
| --- | --- |
| Existing NSL tests | Rust default suite; run full command below |
| NSL full Rust test suite | **193 PASS / 17 ignored / 0 FAIL**; Clippy all targets `-D warnings` **PASS** |
| Vendored NSRP engine tests | **100 PASS / 2 ignored / 0 FAIL** with `cargo test --locked -p nsrp-core --lib -- --test-threads 4` |
| CNMT gating | Base required, patch required at requested version, duplicate patch rejected |
| Output kinds | NSP/NSZ/XCI/XCZ extension and parent preserved, malformed IDs rejected |
| No-clobber publication | Existing destination remains byte-for-byte unchanged; valid new output is published without removing source |
| XCI nested writer | Synthetic HEAD + HFS0 partitions, ROOT hash and secure contents rebuilt; template and intermediate NSP unchanged |
| Journal | Save/load atomic round-trip, interrupted worker reset, traversal rejection, previous manifest retained if validation rejects new data |
| Restored torrent identity | Simulated old `t1` session re-associated with new `r42` job by actual output directory without initiating partial-download merge |
| Real network update download | **Not yet tested against external peers**; no claims about network availability or catalog freshness |
| Real 4GB+ NSZ/XCZ patch merge and re-compress | **Still needs read-only source, independent output inspection, and emulator/console QA**. Synthetic tests do not establish full gameplay support |

Local commands:

```powershell
cd D:\NSW.dev\NSL
cargo fmt --all -- --check
cargo test --locked --all-targets -- --test-threads 4
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -p nsrp-core --lib -- --test-threads 4
cargo build --locked --release
.\scripts\qa-native-gui-window.ps1 -ExpectedCommit (git rev-parse --short HEAD)
```

## Dependencies and reference licenses

Native NSRP processing sources were copied from the owner's local `D:\NSW.dev\NSRP\rust_port\src` at development time into `vendor/nsrp-core/`. The MIT notice is preserved at `vendor/nsrp-core/LICENSE.txt`. `vendor/nsrp-core/README.md` lists the adapted `Cargo.toml`, `lib.rs` and safe `workspace.rs`. NSL's XCI merger is a separate native implementation using NSRP's MIT HFS0 writer. TorrentShopNX GPL source was studied as behavioral reference only and is **not imported**.

## Remaining acceptance work

1. Run real **NSP+update → NSP**, **NSZ+update → NSZ**, **XCI+update → XCI**, and **XCZ+update → XCZ** cases with source packages that can be independently inspected. Compare actual NCA IDs, versions and hashes; inspect output after decompressing NSZ/XCZ.
2. Test pause/cancel/restart at download, native merge, compression and final publication boundaries, using temporary copies rather than writing into the user's real game folders.
3. Confirm emulator loading and (if available and authorized) real Switch support for signed and rights-controlled sources. Do not mistake a successful synthetic HFS0 test for full runtime compatibility.
4. Validate long-running and large-library memory/disk behavior (large torrent metadata, hundreds of queue entries, and genuine disk-full conditions).
