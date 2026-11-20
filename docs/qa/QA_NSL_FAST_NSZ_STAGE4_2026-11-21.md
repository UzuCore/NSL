# NSL Stage 4 - NCZ passthrough fast NSZ update merge

Date: 2026-11-21 KST. Starting revision: `ebc083f`. Workstation: Windows/MSVC.

## Scope and input safety

- Only disposable authorized game package copies under `E:\NSL_QA_20261120` were read for real-package tests. The existing Stage 3 verified NSZs were used as read-only hash references, never overwritten.
- The originals in `E:\nsw` and `E:\nsw\_update`, installed emulator instances, `prod.keys`, and `title.keys` were not modified.
- All new real QA tests are explicitly ignored by the normal Cargo suite and run only with opt-in environment variables. `tempfile::TempDir` removes private scratch output at test completion.
- Existing NSRP/NSL legacy merge continues to handle NSP, XCI, XCZ, and uncommon NSZ structures; this acceleration is for NSZ base + NSP update only.

## Implementation

`src/update_merge/fast_nsz.rs` selects the highest CNMT version for the requested base TitleID, retaining its Base, latest Update and any selected DLC. It rejects missing mandatory NCA and CNMT, skips optional download `DeltaFragment` (content_type=6) and includes the tickets/certificates for the selected titles.

For base NCZ files, the original byte ranges are copied directly from NSZ PFS0 input to the merged NSZ using `Pfs0Source::Region`; they are **not decrypted, re-encrypted, recompressed, or transformed**. The update NSP is compressed once, and those NCZ entries are streamed into the same PFS0 output, preserving plain metadata NCA and associated ticket/cert bytes. Content is deduplicated by logical `.nca` identity, irrespective of `.ncz` physical encoding.

`src/update_merge.rs` invokes the fast path only when applicable, validates resulting CNMT and version before atomic no-clobber publish, and uses a size-aware free-space preflight without requiring giant plaintext files under `LOCALAPPDATA`. Unsupported input layouts return `None` for the pre-existing full NSRP path; post-selection errors fail hard rather than disguising corrupt results as successful fallback.

## Real-package comparison

| Game / isolated TitleID | Fast NSZ merge only (seconds) | Old Stage 3 NSZ (bytes) | Fast NSZ (bytes) | Size change | Verification |
| --- | ---: | ---: | ---: | ---: | --- |
| Horror Tale 2 (`0100F7301AAB6000`) | **103.216** | 537,040,546 | **330,474,468** | **-38.5%** | Base+Update NCA full SHA-256 **8/8 exact** vs Stage 3; CNMT PASS |
| UFO 50 (`01004F90239BA000`) | **48.618** | 481,544,718 | **407,630,791** | **-15.4%** | Base+Update NCA full SHA-256 **8/8 exact** vs Stage 3; optional DeltaFragment handled; CNMT PASS |
| Five Hearts Under One Roof season2 (`0100994026AF6000`) | **500.982** | 8,889,191,832 | **8,875,720,549** | **-0.15%** | **PASS**: 12 PFS0 entries, 2 NCZ, Base+Update CNMT version correct. **Stage 4 structural-only** (no new 8 GiB NCA SHA scan) |

The time values above time **fast assembly plus update compression**, excluding full dual NSZ decompression, NCA hash checks and optional emulator loading. Full SHA-256 for small/UFO compares the decompressed fast output against the **already SHA-256 verified Stage 3 merged reference**, so it detects any NCA byte changes and covers the actual original NCA contents. The large case deliberately limits the new QA to structural metadata and PFS0 checks, due to the prior 1,356.34-second full Stage 3 round-trip hash test; a new full NCA hash comparison cannot be claimed unless rerun.

Stage 3's earlier **947.88-second** large merge used the prior full inflate/extract/recompress path. The Stage 4 end-to-end fast merge-to-output assembly took **500.982 seconds**, an observed **47.1% shorter** elapsed merge/encode phase, and produced an output **13,471,283 bytes** smaller. Compare them as full-pipeline practical observations only: storage/CPU load and differing compression paths make this a non-controlled microbenchmark. The Stage 4 large structural test itself passed in **501.21 seconds**, without decompressing 8.8 GiB again solely to rerun the hash regression.

