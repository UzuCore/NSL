# NSL Stage 7 - atomic PFS0 publication, bounded input reuse, and loader-only QA

2026-11-21 KST. Baseline commit: `d2a42bd` (NSL Stage 6).

## Scope

Harden the shared native PFS0 packagers used by NSP/NSZ compression and merge. Optimize source I/O without changing any NCA/NCZ/ticket bytes or the PFS0 on-disk representation. **Game acceptance remains normal loading in a separate Eden instance only**; no gameplay or extended play verification is requested.

## Changes

- Shared `vendor/nsrp-core/src/formats/pfs0.rs` `build_from_streams` and `build_from_sources` previously created a **fixed `<extension>.partial`**, and then explicitly deleted a same-named result before renaming. Repeated or overlapping jobs could truncate each other's staging files or erase an already valid result.
- Both writers now use a unique, RAII-managed `tempfile` beside the destination, flush and sync the completed output, and **atomically publish without overwriting** a previously existing destination. A competing result produced after initial preflight is rejected at publication time; the temporary file is deleted before returning an error.
- Failed or canceled writes clean up their private temporary file without touching the destination or other jobs. The malformed PFS0 input bounds and summed output size are validated before allocating a large output; string tables have a 64 MiB cap.
- For mixed direct-region and ordinary-file sources, up to **four reusable 2 MiB input buffers** are cached with simple LRU behavior. Repeated alternation between base and patch files no longer continually reopens the same large sources. The maximum number of cached readers is fixed, so memory usage stays bounded.

## Regression coverage

The new tests intentionally simulate: another writer publishing during streaming; two PFS0 writers contending for the same output; packaging directly onto its own source; ordinary region/NCA/ticket byte-for-byte preservation; interleaved sources with reader-cache eviction; invalid regions and canceled writes. All source integrity checks are performed against isolated temporary inputs.

## Actual isolated game QA

QA root: `E:\NSL_QA_20261120`. Never write original games under `E:\nsw`, original key files, or the user's existing emulator profile. Opt-in real repack reuses entries from the isolated Horror Tale 2 base NSZ, streams an independent output in a disposable workspace, and compares **every PFS0 entry's SHA-256**; it also confirms that attempting to overwrite the completed output is rejected without changing that file.

**Real PFS0 repack QA: PASS.** Isolated base `172,777,547 bytes`, **4/4 entries SHA-256 equal**, pure package copy **8.881 seconds**. The byte-for-byte result is temporary, and the stage also rejected an attempted overwrite of the completed result and a pre-cancelled package without leaving `.partial` files. This timing is specific to this file/drive and is not a measured speedup versus an unmodified baseline.

**Above-4GiB logical offset QA: PASS.** The first sparse-file test attempt showed that the local QA drive allocated approximately 4GiB of physical disk, contrary to the intended small-fixture requirement. This disposable test was explicitly stopped, its exact private directory removed, and free disk space restored. The final opt-in test instead uses a **virtual `Read + Seek` cursor based above 4GiB** to validate PFS0 index and bounds arithmetic, plus a direct-region rejection check against an intentionally short physical source. It passed with a fixture below 4KiB, **without a multi-gigabyte allocation**. This virtual-index test is not a real 4GiB high-offset file-copy benchmark.

## Rebuilt real merged game / loading-only verification

The isolated `base.nsz + patch.nsp` Horror Tale 2 case was fully merged with the new atomic stream writer and direct-region packager:

