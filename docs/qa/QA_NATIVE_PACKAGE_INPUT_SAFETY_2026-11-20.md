# NSL native NSZ/XCZ repacker input safety (source-only)

Date: 2026-11-20 (KST). Input baseline: `39ee5e6`, local NSL `D:\NSW.dev\NSL`.

## Purpose / restrictions

Continue the NSRP-derived ROM-index validation into the **native conversion input** reader (`src/native_package.rs`). Preserve user ROMs, the original DB, keys, settings, binaries, and all other running programs. The user reserved builds and releases for manual execution: this review does **not** compile Rust, invoke Clippy, run NSL, decompress NCZ data, create converted ROMs, or trigger a GitHub workflow.

## Converter-specific compatibility

NSL has two related but not interchangeable index readers:

- `container_index.rs` performs read-only library metadata inspection. It can skip invalid metadata entries and tolerate up to 0x200 bytes missing from the end of a **named HFS0 game-card partition** for purposes of discovering surviving game metadata.
- `native_package.rs` reads input for **NSZ→NSP and XCZ→XCI rebuilding**. It must reject any input extent exceeding the original file or its containing partition, because the missing bytes might include compressed content rather than padding. It must also accept **valid zero-entry HFS0** partitions (common in XCZ `normal`/`update`), which the general read-only index does not represent.

No attempt is made to merge their different extent/tolerance policies; that would risk silently writing incomplete output files or rejecting valid empty game-card partitions. Both readers now share the same safe UTF-8 filename policy through `container_index::valid_name`.

## Source changes

- `native_package.rs::read_index`: filename table must contain an actual NUL terminator, rather than silently treating end-of-table as a terminator.
- `native_package.rs::read_index`: reject aliasing/overlapping **nonempty** original data regions using an ordered interval map. Adjacent and zero-length entries remain allowed. The existing per-entry bounds and HFS0 `hash_size <= size` checks remain.
- Case-insensitive name deduplication follows the shared filename reader, and no additional crate or external ROM-analysis tool is needed.
- Source-level Rust regressions added for PFS0/HFS0 overlap, missing terminator and empty HFS0 partitions. **These Rust tests have not been compiled/executed** under the manual-build policy.
- Added an independent **read-only** Python model `scripts/qa-native-package-index-readonly.py` for input acceptance testing. It reads the existing SQLite catalog with `mode=ro&immutable=1`, opens ROM files using `rb`, and compares size/mtime after each inspect; it never extracts or modifies game contents.

## Real corpus and synthetic rule checks

| Source/read-only check | Result |
| --- | --- |
| Available NSZ converter input indexes | **804/804 structurally accepted** |
| NCZ entry references in those NSZ packages | **4,594** (filenames only) |
| Available XCZ inputs | **8** |
| XCZ converter-compatible root indexes | **7/8** |
| XCZ root rejected for missing declared end bytes | **1/8** (`secure`, **411 bytes short**) |
| Valid nonempty HFS0 child partitions in 7 eligible XCZ | **10** |
| Valid empty HFS0 child partitions in 7 eligible XCZ | **11** |
| Entries in nonempty eligible XCZ child indexes | **62** |
| Converter-specific standalone synthetic checks | **10,009 PASS** (10,000 mutations + 9 targeted assertions) |
| Converter-specific synthetic checks alongside real corpus | **2,009 PASS** (2,000 mutations + 9 targeted assertions) |
| NSL library reader full-corpus control check | **875 NSP/NSZ + 14 XCI/XCZ roots**, **17** nested partitions |
| Rustfmt, offline Cargo manifest, Git whitespace, synthetic new/legacy ZIP verifier | **PASS** (no compilation) |

The `secure` partition in one XCZ declares **411 more bytes than physically exist**. The metadata reader can inspect its surviving index with bounded 0x200-sector tolerance; the converter correctly rejects it. Without inspecting/decompressing its protected payload it is **not possible to establish that the missing bytes are only padding**. The converter must not truncate the emitted NCA/NCZ silently. This limitation predates the current hardening; it is recorded here as an **unsupported conversion input**, not a newly introduced regression. The original source is unchanged.

## How to reproduce (without building)

```powershell
cd D:\NSW.dev\NSL
python -B scripts/qa-native-package-index-readonly.py --require-corpus --mutations 2000
python -B scripts/qa-native-package-index-readonly.py --db scripts/missing-library.db --mutations 10000
python -B scripts/qa-rom-index-readonly.py --require-corpus --mutations 1000
cargo fmt --all -- --check
cargo metadata --locked --no-deps --offline --format-version 1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-package-verify.ps1
```

Passing these is **independent format-rule validation only**, not a compiled Rust regression, functional NSZ/XCZ conversion, end-to-end verification, NCA SHA-256 content validation or GUI QA. Those require the next user-authorized manual build. Existing official r22 artifacts and release are unchanged.
