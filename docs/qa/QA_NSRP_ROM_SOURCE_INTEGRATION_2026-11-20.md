# NSL / NSRP: read-only ROM parser source integration (2026-11-20)

## Request and constraints

Inspect real runtime dependencies, inspect existing NSRP Rust sources, integrate useful ROM processing code into NSL, preserve NSL game library data, and commit/push **without building** (manual-build policy).

Project roots: `D:\NSW.dev\NSL`, `D:\NSW.dev\NSRP`. Source baseline: `2fb603c` (P2 modularization and current README, clean main). NSRP root has no Git metadata; its Rust source lives in `rust_port/src/`. No NSRP files were edited.

## Verified external tool status

The actual NSL ROM parsing and NSZ/XCZ conversion implementation uses native Rust code. There is **no active invocation** of `nstool`, `hactool`, `hactoolnet`, `hacpack`, or `nsz.exe`. A few source comments and a packaging cleanup list still mention legacy tools. The application does launch **OS Explorer**, a configured emulator executable, and **PowerShell** for secure self-update. Saying NSL runs no outside processes at all would be incorrect.

## Changes

- Adapted the bounded read-only **PFS0/HFS0** index validation and game-card discovery principles from MIT-licensed NSRP `rust_port/src/formats/pfs0.rs` and `hfs0.rs` into `src/container_index.rs`. The code uses no additional crate dependencies or external tools.
- Integrated a single parser into `scanner.rs`'s three previous table readers (plain CNMT/ticket/XML, NACP fallback, native NSP/NSZ encrypted NCA index) and `xci.rs`'s nested index reader. Removes multiple independent duplicated and less defensive header parsers. Maintains the native scanner's one-index-sharing arrangement for CNMT/NACP parallel reads.
- Enforces entry count, length and offset bounds, checked offset arithmetic, UTF-8 terminated safe names, duplicate name handling and named HFS0 partition last-sector tolerance (at most 512 bytes). The parsed data region is always read-only.
- Strengthens XCI/XCZ root detection with the `0x200` game-card HEAD location, absolute-byte/sector address candidates and the bounded 0x200-aligned conventional header fallback. Requires an HFS0 root containing a `secure` partition; does not scan all ROM content.
- Provides **strict** indexing for structural game-card reads and **best-effort valid-entry-only** indexing for metadata queries. The latter skips malformed entries instead of losing intact CNMT/NACP data if an unrelated PFS0 entry is damaged.
- Includes identical NSRP MIT notice at `licenses/nsrp-MIT.txt` and updates the new package staging, copying, allowlist and synthetic package verification fixtures. Older official ZIPs (without the new notice) remain accepted for legacy validation.
- Adds parser tests for nested HFS0, invalid offset/overflow, unsafe/duplicate names, corrupted unused entries, HFS0 padding, parent bounds, and new XCI root discovery. Tests are **present in source, not executed** because the user reserves builds for manual execution.

## Read-only corpus comparison — completed, no ROM modification

Used SQLite `mode=ro&immutable=1` for the local library catalog and opened ROM files only for bounded header reads. No keys, actual game payloads, extraction, conversion, writes, or executables were used.

| Check | Result |
| --- | --- |
| Initial NSP/NSZ header comparison | 136 available, 135 fully strict-valid; 1 partial |
| Full available NSP/NSZ database corpus | **875/875 indexed** |
| Full strict-valid PFS0 index | **874/875** |
| Partially valid PFS0 (unrelated entry extends beyond EOF) | **1/875**, valid entries remain usable for metadata |
| Available XCI/XCZ root HFS0 indexes | **14/14 strict-valid** |
| XCI/XCZ root `secure` and `normal` / `update` entries | **14/14** |

This is an **independent Python rule comparison against the on-disk headers**, not executed NSL Rust parsing, full NCA decryption, or a full scan parity result. The real file with a truncated entry was *not* modified or repaired. NSRP's entire BKTR/NSZ repacker and converter were deliberately **not** copied into NSL: they serve a separate application role, include additional failure modes and require independent QA.

## Static and packaging QA

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS, Rust syntax/format only |
| `git diff --check` | PASS |
| NSRP `LICENSE.txt` vs NSL `licenses/nsrp-MIT.txt` contents | PASS, text identical |
| Isolated ZIP verifier / new 7-file required package | PASS |
| Isolated ZIP verifier / older 6-file package | PASS |
| Rejection: invalid manifest, unexpected nested file, staged EXE mismatch, ZIP hash mismatch | PASS |
| Source-only read-only ROM sample comparison | 875 PFS0 + 14 XCI/XCZ HFS0 entries checked, as above |
| Cargo compilation, cargo test, Clippy, release build, GUI test | **NOT RUN** (user's manual build policy) |
| GitHub Actions workflow dispatch, release publication | **NOT RUN** |

## Manual follow-up

1. `cargo test --locked --all-targets` and `cargo clippy --locked --all-targets -- -D warnings` for new module imports and test compilation; compare against previous Stage 20 baseline 151 PASS / 15 ignored.
2. Test NSL's `scanner` and `xci` ignored read-only real corpus regression checks in a controlled environment, then test GUI/library metadata on a temporary copy of SQLite data. Do **not** overwrite original game or key files.
3. After validation, use the user's manual `build.bat` or `workflow_dispatch`. Verify the new distribution contains `licenses/nsrp-MIT.txt` and valid executable SHA/manifest. Existing r22 remains unchanged until then.

**Outcome:** useful NSRP parser algorithms incorporated in NSL and applied to live source paths; no external ROM-analysis executable needed. Build/runtime correctness of the new commit remains pending manual verification.
