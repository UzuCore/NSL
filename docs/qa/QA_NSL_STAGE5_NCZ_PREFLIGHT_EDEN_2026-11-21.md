# NSL Stage 5 - NCZ passthrough safety and real Eden GUI verification

Date: 2026-11-21 KST. Starting commit: `9ad66a8`. Windows/MSVC.

## Scope and protection

- Reuse the three **isolated** real NSZ samples in `E:\NSL_QA_20261120` for read-only input inspection. Never modify games under `E:\nsw`, Stage 3 reference merges, actual prod/title key files or a user's existing emulator session.
- Stage 5 adds a bounded NCZ layout preflight before an unchanged `.ncz` is copied into a new merged NSZ. This is **structural validation**, not full decompression or a cryptographic hash proof.
- A temporary Eden portable instance is created solely for this QA inside `E:\NSL_QA_20261120\stage5_eden_loader_20261121\portable`. The CLI executable and keys exist there only while testing, and the title key copy is derived from the game's own ticket. No secret bytes are printed or committed. The original Eden profile is never opened for write.
- Other emulator processes are never terminated or reused. Eden testing targets only an owned PID and verifies the owned window for capture/input.

## Native implementation

`vendor/nsrp-core/src/formats/ncz_stream.rs` now exposes `validate_ncz_region_layout(reader, base, size)`, sharing the following checks with actual NCZ decompression:

- Valid section bounds/types, bounded block exponent (14..=24), declared uncompressed byte count, expected number of blocks, each encoded block's bounded size, safe addition of encoded byte lengths, and the final compressed-data extent relative to its archive entry.
- Stream mode must declare a nonempty payload. Section/block index reading is bounded and **does not inflate the payload**.
- A malformed block layout is rejected **before even the reconstructed NCA header is written**, whereas previously some checks happened after the initial header write.

`src/update_merge/fast_nsz.rs` validates every selected `.ncz` region, including preserved base content and newly compressed update content, **before** `build_from_sources` creates any output. Readers are reused per source container to avoid repeated opens. Malformed selected NCZ is a hard error and never silently falls back to an unverified publication.

Regression coverage includes valid stream NCZ passthrough byte identity, intentional corruption of the NCZ magic, missing mandatory content, cancelled operations, block index corruption, unsafe block exponents, truncated compressed body, nonzero-offset entries and zero-output-on-failure behavior.

## Read-only real input checks

| Source | Compressed NCZ checked | Declared reconstructed NCA bytes | Verdict |
| --- | ---: | ---: | --- |
| Horror Tale 2 base NSZ | 1 | 398,606,336 | PASS |
| UFO 50 base NSZ | 2 | 548,338,176 | PASS |
| Five Hearts Under One Roof season2 base NSZ | 1 | 8,846,786,560 | PASS |

Each opt-in `real_package_ncz_passthrough_preflight_readonly` case passed in **0.02–0.03 seconds** after Cargo compilation, without producing a plaintext NSP. It is not a substitute for full NCZ decompression.

## Fresh real merge / SHA-256 equality

New Stage 5 Horror Tale 2 fast NSZ: **330,474,468 bytes**, **111.245-second** merge and encoding phase. The opt-in full SHA test `real_package_fast_nsz_equivalence` passed in **281.37 seconds**, verifying all **8/8 merged NCA SHA-256** digests against the previously independently verified Stage 3 reference. Export occurred only to a fresh path under the isolated QA root. Original package and key file bytes were never rewritten.

## Actual Eden GPU/title-menu smoke

