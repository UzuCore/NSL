# NSL local release build, large-ROM restore, isolated GUI smoke

Date: 2026-11-20 (KST). Test inputs: the local read-only game library; source baseline `c38a71a` before the following QA patch. No GitHub Actions dispatch, official release update, or modification of ROMs, user keys, the reference `library.db`, or user config was performed.

## Local builds and Rust regressions

- `cargo test --locked --all-targets -- --test-threads 4` — **171 passed, 0 failed, 16 ignored**; full suite rebuilt after adding the failure-cleanup test and after the large QA process released the Windows test EXE lock.
- `cargo clippy --locked --all-targets -- -D warnings` — **PASS** (zero warnings).
- `cargo fmt --all -- --check` — **PASS**.
- `cargo build --locked --release` — **PASS**, updates `D:\NSW.dev\NSL\target\release\nsl.exe`. Local build must be repeated after the final Git commit because `build.rs` embeds `NSL_BUILD_COMMIT` from the current HEAD. The official GitHub release remains r22 until separately published.

An intermediate Rust test rebuild encountered Windows `LNK1104` because the previously built test EXE was **being executed** by the large restoration test. The process was not stopped or interrupted; after the large test exited, the complete test suite rebuilt and passed. This was a transient Windows executable-file lock, not a source error.

## Actual >4GiB input restoration and verification

Ignored opt-in Rust test: `native_package::tests::restore_large_real_nsz_and_xcz_with_entry_checks_and_full_output_sha256`.

| Actual sample | Read-only input | Restored temporary output | Entries checked | NCZ restored | Time (debug profile) |
| --- | ---: | ---: | ---: | ---: | ---: |
| NSZ → NSP | 4,406,542,428 bytes | 4,476,962,816 bytes | 12 | 2 | 205.02 s |
| XCZ → XCI | 5,558,742,565 bytes | 6,887,376,896 bytes | 7 | 1 | 390.31 s |

Both **PASS** with exact restored output lengths, PFS0/HFS0 root and nested entry parsing, normalized NCA output names, HFS0 output entry hashes (where present), restored NCZ sizes, preserved original NCA headers, and byte comparisons for untouched entries. The test computes **SHA-256 across every byte of each complete restored output file**:

```text
NSZ restored NSP SHA-256 = da4e259304ef2afa615c882fb9ac69fe9bc0e531decf0874e7fd348cb0bac928
XCZ restored XCI SHA-256 = e8cd03b2ae1ab130bc315fa4dd2ed00b85c5d75d84056e4c4d63ca10ec69b425
```

These output digests form repeatable baselines for future builds; they **are not** independent reference-decompressor comparisons or proof of game launch compatibility. Times are from the **unoptimized debug test binary** while the system was performing other QA and must not be represented as release throughput benchmarks. Production inputs were opened read-only; source sizes and modification times were checked before and after. Output used `tempfile::tempfile()` and was deleted automatically when the test finished; no games were copied into the project.

To repeat deliberately (requires local >4GiB corpus, fast scratch disk with ample free space, and permission for multi-gigabyte temporary writes):

```powershell
cargo test --locked --bin nsl restore_large_real_nsz_and_xcz_with_entry_checks_and_full_output_sha256 -- --ignored --nocapture --test-threads 1
```

The test selects the second-smallest NSZ and smallest XCZ of at least 4 GiB from the local read-only `library.db`. It is **ignored by default** because it performs substantial disk I/O and full output hashing.

## Failure-path cleanup

Previously a failed `decompress::decompress` conversion removed its `NamedTempFile` but could leave behind an empty private `NSL_Workspace_*` folder. The conversion now scopes staging inside a fallible closure and removes **only its own empty workspace** after staging is dropped. It never recursively removes user-created files or another process's directory contents. New default regression `decompress::tests::failed_nsz_rebuild_removes_empty_workspace_and_preserves_source` constructs a container with a valid plain entry followed by an invalid zstd NCZ payload, verifies the error, the absence of partial outputs/workspaces, and unchanged input bytes.

## Actual Windows GUI startup in isolation

The local release executable was **copied** to a uniquely named `%TEMP%\NSL_GUI_SMOKE_*` portable test folder and launched minimized. The process exposed a main window named `NSL  c38a71a`, remained responsive enough to expose its handle and title, and created its database **only inside the test directory**. The test then closed **only the created test process**, and verified that the temporary directory was removed. No other running NSL, emulator, or personal data was touched. This is a **GUI launch smoke test**; visual inspection and interactive store/download functionality on the actual desktop still need user-driven verification. Headless egui runtime tests covering layout, file selection trees, paging, sidebar width, and click targets are included in the 171 passing default tests.

## Remaining work and limits

The 16 ignored default tests include environment-specific network, full-library scan, database benchmark, additional game-corpus and torrent integration checks (including the opt-in >4GiB test). Some safe real-corpus tests and the large test were executed separately, but **the entire ignored-test set was not executed**. Additional priorities: real desktop mouse/keyboard walkthrough, full 4TB corpus/scan performance comparison, longer torrent transfers and cancellation/disk-full simulation, independent decompressor comparison and real gameplay compatibility. The single XCZ whose root secure partition has 411 absent declared bytes remains safely rejected.
