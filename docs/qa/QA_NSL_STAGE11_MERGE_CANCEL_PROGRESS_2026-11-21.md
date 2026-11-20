# NSL Stage 11 — cancellable batch merges and measured per-stage progress

Date: 2026-11-21 KST. Baseline: Stage 10 `ff07c7ac5fcdab33790f231149af77812cca8029`.

## Scope

The previous Missing Updates batch worker used a private, permanently false cancellation flag. During a lengthy native CNMT merge or NCZ recompression the user could neither cancel the operation nor distinguish it from an indefinite spinner. Stage 11 connects a dedicated `MergeControl` to each currently running native merge and exposes **one** `병합 중단` button on the Missing Updates toolbar. The existing limit of **one active merge** is preserved. Queued torrents and existing source/download files are never canceled or deleted by this control.

## Implementation

- `src/update_merge.rs`: `MergeControl` holds an atomic cancellation flag and a thread-safe `TaskProgress`. Every native selector/decompressor/compressor receives the real flag, including the fast NSZ path. CNMT preflight, output verification, recovery and final publication honor cancellation. Existing callers retain the original public crate API by using a default, non-cancelled control.
- Native `merge_packages[_selected]`, NSP-to-NSZ and XCI-to-XCZ callbacks feed *measured per-stage* progress to the existing footer. Each stage resets its own meter. Preflight, fast NSZ layout selection, verification and publication remain indeterminate if no actual progress total is supplied. **No fabricated total progress percentage** is shown.
- `publish_without_overwrite` checks cancellation before publishing and during the copy fallback on filesystems where hard links fail. It still uses atomic `persist_noclobber`. A cancellation before publication must never create a destination or replace a previous file.
- `src/app/update_batch.rs`: the worker owns one `Arc<MergeControl>` shared with the GUI. The toolbar button requests cancellation only for a running verification/merge worker and then shows `병합 중단 처리 중` until it finishes. The worker result is consumed before releasing its progress/control handle; a canceled job becomes a retryable failure and its downloaded package is preserved.
- `src/app/frame.rs`: the footer displays the active worker's actual stage label and measured stage fraction, or an indeterminate state for unknown work. `src/update_manifest.rs`: runtime control is **never serialized**. An interrupted `Verifying` worker is restored as `Queued` with a fresh cancellation state, under the existing transfer-completed preconditions.

## Acceptance and evidence

| Gate | Outcome |
| --- | --- |
| User-initiated cancel before final publication | Unit test: source intact, no destination |
| 46% measured progress and unknown-total stages | Unit test: correct snapshots, no fabricated total |
| Interrupted batch journal recovery | Unit test: stale cancellation flag not restored |
| Footer during active/cancel-requested worker | GUI-state unit test: stage name / cancelling label |
| **Real NSZ content → unsigned XCI + update**, canceled during CNMT/NCA selection | **PASS, 1/1, 84.18 s**, no published result; both input SHA-256 hashes unchanged; isolated temp workspace removed |
| Full NSL Rust regression | **202 passed, 0 failed, 34 ignored** |
| NSRP core regression | **115 passed, 0 failed, 3 ignored** |
| Rustfmt and strict Clippy (`-D warnings`) | **PASS** |
| Signed retail XCI/XCZ game loading | **NOT VERIFIED**: an authorized signed matching gamecard+update sample is not available |

### Reproduce opt-in cancellation test

```powershell
cd D:\NSW.dev\NSL
$env:NSL_QA_ROOT='E:\NSL_QA_20261120'
$env:NSL_QA_BASE='E:\NSL_QA_20261120\small\base.nsz'
$env:NSL_QA_PATCH='E:\NSL_QA_20261120\small\patch.nsp'
cargo test --locked isolated_native_xci_update_cancel_during_merge -- --ignored --nocapture --test-threads=1
```

The test creates a private, unsigned XCI from **read-only real Base NCA/CNMT** content, starts the *production* merge and waits for the `XCI 콘텐츠 선택·병합` stage before requesting cancellation. It then joins the worker and verifies that the output was never published, both source SHA-256 hashes are unchanged, and the RAII directory is gone. It does **not** pass an unsigned gamecard to an emulator or claim signed-gamecard loader acceptance.

## Final checks and remaining limitations

- Local test logs: `QA_NSL_STAGE11_REAL_CANCEL_LOCAL_2026-11-21.txt`, `QA_NSL_STAGE11_FULL_TEST_FINAL_LOCAL_2026-11-21.txt`, `QA_NSL_STAGE11_NSRP_FINAL_LOCAL_2026-11-21.txt`, `QA_NSL_STAGE11_CLIPPY_FINAL_LOCAL_2026-11-21.txt`.
- Stage 11 changes runtime cancellation/progress and UI only; it does not alter CNMT selection, title matching, catalog contents or the verified Stage 10 XCI/XCZ output algorithm.
- Cancellation is cooperative. Existing synchronous downloaded-package metadata checks may take time before honoring a request; native merge/decompression/compression and publication use the shared flag throughout. A cancellation request arriving *after* successful atomic publication cannot retroactively undo an already accepted result.
- Full keyboard/mouse clicking of the toolbar with a simultaneous real download session and actual Nintendo-signed retail XCI/XCZ loader boot remain environment-dependent follow-up checks. No other emulator instance or real user library data is modified.