- **123.623 seconds** in the fast merge operation (includes update NSP compression and final PFS0 assembly); **330,474,468 bytes**, 10 PFS0 entries, 2 compressed NCZ. This is **not** a measured throughput gain relative to Stage 5's 111.245-second trial. CPU/disk load and the extra flush/sync step may vary.
- Final temporary game NSZ SHA-256: **`68F0DA09DCE6AB7ED44AD58E5B556BA12F51BDAE02DB3CAAF7788CB5C27D79BC`**. This is **identical to the independently verified Stage 6 merged output SHA-256**, confirming byte-for-byte package equivalence despite the shared writer refactor. The output's base/update CNMT selection also passed.
- The first Eden loader-only capture at **18 seconds** was blank while shader compilation was still active; this was recorded as a **failed observation, not a game compatibility failure**. Its diagnostic engine log did not indicate an NCA header error. The subsequent **38-second** isolated loader-only run passed.
- Actual Horror Tale 2 **title screen visible** in the independently inspected screenshot, **1296 × 759**, **241 distinct sampled colors**, loader and GPU renderer both observed, **0 engine Critical/Fatal entries**, **no NCA bad-header error**, and source SHA-256 unchanged. **No menu keys or gameplay inputs were sent.**
- Evidence: `E:\NSL_QA_20261120\stage7_loader_20261121\eden_loader_stage7_retry.png` and `.json`. The newly built **330,474,468-byte duplicate test NSZ** and private Eden executable, title/product key copies and emulator profile were safely deleted. **0 copied private key files, 0 private directories** remain.

## Full test matrix

| Gate | Result |
| --- | --- |
| NSL `cargo test --locked --all-targets` | **199 passed, 0 failed, 29 ignored** (228 tests) |
| Vendored NSRP `cargo test --locked -p nsrp-core --lib` | **107 passed, 0 failed, 2 ignored** (109 tests) |
| `cargo fmt --all -- --check`, vendored `cargo fmt` | **PASS** |
| Concurrent PFS0 publication, conflict cleanup, canceled output, source reuse | **PASS** |
| Real isolated NSZ entry SHA-256 equality | **4/4 PASS**, copy 8.881s |
| Virtual 4GiB+ PFS0 index/bounds, no large allocation | **PASS** |
| Actual merged NSZ package hash equal to verified Stage 6 result | **PASS** |
| Independent Eden real title-screen loading, no game controls | **PASS (38s)** |
| NSL `cargo clippy --locked --all-targets -- -D warnings` | **PASS, 0 warnings** |
| Vendored core `cargo clippy --locked -p nsrp-core --all-targets -- -D warnings` | **PASS, 0 warnings** |

## Release build and delivery

- `cargo build --locked --release`: **PASS**. Final executable `D:\NSW.dev\NSL\target\release\nsl.exe`, **22,027,264 bytes**, SHA-256 **`CE787508DF8EEAC24E9FDF8AA787E490969BF71C606C23389CD4FBE8D7A2E43D`**.
- Final executable copied to a unique disposable portable directory and independently SHA-256 compared; owned GUI process survived eight seconds and reported a valid Windows main-window handle. The test-owned GUI PID alone was terminated and its isolated temporary folder deleted. **PASS**.
- GitHub deployment: the committed Stage 7 source and this QA document belong to `UzuCore/NSL` on `main`. To confirm the final pushed commit and clean state without writing a self-referential commit ID into this file, run `git rev-parse HEAD`, `git ls-remote origin refs/heads/main` and `git status --porcelain` from the project root.

## Reproduction (opt-in on private QA copies only)

```powershell
cd D:\NSW.dev\NSL
cargo test --locked --all-targets -- --test-threads=4
cargo test --locked -p nsrp-core --lib -- --test-threads=4
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
cargo test --locked real_package_pfs0_atomic_repack_and_no_clobber -- --ignored --nocapture
cargo test --locked real_package_pfs0_atomic_4gib_logical_region -- --ignored --nocapture
```

The real game fast-merge/Eden loader reproduction additionally requires the already documented Stage 6 opt-in `NSL_QA_PATCH`, `NSL_QA_BASE_ID`, `NSL_QA_VERSION`, `NSL_QA_STRUCTURAL_ONLY=1`, and an **unused private** `NSL_QA_EDEN_EXPORT` path under the QA root. Run `scripts\qa_eden_loader_smoke.ps1` with `-RunLabel` and a suitably bounded `-ObserveSeconds`, without sending game controls. **Do not run tests on original production ROM or emulator instances.**

## Acceptance boundaries

- An NCZ structural header check does not replace decompressed cryptographic content verification. Independent end-to-end NCA hashes are documented for the earlier Stage 6 Horror Tale 2 output.
- A loader-only smoke does not certify game completion, 60 FPS stability or Switch hardware compatibility.
- Signed retail XCI/XCZ update acceptance remains open until a matching authorized source is available. Synthetic tests cannot substitute for retail verification.