- Eden CLI launched from a copied executable and its own portable `user/keys` profile, with private application-data roots and the newly exported Stage 5 fast NSZ as input.
- **Horror Tale 2's actual rendered title screen was visually inspected**: logo and the `CONTINUE`, `NEW GAME`, `SETTINGS`, `ACHIEVEMENTS` choices are visible. The window capture measured **1296 x 759 pixels**, with **254 distinct sampled colors**. This is GPU-rendered title UI, not a null renderer or a black placeholder.
- The isolated owned process survived a **66-second** loader smoke. Eden log confirmed loader and renderer activity. There was no `NCA bad header` / `0007-000C` error, and the source NSZ SHA-256 was unchanged after execution.
- Initial profile setup emitted a nonfatal `Invalid string size` warning from Eden after a private `title.keys` file had included a 16-digit alias. This is **an Eden QA-profile formatting issue, not evidence that the game package is malformed**. On the next isolated run the profile used only its full 32-digit rights ID and the spurious critical messages dropped to **zero**.
- A second owned-instance test dispatched Down and Enter to the exact owned window, collected before/after captures, and confirmed the emulator remained alive. The images differed partly through animation, but still showed the same title menu; **interactive menu navigation was not certified** by this test.
- Eden SDL2 config in this **private** profile mapped controller A to `SDL_SCANCODE_A` (Windows VK `65`), not Return. On the third isolated run, the title screen visibly highlighted **`NEW GAME`** after Down + mapped A. The fourth run sent Down + two mapped A presses to the owned window; an independently viewed screenshot showed the actual **new-game difficulty selection** screen with `Easy / Normal / Hard` choices and a `PLAY` button. This is stronger than loader-only smoke and establishes that menu selection operates in a real renderer. No `NCA bad header` or engine Critical/Fatal messages occurred in those clean-profile runs. **`PLAY` button confirmation and subsequent gameplay have not yet been established** by these screens.
- A final controlled attempt sent Down/A/A/Down/A against another verified owned Eden window. The process stayed alive, the engine logged **0 critical/fatal or NCA bad-header errors**, but the independently inspected screenshot still displayed the same **difficulty selection and PLAY button**. This does **not** count as entering gameplay; it confirms stable rendering after the additional input without proving the intended PLAY action.

## Final validation and release

- NSL `cargo test --locked --all-targets -- --test-threads=4`: **196 passed, 0 failed, 27 ignored** (223 registered).
- Vendored NSRP core `cargo test --locked -p nsrp-core --lib -- --test-threads=4`: **103 passed, 0 failed, 2 ignored** (105 registered).
- `cargo fmt --all -- --check`, core `cargo fmt --manifest-path vendor/nsrp-core/Cargo.toml -- --check`, NSL and core `cargo clippy --locked --all-targets -- -D warnings`: **all PASS with zero compiler/lint warnings**.
- `cargo build --locked --release`: **PASS**, executable `D:\NSW.dev\NSL\target\release\nsl.exe`, **22,022,656 bytes**, SHA-256 **`D4E7129D65A71874EF1192DBAB616DC80B97CEB4FF4E29E7341274ED25B725F1`**.
- Final release GUI started from an isolated temporary portable copy, remained running after eight seconds and had a nonzero Windows main window handle; the test copy hash matched the release executable. The QA-created NSL GUI instance alone was terminated, and its disposable folder removed.
- Stage 5 fresh NSZ SHA and three real structural preflights passed (see above). Eden real title screen and difficulty menu were independently inspected, and all emulator executions used temporary owned PIDs only.
- After all emulator sessions exited, cleanup removed **365,488,950 bytes** of disposable QA artifacts: the fast NSZ duplicate, copied Eden executable/profile and all private `prod.keys` / `title.keys` files. **0 private key files and 0 `.partial` files remain** in the Stage 5 QA tree. Six small PNG screenshots plus text logs remain as evidence; no source game or Stage 3 reference was deleted.

## Remaining work / acceptance boundary

- Retail, signed XCI/XCZ plus matching update **were not found in the available authorized sample folders**, so physical-size XCZ/XCI update-merge acceptance remains open. Synthetic HFS0 and XCI/XCZ tests are valuable regressions but cannot replace independent retail media.
- Eden title-menu rendering and 66-second survival do **not** prove gameplay completion, sustained performance, or Nintendo Switch hardware compatibility. Do not represent this stage as a full game playthrough.
- The NCZ preflight intentionally checks **declared metadata bounds**, not the authenticity or the decompressed cryptographic contents of every game. Full NCA SHA-256 equality was independently verified for the real Horror Tale 2 merge only in this stage; the separate Stage 4 checks covered Horror Tale 2 and UFO 50, and Stage 3 included the 8GB case.

## Reproduction (explicit opt-in on isolated owned copies only)

```powershell
cd D:\NSW.dev\NSL
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
cargo test --locked real_package_ncz_passthrough_preflight_readonly -- --ignored --nocapture

$env:NSL_QA_PATCH='E:\NSL_QA_20261120\small\patch.nsp'
$env:NSL_QA_BASE_ID='0100F7301AAB6000'
$env:NSL_QA_VERSION='65536'
$env:NSL_QA_EXPECTED='E:\NSL_QA_20261120\small\[NSL Merged] [0100F7301AAB6000][v65536].nsz'
cargo test --locked real_package_fast_nsz_equivalence -- --ignored --nocapture
```

The optional `NSL_QA_EDEN_EXPORT` variable can be set to a **fresh** `.nsz` path with an existing parent inside `NSL_QA_ROOT`. This produces a private emulator QA artifact without overwriting any existing file. Keys are not automatically exported.
