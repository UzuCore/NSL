# NSL Stage 8 - HFS0/XCI/XCZ safe output and bounded compatibility I/O

Date: 2026-11-21 KST. Baseline commit: `d455e29` (Stage 7).

## Scope and isolation

Continue native package-engine maintenance while preserving the user's original game library, keys, emulator profiles, and other active emulator sessions. The user's game acceptance standard is **successful loading only**. No gameplay testing or long-duration FPS claims are required.

**Retail signed XCI/XCZ plus matching update are not present in the previously authorized isolated sample set.** Stage 8 therefore tests its changed game-card paths with deterministic nested HFS0/XCI/XCZ fixtures. Synthetic structural verification is not a substitute for a retail title or Switch hardware run. Stage 7 already independently verified loading of the unchanged real Horror Tale 2 NSZ merge; the Stage 8 HFS0/XCZ changes are not used by that NSZ loader path.

## Changes

- `vendor/nsrp-core/src/formats/hfs0.rs`: the legacy `build_from_paths_layout` directly opened its final output using `File::create`, risking data loss if a second job reused the path or encountered an I/O error. It now creates a unique same-directory temporary file, flushes and syncs it, validates the completed length, and commits with **no-clobber atomic persistence**. No existing output is deleted.
- HFS0 input preflight now rejects empty or oversized entry counts, case-insensitive duplicate names, missing source files, oversized/potentially overflowing name tables, unreasonably large header/data alignments, and 64-bit offset/size overflow **before** creating a large destination. Data is copied by exact declared lengths in bounded chunks and padding is written from a small reused zero buffer.
- `build_from_paths_layout_cancelled` is the cancel-aware version of this writer, used by XCZ compatibility staging. The existing 4-argument `build_from_paths_layout` wrapper remains for other callers.
- `vendor/nsrp-core/src/ops/xcz.rs`: fallback `NSRP_XCZ_COMPAT_STAGING` previously created a fixed `<extension>.partial` and explicitly deleted an existing result before a rename. It now uses a unique temporary file and the common **no-clobber** publication boundary. The root/partition HFS0 stages accept cancellation and the final cross-file copies check cancellation and expected length.
- `vendor/nsrp-core/src/ops/xcz_streaming.rs`: the default streaming transform and unchanged-XCI fast copy also use the same no-clobber publication function. If an output appears during a multi-gigabyte job, its bytes are preserved and the losing temporary is cleaned.
- The compatibility route now reuses one bounded **2MiB copy scratch buffer** for original partitions and root data instead of allocating a fresh 8MiB buffer on every copy. No full-NCA allocation is added.

## Deterministic test evidence

Tests cover nested HFS0 byte equality against the streaming writer; file name/bounds/cancel rejection; earlier output preservation; exactly one successful publication when two writers race; streaming and staged XCI/XCZ nested secure-content and NCA length consistency; no-clobber behavior after the result exists; pre-cancel cleanup; and two simultaneous default XCZ writers. Existing BKTR/NCZ/XCI regressions must also pass.

### Actual isolated compressed-data QA

Opt-in `real_package_ncz_nested_hfs0_passthrough` used only the read-only isolate `E:\NSL_QA_20261120\small\base.nsz`. It extracted one actual **171,859,243-byte NCZ** to a disposable private folder, built a nested `secure` HFS0 and root HFS0 using the updated writer, and compared full streaming SHA-256 of (a) the exact NCZ region in the NSZ, (b) the isolated NCZ copy, and (c) the NCZ region in the nested HFS0. **All three hashes matched**. The completed HFS0 also refused to overwrite itself and retained the original digest. **1 PASS, 0 FAIL; 42.894 seconds** for copying, packaging and hashing (test total 42.97 seconds). The temporary ~500MB private test files were deleted automatically upon test exit. This fixture is **not a Nintendo-signed XCI or a boot test**.

## Acceptance boundaries

No claimed retail signed XCI/XCZ boot, gameplay completion, hardware verification or unmeasured throughput speedup. A verified same-format synthetic roundtrip is useful for detecting regressions but does not prove retail compatibility. Preserve the original files during all QA.

## Final verification and delivery

| Validation gate | Final result |
| --- | --- |
| NSL `cargo test --locked --all-targets` | **199 passed, 0 failed, 30 ignored** (229 registered) |
| NSRP `cargo test --locked -p nsrp-core --lib` | **113 passed, 0 failed, 2 ignored** (115 registered) |
| HFS0 byte-identical nested-layout check and two concurrent writers | **PASS** |
| XCZ default and compatibility staged nested-card regressions | **PASS** |
| XCZ cancellation, duplicate and stale output safety | **PASS** |
| Full SHA-256 comparison of actual isolated NCZ across nested HFS0 | **PASS: 171,859,243 bytes** |
| `cargo fmt --all -- --check` and vendored format check | **PASS** |
| Strict NSL and NSRP `cargo clippy --all-targets -- -D warnings` | **PASS, zero warnings** |

### Optimized executable and GUI acceptance

- `cargo build --locked --release`: **PASS**, optimized profile completed in 5m 06s.
- Latest executable: `D:\NSW.dev\NSL\target\release\nsl.exe`, **22,020,096 bytes**.
- Executable SHA-256: **`57A6DC9E8E31E47AA392425969866BEFFDCF8809A5B1C29EB79522D43400D083`**.
- Actual GUI smoke: copied this exact executable into a unique temporary portable directory; hash matched the release binary, one **owned** NSL process survived eight seconds and exposed a nonzero Windows main-window handle. Only the owned test PID was stopped; its temporary directory was removed. **PASS**.
- The isolated authorized QA corpus has **0 signed XCI/XCZ samples**, and the Stage 8 NCZ-to-HFS0 test left **0 private work folders** and **0 `.partial` files**. No original game, production keys, or existing emulator process was modified.
- GitHub deployment: this report and Stage 8 code belong to the next commit on `UzuCore/NSL` `main`. After push, verify exact identity using `git rev-parse HEAD`, `git ls-remote origin refs/heads/main`, and `git status --porcelain` rather than embedding a self-referential commit ID in the document.

### Reproduction of isolated real HFS0 QA

```powershell
cd D:\NSW.dev\NSL
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
cargo test --locked real_package_ncz_nested_hfs0_passthrough -- --ignored --nocapture --test-threads=1
```

This test uses a private temporary folder within the QA root and removes it on completion. It neither modifies any library game nor loads the synthetic gamecard in Eden.
