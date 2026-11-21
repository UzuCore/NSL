# NSL Stage 12 — Missing DLC catalog audit, download queue and cumulative merges

Baseline: Stage 11, `427c6668b1cc549caf42b0fc93133e9ff41b8df3` (2026-11-21 KST).

## User workflow

The **미보유 DLC** filter now has the same independent `카탈로그 전체 대조` toolbar used by **미보유 업데이트**. DLCs are displayed as distinct selectable rows under each owned base game; a `+` marking is shown on the game when a catalog candidate has been resolved. The user can select/deselect all, inspect per-DLC audit results, enqueue only selected torrent files, observe download/merge state and live stage progress, cancel one active native merge, and retry a failed merge without downloading again.

## Safety and correctness rules

1. DLC candidates are indexed by *base* Title ID but accepted only when an actual `.nsp` / `.nsz` torrent filename contains the exact **16-digit DLC Title ID**. No filename-only proof is accepted after download; native/header CNMT must independently prove DLC content type, exact ID and the claimed version (including valid `v0`). The existing update version and CNMT rules remain separate.
2. `BatchKind::Dlc` and each row's `dlc_id` reuse the existing update-batch poll, task queue and journal. A shared per-release preview cache avoids re-fetching the same multi-DLC torrent metadata for every DLC ID, with the existing **128 MiB total metadata cap**. **Selected DLC files in the same torrent infohash are grouped into one torrent registration**, with the union of selected file indices. Different tracker URLs for the same torrent do not cause duplicate registrations. Each DLC is tracked separately in the durable journal, sharing a transfer key but not a merge worker. Completed transfer jobs retain the read-only original and downloaded packages.
3. Same-game DLCs are processed **sequentially**, never independently against the same base. A later DLC's input is the most recently published merged DLC output for that base; if a previous selected DLC has not finished or has failed, the later job does not skip it. The global native merge concurrency remains one. A separate `[NSL DLC] [BASE][DLC][vVERSION]` output filename prevents unrelated DLC results from colliding.
4. All four original game container formats are preserved: NSP → NSP, NSZ → real NCZ-containing NSZ, XCI → XCI, XCZ → real NCZ-containing XCZ. The same Stage 10 direct-XCI path removes a redundant merged NSP. Every output must contain the Base CNMT, the specific new DLC CNMT and **every pre-existing update/DLC CNMT** from its input. Existing or mismatched files cannot be overwritten; validation precedes atomic publication.
5. Stage 11 atomic cancellation and measured step progress are reused. Recovery verifies an already published result, including previous content preservation, before treating it as complete. `batch-updates.json` stores a backward-compatible default `Update` kind, or explicit `Dlc` kind and DLC ID. Runtime cancellation flags and torrent metadata are never serialized; stale verifying jobs requeue under the existing completed-transfer gate.
6. Switching between Missing Updates and Missing DLC while another batch is ongoing does **not** silently replace its state. A completed batch of the other kind can be replaced by a fresh audit. No existing official release, real game archive, key file or downloaded update/DLC package is removed by this work.

## Tests

| Check | Result |
| --- | --- |
| Strict DLC filename ID matching (including v0, wrong ID, empty or wrong extension) | PASS |
| Multiple DLCs in one torrent: shared preview, one download group, exact selected indices, tracker URL variants, all-row completion | PASS |
| Missing DLC UI toolbar & exclusion rules | PASS |
| DLC per-title queue dependency, failure blocking, cumulative source path | PASS |
| DLC result filename uniqueness, base ID and CNMT validation, previous DLC/update preservation | PASS |
| DLC crash journal round-trip, version 0 and wrong ID rejection | PASS |
| Read-only real DLC NSP CNMT check in isolated folder | PASS, 1/1; wrong DLC ID rejected; 121,040-byte input unchanged |
| NSL all-target regression | 208 PASS / 0 FAIL / 35 ignored |
| NSRP core tests | 115 PASS / 0 FAIL / 3 ignored |
| Format + strict Clippy | PASS |

Real DLC evidence uses a locally available *read-only* `Watermelon Game (Suika)` DLC NSP with DLC Title ID `0100800015927001`. The test copies only the small DLC into an automatically cleaned temporary `downloads/qa` directory. The output is validated with the existing native scanner and a deliberately incorrect DLC Title ID is rejected.

Opt-in command:

```powershell
cd D:\NSW.dev\NSL
$env:NSL_DLC_QA_FILE='E:\NSW\_dlc\Watermelon Game (Suika) [DLC Multi-Player Mode Expansion Pack] [0100800015927001][v0].nsp'
cargo test --locked isolated_real_dlc_cnmt_validation_rejects_other_title_ids -- --ignored --nocapture
```

## Limits / outstanding integration checks

- A catalog torrent whose filename does not identify a specific DLC Title ID remains **unresolved**, even if its payload might contain DLCs. NSL deliberately does not guess the DLC ID from a release title or icon. DLC packages are supported as `.nsp` or `.nsz` with a verifiable internal CNMT.
- Native DLC merging and accumulation are covered by unit/correctness checks, but a **complete live torrent download → multi-DLC cumulative package → signed runtime boot** was not performed during Stage 12. Such a test requires a matching authorized base+multiple DLC fixture and real catalog peers; missing evidence is not represented as PASS.
- Before future release publication, independently test multiple DLCs on one title across a crash/restart, including NX retail signatures/rights-controlled content when an authorized sample is available. Do not treat synthetic XCI container success as loader acceptance.
