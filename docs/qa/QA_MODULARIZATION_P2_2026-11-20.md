# NSL P2 main.rs modularization / source-only QA

Date: 2026-11-20 (KST)

## Scope

- Moved **62 existing `NslApp` methods** to 10 dedicated `src/app/*.rs` modules.
- Moved `#[cfg(test)] mod runtime_tests` to `src/runtime_tests.rs` with the same test names/attributes.
- Reduced `src/main.rs` from **13,212 to 1,764 lines** by extracting implementation and tests (not by deleting functionality).
- Kept `NslApp` state, binary startup, `eframe::App` event bridge, and shared layout utilities in `src/main.rs`.
- No feature behavior, persistence format, UI text, database schema, or thread scheduling intentionally changed.
- Changed only the moved methods' visibility to `pub(crate)` so they can be called from sibling modules and the crate root.
- Module architecture rationale: `docs/architecture/NSL_APP_MODULES.md`.

## Validation performed without a build

| Check | Result |
| --- | --- |
| Baseline source: commit `0eeb36d7fd0bc3ed21f317dce65e13fe7bc381df` | Read-only snapshot available |
| All original method names accounted for exactly once | PASS: 62/62 |
| Generated module source compared against baseline after identical rustfmt normalization | PASS: 10/10 modules |
| Original regression test module compared against moved test file after rustfmt normalization | PASS: identical |
| Original root entry point / app state / shared utilities reconstructed and compared | PASS: only rustfmt removed two extra blank lines |
| `cargo fmt --all -- --check` | PASS (syntax parsing and formatting) |
| `git diff --check` | PASS |
| Local binary build / cargo test / cargo clippy / GUI launch | **NOT RUN**, user requested manual builds |
| GitHub Actions dispatch / new GitHub release | **NOT RUN**, user requested manual builds |

The static parity checks compare the module files to an independently reconstructed source from `git show 0eeb36d:src/main.rs`, after applying only the internal visibility annotation and the same rustfmt normalization. The exact prior test bodies are likewise reconstructed and checked. This establishes a lossless **source move**, not compiled runtime correctness.

## Follow-up required after manual build

1. Compile the new modules with `cargo test --locked --all-targets` (prior Stage 20 baseline: 151 pass / 15 ignored / 0 fail).
2. Run Clippy with `-D warnings`; pay attention to internal import visibility and cross-module method resolution.
3. Exercise Store / Downloads / Settings / Library and modal dialogs in a **separate test installation**. Validate the Stage 20 torrent file picker, delete confirmation, and seed/peer labels.
4. Build the release manually and verify its ZIP/manifest/executable SHA before changing the official release.

The source restructuring itself creates no new build artifacts, Windows executables, releases, or GitHub tags. Existing user library data, keys, configuration, downloaded files, and historical local binaries remain outside the change scope.
