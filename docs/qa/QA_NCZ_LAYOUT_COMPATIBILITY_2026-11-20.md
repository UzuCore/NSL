# NSL NCZ layout and repacker output validation — source-only

Date: 2026-11-20 (KST). Baseline `50e36e4` on main. Local source: `D:\NSW.dev\NSL`.

## Scope / precautions

This follow-up audits the native NSZ/XCZ conversion *plan* and the NCZ section/block headers used by `src/ncz.rs`. All 804 available NSZ and 8 available XCZ files were opened **read-only** from paths referenced by `library.db` (SQLite `mode=ro&immutable=1`). Game payloads were **not decompressed**, NCA keys were **not retrieved or printed**, no output ROMs were created, and no user data/executables/processes were altered. The Python validator skips each NCZ section's embedded key and counter rather than loading them.

The user reserves Cargo compilation and releases for manual execution. No `cargo build`, `cargo test`, `cargo check`, Clippy, NSL GUI launch, GitHub Actions dispatch, or release was performed. These results **do not establish that the modified Rust code compiles or that actual decompression succeeds**.

## Actual input observations

An earlier read-only container-index check had reported 804/804 accepted NSZ indexes, 7/8 strictly accepted XCZ converter indexes, and one XCZ `secure` partition with a declared end 411 bytes beyond the available file. That XCZ remains deliberately rejected for conversion: missing data cannot be assumed to be padding.

A deeper independent NCZ **layout** check examined all **4,601** reachable `.ncz` entries: 4,594 inside NSZ and 7 inside converter-eligible XCZ. Prior to this source change, the same strict header rules accepted 4,023 and rejected 578:

| Previously unsupported NCZ layout | Real-corpus evidence | Resolution |
| --- | --- | --- |
| First encrypted section begins at `0xC00`, before the `0x4000` preserved NCA header ends | **541**; each NCZBLOCK uncompressed byte count matches raw section end minus `0x4000` | Preserve the first `0x4000` bytes and decode/reencrypt only the part after the header; reject arbitrary earlier/overlapping sections |
| More than 4,096 section records | **31**, count 4,262–32,299; each section table, block count and compressed range independently validated | Increase cap to **32,768** sections (2 MiB at 64 bytes per record), still bounded |
| NCZBLOCK payload extends beyond last encrypted section | **6**, each with a valid block count and compressed-size table; last encrypted region is smaller than the declared full decompressed payload | Treat the additional bytes as **unencrypted trailing NCA data**, stream them without AES, and enforce the declared block/full-output boundaries |

After these format-rule updates:

| Header-only inspection result | Count |
| --- | ---: |
| Valid NCZ structure/header | **4,601 / 4,601** |
| NCZBLOCK / solid encoding | **4,593 / 8** |
| NCZ section descriptors checked | **500,120** |
| NCZBLOCK block-size table entries checked | **4,649,373** |
| NCZ files with plain data following their last encrypted section | **6** |
| Total plain trailing bytes declared by those 6 headers | **37,199,757,312 bytes** |
| Invalid NCZ headers after supported compatibility updates | **0** |
| NSZ input indexes | **804 / 804 accepted** |
| XCZ root input indexes | **7 / 8 accepted; 1 with 411-byte short tail still rejected** |

No source conversion-name collision was seen in **832** eligible NSZ/XCZ container indexes checked. This is not a guarantee against malformed future inputs; a new `validate_restored_names` preflight in `native_package.rs` now rejects case-insensitive `*.ncz` → `*.nca` collisions **before** NCZ payload inspection and output writes. `.NCZ` also maps to `.NCA` without mixing extension case.

## New Rust behavior (source present, not yet compiled)

- `native_package.rs`: preflight restored-file-name collisions independently for each input PFS0/HFS0 index; preserve extension case consistently.
- `ncz.rs::inspect`: recognize the verified first-section `0xC00` overlap of the already preserved header, and raise the strict section-record cap from 4,096 to 32,768 with all existing bounds/cryptotype checks.
- `ncz.rs::inspect` NCZBLOCK: allow a full uncompressed byte count **at least** as large as the last encrypted section's end, but not over `MAX_NCA_SIZE`; the exact declared count must still agree with the block count and each block's bounded compressed bytes.
- `ncz.rs::decompress`: copy the remaining **plain** NCZBLOCK data through the existing bounded streaming buffer. Incomplete data causes an error; no guessed padding. The `SparseNca` reader already handles bytes after the final encrypted section as unencrypted.
- Added Rust source tests for case-insensitive output name collisions, solid/block `0xC00` sections, over-4096 section counts, and NCZBLOCK encrypted-prefix/plain-tail restoration including sparse seeks. **The Rust tests are authored but were NOT run**.

## Repeatable no-build checks

```powershell
cd D:\NSW.dev\NSL
python -B scripts/qa-native-package-index-readonly.py --require-corpus --inspect-ncz-headers --mutations 2000
python -B scripts/qa-native-package-index-readonly.py --db scripts/no-such-library.db --mutations 10000
python -B scripts/qa-rom-index-readonly.py --require-corpus --mutations 1000
cargo fmt --all -- --check
cargo metadata --locked --no-deps --offline --format-version 1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-package-verify.ps1
```

`--inspect-ncz-headers` intentionally exits nonzero if any NCZ section/block layout fails. These independent Python checks cannot validate zstd decompression, AES-CTR reconstruction, NCA file hashes, Nintendo game launch compatibility, the GUI, the Rust type checker or the new Rust tests. Those remain mandatory for the **user-authorized manual build** phase before a new release. Existing official r22 binaries and release are unchanged.
