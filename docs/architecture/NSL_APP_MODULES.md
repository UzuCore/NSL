# NSL application module boundaries

2026-11-20 / Windows Rust application / source-only refactor

## Entry point and shared state

- `src/main.rs`: binary entry point, `NslApp` state definition, application-level layout helpers, top bar and headless entry point. It contains the single `eframe::App` implementation and registers the application modules. The state struct was intentionally not rewritten or duplicated.
- `src/app/mod.rs`: module registry. App subsystems implement separate inherent `impl NslApp` blocks instead of adding methods to `main.rs`.
- `src/runtime_tests.rs`: the existing application-level Rust regression tests, moved intact out of `main.rs` under the same `#[cfg(test)] mod runtime_tests` module name.

## Application subsystems

| Module | Responsibility | Representative methods |
| --- | --- | --- |
| `src/app/startup.rs` | App initialization, persisted state, and service setup | `new`, `with_base_dir` |
| `src/app/scanning.rs` | Library reload and incremental/full scanning | `start_reload`, `start_scan` |
| `src/app/images.rs` | Lazy image loading, caches, cover prefetch, performance sampling | `poll_loaded_images`, `start_cover_prefetch` |
| `src/app/store.rs` | Store catalog, details, filter, and torrent-file selection | `catalog_panel_ui`, `start_catalog_torrent` |
| `src/app/downloads.rs` | Download list, sidebar, state and deletion controls | `downloads_panel_ui`, `download_row_buttons` |
| `src/app/maintenance.rs` | Title DB/media/background conversion and cleanup | `start_titledb_download`, `start_media_collection` |
| `src/app/library.rs` | Library queries, navigation, item actions, bulk rename/export, detail view | `library_rows_cached`, `selected_detail_ui` |
| `src/app/updates.rs` | Check and prepare app updates | `start_update_check`, `start_update_download` |
| `src/app/settings.rs` | Settings panel and cache status | `settings_panel_ui` |
| `src/app/frame.rs` | UI frame orchestration, progress, panels and modal dialogs | `ui_frame`, `active_progress_views` |

Existing `scanner.rs`, `titledb.rs`, `torrent.rs`, `native_nca.rs`, etc. remain the domain engines. Application modules orchestrate them; engines do not depend on app UI.

## Rules for subsequent changes

1. Keep a single owner for app state (`NslApp` in `main.rs`); add business/UI actions to the relevant subsystem rather than expanding `main.rs`.
2. Put new UI functions beside the tab or feature they belong to. Put cross-tab runtime orchestration in `frame.rs` and persistent app initialization in `startup.rs`.
3. Preserve background worker channels, error handling, and user-data boundaries. Do not move DB or filesystem side effects into rendering helpers.
4. `pub(crate)` on moved inherent methods is **crate-internal** visibility required by the sibling implementation modules and the root event loop. It is not a public API.
5. The current subsystem files use `use crate::*` to preserve access to the original root-scoped helpers without a simultaneous import refactor. Replace it gradually with explicit imports **only after compiling and running regressions**, to avoid two unrelated changes in one commit.
6. Keep isolated GUI/torrent tests and real-corpus tests separate from end-user game/DB/config files.

## Review and verification status

This is a **behavior-preserving extraction**, not a new feature implementation. The original 62 inherent methods and the test module have been compared with their relocated versions after rustfmt normalization. All matched. Rustfmt syntax/format validation passed.

Per the user's manual-build policy, **no Rust compilation, cargo tests, Clippy, GUI launch, GitHub workflow dispatch, or release creation** was performed for this refactor. The previously reported 151-passing tests belong to Stage 20 and **do not validate this unbuilt modularization**.

### Manual verification before publishing a new binary

```powershell
cd D:\NSW.dev\NSL
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
# Run GUI and torrent E2E checks in an isolated environment.
# Only then run your own build.bat or manual GitHub Actions workflow.
```

Future cleanup candidates: extract distinct dialog builders from the large `ui_frame` method after compilation is available, split `runtime_tests.rs` into test-only feature modules with shared fixtures, and reduce broad imports once module boundaries have passed a full Rust type-check.