## Regression, build and deployment

- `cargo fmt --all -- --check`: **PASS**.
- `cargo clippy --locked --all-targets -- -D warnings`: **PASS, zero warnings**. Its initial invocation identified `try_merge_nsz_fast`'s excessive argument count; inputs were consolidated into a shared `FastNszRequest` type and the strict static gate then passed.
- `cargo build --locked --release`: **PASS**. Final executable: `D:\NSW.dev\NSL\target\release\nsl.exe` (22,006,272 bytes), SHA-256 `AA9ECACD134880E4B43B4246520578982568928E3829B6AD7A1CD0EFB3FCBB23`.
- The final executable was copied to an isolated temporary portable directory, opened a responsive GUI window (`MainWindowHandle` nonzero), and remained alive for seven seconds; only the specifically launched QA GUI process was stopped and its temporary directory deleted.
- A normal suite started concurrently with a long real QA unit-test executable failed to link on Windows (`LNK1104`: test EXE in use). This was a **Windows executable file lock**, not a test assertion failure or production source diagnostic. Retrying sequentially after real QA exited yielded a clean full pass.
- Final `cargo test --locked --all-targets -- --test-threads=4`: **195 PASS, 0 FAIL, 26 ignored**, 221 registered, 3.41 seconds for the normal suite.
- The two synthetic fast NSZ tests passed (verbatim NCZ and metadata passthrough, missing mandatory NCA and cancellation). Opt-in Horror Tale 2 and UFO 50 full NCA hash equality tests and the 8.8GB structural fast-path case also passed.
- Independent opt-in **full production backend path** `real_package_fast_nsz_end_to_end_publish` on isolated copies of Horror Tale 2: **1 PASS, 0 FAIL**, 141.26 seconds. Produced 330,474,468-byte NSZ, verified atomic publication, recovery of an already published output, refusal to overwrite on a repeated merge request, and unchanged **SHA-256 of both original source and update copy**.
- Both successful real-package QA tests and the end-to-end publication test cleaned their private temporary work trees. The previously validated Stage 3 sample inputs and outputs remain present and unchanged.

## Remaining limitations

- Original compressed base NCZ bytes are preserved verbatim but the fast path does not necessarily run a fresh full zstd decompression and per-NCA SHA verification on every user invocation. Structural/index and final CNMT validation should not be advertised as a content-integrity cryptographic scan. Refer to the full-hash QA above for actual equality evidence.
- The older full merge retains support for NSZ updates / XCI / XCZ input and unsupported layouts. Retail signed XCI/XCZ sources and independent Nintendo Switch hardware compatibility have not been validated in Stage 4.
- The earlier Eden loader smoke tests in Stage 3 do not establish menu interaction, extended play, or hardware acceptance of a newly produced Stage 4 binary.

## Reproduce opt-in tests

Set `NSL_QA_ROOT` to the isolated input-tree root, `NSL_QA_BASE` to an **isolated** `.nsz`, `NSL_QA_PATCH` to an **isolated** `.nsp`, and the base title ID / version from the actual matching CNMT in `NSL_QA_BASE_ID` / `NSL_QA_VERSION`:

```powershell
cd D:\NSW.dev\NSL
cargo test --locked real_package_fast_nsz_equivalence -- --ignored --nocapture
```

Without `NSL_QA_STRUCTURAL_ONLY=1`, set `NSL_QA_EXPECTED` to the full path of the known, independently verified Stage 3 NSZ to run full NCA SHA-256 comparison; leave `NSL_QA_STRUCTURAL_ONLY` unset otherwise. For publication and recovery with a smaller input, run `cargo test --locked real_package_fast_nsz_end_to_end_publish -- --ignored --nocapture` using the same environment. Tests only use private copies and automatically delete private temporary results.
