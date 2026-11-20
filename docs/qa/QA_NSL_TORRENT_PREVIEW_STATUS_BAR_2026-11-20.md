# NSL: stale torrent file-preview status bar message

Date: 2026-11-20 (KST). Previous baseline: `8c1eb90`.

## Reproduction and root cause

Clicking the download button in Store queued a magnet-metadata-only preview (`TorrentEngine::preview_files`) and assigned the permanent `NslApp.status` string `토렌트 파일 목록 확인 중…`. The metadata preview dialog could successfully resolve, error, lose its worker channel, or be canceled by clicking `취소` / pressing `Esc`, but none of those paths cleared this stored string. The bottom status bar falls back to `self.status` when there is no foreground progress view. Consequently it continued to display **"checking torrent file list" even when no preview job was running**. This is a stale UI message, not evidence of a long-running download.

## Fix

- Remove the permanent loading assignment from `start_catalog_torrent`.
- Include `ProgressView::busy("토렌트 파일 목록 확인")` **only while a live `CatalogTorrentDialog.request` exists**. The status bar and progress tooltip both derive this activity from that live state.
- On successful resolution, record `토렌트 파일 목록 준비 완료 · N개` and display the file selection tree. On an invalid preview, worker failure or channel disconnection, show the reason both in the dialog and the status bar.
- On `Esc` or Cancel, drop the dialog and its `TorrentPreviewRequest` (its Drop flag actually cancels the async metadata task); show `토렌트 파일 선택 취소됨` instead. Request a fresh frame when the request completes so the status bar promptly stops showing its busy indicator.
- The change does **not** start torrent payload transfers, alter ongoing download jobs, modify user data, or kill an existing NSL process.

## Regression QA

Two new headless egui integration tests use a synthetic mpsc metadata response channel and assert actual status bar text alongside engine cancellation signals:

1. `torrent_preview_status_bar_only_shows_loading_while_request_is_pending`: prior status stays intact while waiting, status bar shows an indeterminate preview task, success shows the correct file count, and clicking Cancel removes the dialog, the busy task and the stale status. Several subsequent rendered frames stay correct.
2. `torrent_preview_failure_disconnect_and_escape_clear_busy_status`: explicit metadata failure, disconnected response channel and Esc during pending resolution each clear the progress indicator, display their proper status and cancel the request (where applicable).

Commands:

```powershell
cd D:\NSW.dev\NSL
cargo test --locked --bin nsl torrent_preview_ -- --nocapture --test-threads 1
cargo test --locked --all-targets -- --test-threads 4
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
.\scripts\qa-native-gui-window.ps1 -ExpectedCommit (git rev-parse --short HEAD)
```

Verified locally: **2 targeted regression cases passed**; full default suite **183 passed, 0 failed, 17 ignored**, Clippy `-D warnings`: **PASS**. Only synthetic metadata channels were manipulated, not the user's download session or any public torrent peers.

The production `target\release\nsl.exe` must **not be overwritten while the user has that process running**: Windows holds an image lock. Keep the existing application alive and either wait for a user-initiated exit or build to an isolated staging path until a safe replacement is possible. Avoid extra permanent executable variants in `target\release`.
