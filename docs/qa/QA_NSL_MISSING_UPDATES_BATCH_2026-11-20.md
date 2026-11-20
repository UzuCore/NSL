# NSL — Missing Updates catalog audit and selective batch queue

Date: 2026-11-20 KST. Baseline: `1a10531`.

## Scope implemented

- Only the **미보유 업데이트** tab displays the new **카탈로그 전체 대조** action. Other library tabs, Store, and Downloads retain their existing navigation.
- Build a title-ID-indexed candidate list from the installed game rows that have a newer known title DB update version, honoring the user's existing excluded-update IDs. Catalog entries without a valid base title ID or magnet are omitted. At most three ranked release candidates are probed per game, in sequence.
- Use the existing librqbit **list-only** metadata preview, with progress in the shared status bar, cancellation and an explicit **대조 중단** action. This does not download payloads.
- Limit retained torrent metadata across all confirmed candidates to **128MiB**, so hundreds of large torrent descriptors cannot exhaust the UI process's memory.
- Require the actual torrent file entry to be an **NSP/NSZ** with the correct application patch ID (`base | 0x800`) and explicit `[vNUMBER]` metadata; its version must exceed the installed version and be at least the DB's known latest version. A candidate without such evidence is **not** shown as downloadable.
- Overlay a small theme-colored **↑** on qualifying grid icons, show ↑ in list names, and provide an expandable review table with per-game checkboxes, Select All / Clear All and a second confirmation before queuing.
- Enqueue **only the matching original torrent file indices** through the current selective-download engine into distinct, private per-batch subfolders under `downloads`. Never overwrite existing game, update or download paths. Existing pause/resume/cancel/seed/peer UI remains available.
- On an actual torrent **completion notice** (not 100% progress alone), scan the downloaded package and require trusted `header_native` or `header_cnmt` metadata matching the exact patch title ID and expected version. Unverifiable packages remain downloaded but are **not** merged automatically.
- For **NSP base + NSP patch only**, stream-copy the original package entries into a new strict PFS0 package. Reject invalid input indices and conflicting entries, flush/sync the output, confirm the output contains the expected CNMT, and publish atomically via `persist_noclobber` to a **separate** `[NSL Merged] [TitleID][vVersion].nsp` beside the base. Originals and downloaded update files are left unchanged. Request an incremental library rescan afterward.

## Explicit current limits / remaining work

**This is not the final full requested merge/compress workflow.**

1. **NSZ/XCZ original packages:** The downloaded patch is verified and retained, but automatic **compressed merge output is not yet implemented**. NSRP has merge and native recompression code in `D:\NSW.dev\NSRP\rust_port\src\ops\{merge,compress,xcz}.rs`; it needs careful integration and real-game roundtrip QA. Renaming an uncompressed NSP/XCI to NSZ/XCZ is forbidden.
2. **XCI base + downloaded NSP/NSZ:** No standalone safe native XCI partition writer has been integrated into NSL's batch processor. The verification stage still runs; originals are never rewritten.
3. **Durability:** Batch selection and the automatic post-download merge job mapping live in memory; the torrent engine's own downloads persist, but batch post-processing requires NSL to remain open. A durable batch manifest and restored-job reconciliation are needed before this is a fully automatic overnight workflow.
4. **Catalog accuracy:** The current local cached catalog has **7,344 entries**, **7,002** valid 16-character title IDs, but no per-entry numeric update version or files list. Probing peers can take time and unavailable magnet metadata is reported, not guessed. No public-peer or 4GB+ compression E2E validation has been run for this feature.

## Upstream review and licensing

- TorrentShopNX `source/ui/LibraryView.cpp::updateAllGames`, `source/catalog/release_picker.hpp` and `source/catalog/game_version.hpp` were **read for behavior**, not copied. TorrentShopNX is GPLv3; avoid importing its source into NSL without license review.
- NSRP `rust_port/src/ops/merge.rs`, `compress.rs`, `xcz.rs` are the MIT-licensed reference. NSL's new conservative PFS0-only writer is standalone and does not bundle NSRP's GUI, external binary tools or compression engine.

## Local QA

Run from `D:\NSW.dev\NSL`:

```powershell
cargo fmt --all -- --check
cargo test --locked --all-targets -- --test-threads 4
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
.\scripts\qa-native-gui-window.ps1 -ExpectedCommit (git rev-parse --short HEAD)
```

The synthetic tests use temporary files and application state; no existing user's game archive, DB, configuration, torrent session or emulator process is modified. Actual live torrent completion, rights-protected CNMT extraction and compressed output are separate, explicitly unverified acceptance requirements.
