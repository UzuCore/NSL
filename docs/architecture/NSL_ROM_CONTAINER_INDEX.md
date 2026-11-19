# NSL ROM container reader integration

Date: 2026-11-20 (KST). Source-only integration of NSRP Rust techniques.

## Audit of external tools

NSL's ROM parsing and NSZ/XCZ conversion paths use internal Rust implementations (`scanner.rs`, `native_nca.rs`, `ncz.rs`, `native_package.rs`, `xci.rs`). They do **not** launch `nstool.exe`, `hactool.exe`, `hactoolnet`, `hacpack`, or `nsz.exe` in normal operation. Source references to those tools are historical comments or packaging cleanup of obsolete files. NSL still invokes the system Explorer, a user-specified emulator launcher, and PowerShell to apply self-updates; those are not ROM decryption or parsing dependencies.

## Source review / licensing

- Reference: `D:\NSW.dev\NSRP\rust_port\src\formats\pfs0.rs` (`Pfs0Index::parse_reader`): bounded table, safe filenames, case-insensitive duplicate detection, checked data offsets.
- Reference: `D:\NSW.dev\NSRP\rust_port\src\formats\hfs0.rs` (`Hfs0Index::parse_at` / `find_xci_root_with_header_base`): 0x200 sector padding handling on named partitions, HEAD pointer variants, aligned fallback root search.
- Reviewed, **not imported**: `ops/cnmt.rs`, BKTR readers/patcher, NCZ compression/repacking engines. NSL already supports CNMT/NCZ; the NSRP BKTR/repacking operations need separate security, compatibility and performance work before adoption.
- NSRP's `LICENSE.txt` is MIT, copyright 2026 sinjunyoung. The unchanged license is retained at `licenses/nsrp-MIT.txt`, required in all **newly produced** packages. Legacy r22 package verification remains supported without this new notice.

## Architecture

`src/container_index.rs` centralizes read-only PFS0/HFS0 table parsing for both ROM families. It uses only the Rust standard library. It does **not** decrypt game data, spawn programs, write output files, or cache sensitive file bytes.

| Consumer | Operation | Policy |
| --- | --- | --- |
| `scanner.rs::container_contents` | Plain CNMT/ticket/XML metadata fallback | Metadata: safely skip individual invalid entries |
| `scanner.rs::container_nacp` | Plain NACP metadata fallback | Metadata: safely skip individual invalid entries |
| `scanner.rs::pfs_ncas` | Shared encrypted NCA index for parallel native CNMT/NACP readers | Metadata: safely skip individual invalid entries |
| `xci.rs::read_index` | Nested XCI/XCZ HFS0/PFS0 game-card parsing | Strict: reject malformed index |
| `xci.rs::root_index` | HEAD pointer and 0x200-aligned root discovery | Strict HFS0 only; root must contain `secure` |

Both policies validate header/table bounds with checked arithmetic, cap entry counts/string bytes, require safe UTF-8 NUL-terminated names, and prevent an invalid entry from being returned. **Strict** rejects the entire index when any entry is invalid; **metadata** only omits the bad entry so one broken unrelated record cannot hide valid game metadata. Duplicate names are rejected in strict mode or deduplicated in metadata mode (first valid entry wins).

NSRP's HFS0 compatibility rule tolerates an over-declared named partition size by **at most one 0x200-byte sector**, never an arbitrary NCA or PFS0 entry. XCI root search now checks the additional 0x200 HEAD location and 0x200-aligned offsets in the conventional header region, without sweeping the game payload.

The native conversion writer (`native_package.rs`) retains its own strict, transform-aware index representation; it has not been casually replaced with a metadata-only parser. No NCA decryption or filesystem write paths have been changed.

## Validation and limits

Read-only local library header sampling first found 136 NSP/NSZ entries and identified one compatibility case. The follow-up **full database-corpus header check inspected 875/875 available NSP/NSZ files**, with **874 fully valid index tables and 1 file with one out-of-bounds unrelated entry**. Metadata mode safely keeps that file's valid index entries. **14/14 available XCI/XCZ root HFS0 indexes** were accepted by the strict structural validation model; all 14 contained `secure` and update/normal partitions. These checks only read package headers and a read-only SQLite snapshot, **not** decrypted game contents; they are independent rule checks rather than compiled Rust tests.

Rustfmt and repository/source attribution checks passed. Isolated PowerShell ZIP verification passed both the legacy 6-file and new 7-file cases, plus invalid-manifest/extra-file/hash-mismatch rejections.

**Manual-build policy:** no Cargo compilation, Clippy, executable launch, GitHub Actions run, or release creation has been performed. Before the next release, run `cargo test --locked --all-targets`, `cargo clippy --locked --all-targets -- -D warnings`, the optional local-corpus XCI/XCZ test, and isolated GUI/package smoke tests. Preserve original ROM files, user DB/config/keys and existing r22 binaries.
