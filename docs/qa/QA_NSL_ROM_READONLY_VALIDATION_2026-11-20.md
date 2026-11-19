# NSL / NSRP ROM integration — independent no-build validation

Date: 2026-11-20 (KST). Baseline source commit: `0f339a979f23747d36b4d9b180f64154c7128a8f`.

## Scope and safeguards

The requester asked to continue verification while keeping release builds under manual control. This review did **not** invoke `cargo build`, `cargo test`, `cargo check`, Clippy, the GUI executable, the GitHub Actions workflow, or a new release. Such commands would compile Rust and have not been authorized under the existing no-build policy. This report must **not** be interpreted as proof of Rust type checking, test pass, decryption correctness or GUI behavior.

All real game files were accessed with `open(..., "rb")`; the SQLite database was queried using `mode=ro&immutable=1`. The independent QA runner checked each inspected file's size and modification timestamp after the read. No private keys, ROM payload extraction, conversions, writes to the library, running instances, or user settings were involved.

## Validations performed

| Check | Outcome |
| --- | --- |
| `python -B scripts/qa-rom-index-readonly.py --mutations 10000` against absent test DB | **10,014 synthetic checks passed**, no library needed |
| `python -B scripts/qa-rom-index-readonly.py --require-corpus --mutations 2000` | **2,014 synthetic checks passed** plus real corpus below |
| Total available NSP/NSZ package files | **875/875** recognized |
| Strict-valid PFS0 package indexes | **874** |
| Partial PFS0 indexes (valid entries retained) | **1**, one malformed index entry omitted |
| Safe available NSP/NSZ entries | **15,052** |
| XCI/XCZ roots | **14/14** strictly accepted |
| XCI/XCZ nested partition indexes | **17/17** validated against their own partition ends |
| Safe nested entries | **180** |
| CNMT references in `secure` / `update` / `normal` | **54 / 2 / 1**, structural names only, not decrypted |
| Small/empty partitions skipped under existing policy | **25** |
| `cargo fmt --all -- --check` | PASS (Rust parsing and formatting only) |
| `cargo metadata --locked --no-deps --offline --format-version 1` | PASS (manifest targets, no compilation) |
| Isolated new-format and legacy-format ZIP verifier tests | PASS |
| ZIP verifier rejection: invalid manifest/unknown file/changed EXE/hash mismatch | PASS |
| Existing **r22** `dist/nsl-win64.zip` | PASS: ZIP CRC, no private-user files, manifest JSON equality, `nsl.exe` SHA-256 equality |
| Existing r22 ZIP manifest commit | `d09c91ab1895...` — expected **pre-modularization** build |
| Missing-DB mode with explicit `--require-corpus` | PASS: nonzero exit code 2 |
| No-DB mode without `--require-corpus` | PASS: synthetic checks run independently, corpus reports skipped |

The independent header verifier has expanded coverage and reports per-partition CNMT counts. The `--require-corpus` flag was added so automated real-corpus checks **fail**, rather than quietly passing, when the DB or files are absent. Deterministic mutations now cover PFS0 and HFS0, with additional impossible count/length and missing-NUL cases.

## Observations / unresolved risks

1. The 1 partially malformed NSP remains unchanged. Best-effort library metadata lookup may use its 1 or more valid entries, while structural transformations must still reject invalid content.
2. The 3 CNMT filename references outside `secure` (2 in `update`, 1 in `normal`) demonstrate why the current XCI/XCZ scanner searches all three partitions. This count is **not** proof their encrypted CNMT payloads successfully decrypt.
3. The existing r22 ZIP passed its own integrity checks, but it was built from `d09c91a` and therefore does **not** test the latest modularized / NSRP-integrated Rust source.
4. Neither an independent Python replica nor rustfmt establishes that the changed Rust code compiles. Full Rust unit tests, Clippy, live NCA metadata scanning, GUI regression and official release build require the user's explicit **manual build** workflow.

## Reproducible commands

```powershell
cd D:\NSW.dev\NSL
python -B scripts/qa-rom-index-readonly.py --require-corpus --mutations 2000
python -B scripts/qa-rom-index-readonly.py --db scripts/no-such-library.db --mutations 10000
cargo fmt --all -- --check
cargo metadata --locked --no-deps --offline --format-version 1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-package-verify.ps1
```

Only after manually authorizing a compile/build, run `cargo test --locked --all-targets`, `cargo clippy --locked --all-targets -- -D warnings`, and the ignored real-corpus / isolated GUI tests. Do not replace the existing local r22 executable or start workflows without explicit user instruction.
