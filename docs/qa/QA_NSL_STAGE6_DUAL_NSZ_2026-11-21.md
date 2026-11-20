# NSL Stage 6 - compressed-update NSZ fast-path and loader-only verification

Date: 2026-11-21 KST. Baseline: `b880c45` (Stage 5).

## Scope and safety

- Honor the new game acceptance policy: **load successfully in a separate Eden instance only**. No gameplay, menu-control endurance, or prolonged playthrough testing.
- Read only the existing isolated game inputs `E:\NSL_QA_20261120\small\base.nsz` and `patch.nsp`, plus the independently verified Stage 3 merged NSZ. Do not touch the original `E:\nsw` library, the original prod/title key files, or another emulator session.
- The private optional test first creates an NSZ update from the isolated NSP **in a temporary directory**, then uses that already compressed update as the merge input. Compression of the patch for test-fixture creation is **not part of** the measured fast merge elapsed time. All temporary outputs are opt-in and cleaned after testing.

## Implementation

- `src/update_merge.rs`: NSZ base now admits **either NSP or NSZ update** into the fast-path dispatch. The fast-path space preflight reserves less transient update space when it is already compressed; the legacy fallback retains its larger space check.
- `src/update_merge/fast_nsz.rs`: NSZ+NSZ merges stream unchanged NCZ byte ranges from **both** packages directly into the final NSZ. A precompressed update requires no temporary `fast_patch.nsz` and no patch NCZ decompression or recompression. Existing CNMT selection, DLC handling, rights/ticket passthrough, cancellation and no-clobber publishing remain shared with the NSP patch path.
- Compressed `.cnmt.ncz` or an NSZ without any `.ncz` explicitly opts out of this fast path before writing output. Layout errors after selection remain a hard failure; every selected NCZ also passes the Stage 5 bounded structural preflight.
- Normal and opt-in regression tests check dispatch eligibility, unsupported layouts, exact compressed-payload passthrough, CNMT acceptance and cancellation/nonpublishing behavior.
- `scripts/qa_eden_loader_smoke.ps1`: reusable **loader-only** QA. Creates a unique portable Eden profile, copies the user's `prod.keys` read-only, derives the required encrypted title key solely from the authorized isolated update NSP's embedded ticket, runs one owned Eden instance, checks loader/GPU engine logs, checks for fatal/NCA errors and captures one real renderer frame. It **does not send gameplay or menu input**. It verifies the game source SHA-256 before/after, saves only a small report and screenshot, stops only the exact owned PID, and deletes the entire private executable/profile/keys directory on completion.

## Validation results

For a precompressed update NSZ generated once outside the timed merge step:

| Isolated real package | Prior Stage 5 NSP-update merge | Stage 6 NSZ-update merge | Detail |
| --- | ---: | ---: | --- |
| Horror Tale 2, base and update | 111.245 seconds | **13.736 seconds** | **87.65% shorter** for the merge stage only; precompressing the patch into NSZ is excluded from the 13.736 seconds |

Both base and update supplied **one reused NCZ each**, with SHA-256 of each copied compressed payload matching its original byte range. The resulting NSZ has **10 PFS0 entries, 2 compressed NCZ, 330,474,468 bytes**. Full restored-NCA equality and Eden loading are independent further checks, not implied by the byte-passthrough comparison.

The real opt-in equivalence test then **fully decompressed both the new result and independently SHA-256 verified Stage 3 reference** and confirmed **8/8 restored NCA SHA-256 equality**. End-to-end QA elapsed **317.81 seconds** including initial independent patch compression, region hash comparisons and double restoration. This **is not** the 13.736-second merge stage duration.

## Eden loading-only acceptance

The newly merged Stage 6 NSZ was opened in a fresh, isolated Eden CLI copy. The finalized `scripts/qa_eden_loader_smoke.ps1` reports:

| Check | Observed result |
| --- | --- |
| Loader-only observation | **18 seconds PASS**, no menu interaction |
| GPU-rendered window | **1296 × 759** |
| Distinct sampled pixel colors | **350**, nonblank frame |
| Actual rendered game title | **Horror Tale 2**; directly inspected screenshot |
| NCA bad-header error | **No** |
| Engine Critical/Fatal entries | **0** |
| Source NSZ SHA-256 before/after | **Unchanged** |
| Other emulator instances affected | **None** |

