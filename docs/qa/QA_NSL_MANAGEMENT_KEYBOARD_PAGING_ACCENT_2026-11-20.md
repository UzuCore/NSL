# NSL Store / Downloads keyboard, Store paging, and active tab accent

Date: 2026-11-20 (KST). Previous commit: `f2b1f09`. Source files: `src/main.rs`, `src/app/frame.rs`, `src/app/store.rs`, `src/app/downloads.rs`, `src/app/settings.rs`, `src/runtime_tests.rs`, README.

## Behavior

### Store and Downloads keyboard navigation

- Both lists accept **Arrow Up/Down/Left/Right** to move one row at a time, **Page Up/Down** to jump approximately one viewport, and **Home/End** to reach the first/last matching item. Navigation crosses page boundaries, updates the selected card and correct page number, and reveals the selected row inside the visible scroll area.
- The Store's **Enter** key opens the selected game's existing torrent file-selection dialog (which still requires explicit confirmation before transferring any files).
- The Downloads list's **Enter** key safely toggles **pause/resume** for the selected active job only; it does not delete downloads or change completed/failed jobs.
- Navigation uses the currently filtered and sorted list, not an unrelated hidden row. Search TextEdit focus, Settings, custom confirmation windows and built-in egui menus suppress these shortcuts. The earlier Escape behavior remains unchanged.
- Existing mouse selection and row action buttons remain independent of keyboard selection.

### Store page count

- The Store always paginates, **regardless of `paging_on`** (which controls only the standard library's paginator).
- It uses the same positive `page_size` setting shown under **Settings > Paging > Titles per page**, validated up to 99,999 titles (fallback 500). The input remains editable even if library paging is disabled, and changing it resets the Store to its first page.
- Downloads retain the existing **100 jobs per page**. Existing 100-item Store regression fixtures now explicitly configure `page_size=100` rather than depending on the removed hard-coded limit.
- The virtualized Store list loads only visible row cards even with a large configured page size. Pager clicks and keyboard navigation keep selection on a visible page.

### Active tab indicator

- Every selected library/directory, Store or Downloads tab has a **2-point (~2px at 100% UI scale) top bar** drawn inside its unchanged tab rectangle and click area.
- The color uses the active theme's **selection/link accent** (`egui::Visuals.hyperlink_color`, mapped from `Palette.accent2`) to match selected card borders rather than its action-button accent. On GitHub Dark, this is **blue `#1F6FEB`**, instead of green `#238636`. Other themes continue using their own palette accent2. There is no line on inactive tabs.

## Actual local regression tests

New headless egui tests (no game or network writes):

1. `store_uses_configured_page_size_even_when_library_paging_is_disabled`: 9 synthetic items, `paging_on=0`, page size 3 then 2, including last-page remainder.
2. `store_and_downloads_keyboard_navigation_crosses_page_boundaries`: nine Store entries and 105 synthetic download jobs, ArrowDown/Right, End, Home, cross-page selection, search focus and modal suppression, independent download 100-item page limit.
3. `active_tab_draws_two_pixel_selection_accent_for_each_screen`: actual tab geometry, 2-point size and palette color across **GitHub Dark, GitHub Light and Catppuccin Mocha**, for All, Store and Downloads tabs, including the explicit blue `#1F6FEB` expectation on GitHub Dark.
4. `management_keyboard_end_reveals_selected_row_in_long_scrollable_lists`: virtualized Store page with **120 rows**, Downloads page with **100 jobs**. Both keyboard-selected final items must be rendered fully within the real egui viewport. This test detected that the first Downloads scroll-to-rect implementation failed to reveal a far-away selected row in headless execution; the fix now stores the precise scroll offset from the selected card's actual geometry for immediate visibility on the following frame.

After updating two old Store page-size assertions for configurable paging, the complete `cargo test --locked --all-targets -- --test-threads 4` suite passed: **181 passed, 0 failed, 17 ignored**. `cargo clippy --locked --all-targets -- -D warnings`: **PASS** (zero warnings). Runtime tests do not launch a production download, visit a network service, modify a live library or press keys globally on Windows.

## Reproducible local release verification

```powershell
cd D:\NSW.dev\NSL
cargo fmt --all -- --check
cargo test --locked --all-targets -- --test-threads 4
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/qa-native-gui-window.ps1 -ExpectedCommit (git rev-parse --short HEAD)
```

**Build after committing:** `build.rs` embeds the Git commit in the executable title, so the final local release must be built after the new source commit. `target\release\nsl.exe` is the user's canonical local test binary. Do not dispatch GitHub Actions or modify the official r22 release as part of this work.

## Limitations

The UI is exercised through the real egui update loop and synthetic pointer/keyboard events, plus isolated native Win32 window startup/size checks. An exhaustive visual inspection or physical keyboard walkthrough on the user's desktop, real multi-hour transfers, and end-to-end game launching are outside this source/UI regression scope.
