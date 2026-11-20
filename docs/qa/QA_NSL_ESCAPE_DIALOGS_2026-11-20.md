# NSL Escape dialog, settings and log dismissal QA

Date: 2026-11-20 (KST). Baseline: `0149f6d`. Source: `src/app/frame.rs`, `src/runtime_tests.rs`. This change does not start or cancel a scan, download or other background task; it affects only UI dismissal and search clearing.

## Required behavior

- `Esc` closes **one window per keystroke**, preferring the one rendered above other overlays. An egui foreground menu/combobox popup is closed first, followed by the last-drawn NSL centered popup.
- All **17** custom NSL popup categories are handled: rename confirmation, torrent file picker (including its metadata-loading state), download deletion confirmation, batch rename confirmation, title DB file information, title DB reset, library reset, scan-folder removal confirmation, folder notice, unused-media cleanup, conversion-workspace cleanup, update information, prepared-update confirmation, cache information, keyboard shortcuts, About NSL, and running-task exit confirmation.
- When no popup is open, `Esc` closes the Settings screen. Once Settings is closed, **`Esc` closes the docked log panel, even if it is the only visible auxiliary panel**.
- Only when no popup, Settings, or log is open does `Esc` use its previous library/store/download search-clear behavior. This includes clearing the committed/debounced search state so stale filters do not remain.
- Dismissal never selects Delete, Clear, Rename, Exit, Update/Restart or any other consequential confirmation button. Search/filter input behind a dismissed overlay is preserved. A pending torrent file-preview request is canceled by dropping its preview dialog; an already running torrent download is untouched.
- The About/Shortcuts panel documents the new `Esc` hierarchy.

## Implementation

One `dismiss_topmost_overlay()` method in `src/app/frame.rs` uses the **reverse of the actual rendering order** to select the last/uppermost custom popup. The main Escape handler checks built-in egui foreground popups first (`memory.any_popup_open`/`close_popup`), then centered dialogs, then Settings, then the docked log, and finally the old search-clear path. When an overlay is dismissed, that Escape keypress is consumed so underlying editors and widgets do not also act on it. The former separate Escape test inside the torrent file-picker dialog was removed to avoid double handling.

## Local verification

`cargo test --locked --bin nsl escape_ -- --nocapture --test-threads 2` checks five focused cases, including:

1. All 17 rendered dialog categories close individually while Settings/log remain open, without changing any of three search text buffers or triggering exit.
2. Multiple simultaneously open dialogs close one at a time in reverse rendering order; next `Esc` closes Settings, then log, then clears filters.
3. An egui context menu closes before the centered dialog, Settings, log and search.
4. Pre-existing search clearing and Settings `Esc` behavior remain correct.

Focused test results: **5 passed, 0 failed**. The full default Rust test suite: **177 passed, 0 failed, 17 ignored**; Clippy with `-D warnings`: **PASS**. Rebuild the local release executable and run the isolated Win32 GUI smoke **after** committing these sources. The main executable used for local testing must be updated at `D:\NSW.dev\NSL\target\release\nsl.exe`, with the **new** commit number embedded. No GitHub Actions or official GitHub Release updates are required. No production library DB, ROMs, keys or downloads are used for the focused headless keyboard tests.

Remaining real-desktop manual QA: combined IME focus and actual modal buttons under unusual Windows focus conditions. Headless egui event regressions are not equivalent to an exhaustive screenshot or physical keyboard test.