Final captured evidence: `E:\NSL_QA_20261120\stage6_dual_nsz_20261121\eden_loader_repeat.png` and `eden_loader_repeat.json`. The first smoke attempt coincided with heavy QA I/O and did not retain a renderer window through its observation deadline; the later standalone loader-only run passed. The first successful loader verification also exposed a transient Windows file lock on the copied Eden executable during immediate cleanup; the QA script was hardened to wait and retry deletion, **deleting copied keys first**. The final loader run passed including full private-profile deletion. Do not count the transient failed attempts as emulator acceptance passes.

The temporary **330,474,468-byte** new merged test package (SHA-256 `68F0DA09DCE6AB7ED44AD58E5B556BA12F51BDAE02DB3CAAF7788CB5C27D79BC`) was deleted after testing, preserving only small evidence and the original isolated inputs. No new files were published into the user's library by the real QA test.

## Reproduction: explicit isolated QA only

```powershell
cd D:\NSW.dev\NSL
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
$env:NSL_QA_PATCH='E:\NSL_QA_20261120\small\patch.nsp'
$env:NSL_QA_BASE_ID='0100F7301AAB6000'
$env:NSL_QA_VERSION='65536'
$env:NSL_QA_EXPECTED='E:\NSL_QA_20261120\small\[NSL Merged] [0100F7301AAB6000][v65536].nsz'
$env:NSL_QA_PRECOMPRESS_PATCH='1'
# Opt-in test: compress update once, then merge an NSZ base + NSZ update.
cargo test --locked real_package_fast_nsz_equivalence -- --ignored --nocapture
# Separate test: publish, recover existing output, reject overwrite, preserve inputs.
cargo test --locked real_package_fast_nsz_end_to_end_publish -- --ignored --nocapture
```

For loader-only smoke, first set `NSL_QA_EDEN_EXPORT` to a **new**, nonexisting `.nsz` filename inside a preexisting isolated QA directory, rerun the equivalence test to export that test file, and invoke `scripts\qa_eden_loader_smoke.ps1` with the exported `-Game`, isolated original `-TicketNsp`, local read-only `-ProdKeys`, original `-EdenCli`, its containing `-QaDir`, and a new `-RunLabel`. The script never presses keys in the game, never overwrites existing evidence and deletes its unique private keys/profile afterward.

The independent **full production backend path** `real_package_fast_nsz_end_to_end_publish` also passed with `NSL_QA_PRECOMPRESS_PATCH=1`: **1 passed, 0 failed; 167.75 seconds** including fixture compression. It used separate byte copies of both inputs, published one **330,474,468-byte** NSZ, recovered the preexisting output safely, rejected an attempt to overwrite it, and confirmed unchanged SHA-256 for the base NSZ, uncompressed update NSP and generated update NSZ. Its private temporary workspace was removed automatically.

## Final regression gates

- `cargo fmt --all -- --check` and vendored core format check: **PASS**.
- `cargo test --locked --all-targets -- --test-threads=4`: **199 passed, 0 failed, 27 ignored**, 226 registered.
- `cargo test --locked -p nsrp-core --lib -- --test-threads=4`: **103 passed, 0 failed, 2 ignored**, 105 registered.
- Strict NSL and NSRP core `cargo clippy --locked --all-targets -- -D warnings`: **both PASS, zero warnings**.
- `scripts/qa_eden_loader_smoke.ps1`: PowerShell parser check PASS and a completed isolated Eden run PASS. This reusable QA script deliberately never presses game controls.
- `cargo build --locked --release`: **PASS**. Final executable `D:\NSW.dev\NSL\target\release\nsl.exe`, **22,026,240 bytes**, SHA-256 **`5463DC91915A71E494AAC7AF8F1B1BE8D7A0C89ED71AFD35DD3DB5F63FF9D11E`**.
- A SHA-matched copy of that final executable was launched from an isolated temporary portable directory, survived **8 seconds**, and produced a responsive nonzero Windows main-window handle. Only the specifically launched QA PID was stopped, and its temporary directory was removed. **PASS**.
- Isolated game and emulator QA left **0 copied key files**, **0 NCZ `.partial` files** and **0 active Eden processes**. The original library games, installed emulator, and key files were not modified.
- Final deployed revision is the Stage 6 commit on `UzuCore/NSL` `main`; use `git rev-parse HEAD` and compare it to `git ls-remote origin refs/heads/main` to verify exact remote identity without embedding a self-referential commit hash in this document.

## Explicitly not established

- No Nintendo Switch hardware testing, real signed XCI/XCZ plus matching update input, gameplay completion, or long-term frame-performance claims.
- NCZ block/header structural checks are **not** the same as fully decompressing, authenticating and hashing all encrypted NCA bytes. A separately run full NCA comparison is needed for that stronger claim.
