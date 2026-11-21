# NSL Stage 14 — Suika Game 29-DLC cumulative NSZ: real Eden loading

Date: 2026-11-21 KST. Stage 13 native merger baseline: `9790db0d303e23c83f4fb9df6f05c905fa130207`.

## Acceptance boundary

**PASS: Real Suika Game title screen and the three-item start menu displayed in Eden's actual GPU-rendered window.** The user requested emulator **loading only**, not entering gameplay, playing through a level, testing every DLC option, or Switch-hardware verification. The screenshot was independently inspected after the loader script captured the owned Eden window.

## Real-content fixture

- Game: Suika Game (Watermelon Game), Base Title ID `0100800015926000`.
- Source: real NSZ in `E:\NSW`, 1,372,476,535 bytes, with original Base, Update `v2490368` and 27 DLCs, read-only.
- New DLC #1: `010080001592701D` (Night Chill), NSP, 124,112 bytes.
- New DLC #2: `010080001592701C` (Waterside), NSP, 124,112 bytes.
- Native cumulative merger result: `1,372,555,717` bytes, **31/31 CNMTs**, Base + Update + **29 DLCs**. Existing Base/Update/DLC metadata and all original source-file SHA-256 hashes passed the prior-content-preservation and no-overwrite checks.
- An opt-in test-only `NSL_STAGE14_EDEN_EXPORT` output path under `E:\NSL_QA_20261120\stage14_eden_20261121` allowed exporting the **actual user-facing native merger** output after source integrity checks. The test required a new QA file, refused overwrite, streamed through a private temporary file, compared source/output SHA-256 and used atomic no-clobber publication.
- Export SHA-256: `83552dbf9e3325aa45ee9abde8c97755180662c5cea5a2364863d343e5a95898`. Export stage: **1 PASS / 0 FAIL**, 525.04 seconds including full re-run of two real DLC merges and original/merged/export SHA-256 checks.

## Isolated Eden loader verification

The existing, previously used `scripts/qa_eden_loader_smoke.ps1` performed a controlled loader-only smoke with:

- Eden CLI source: `D:\NSW.dev\eden\eden-cli.exe`, copied to a new disposable portable profile.
- Game and ticket source: the same **fully merged 29-DLC NSZ** under the Stage 14 QA directory.
- Private `prod.keys` copy and ticket-derived `title.keys` inside the temporary portable profile only. **No key values were printed, committed or saved in QA reports.** Existing installed Eden settings, user saves and global key files were not modified.
- Existing emulator-process check before starting, plus owned executable/PID and window-handle checks. It terminated **only its own isolated Eden process**.
- Real GPU renderer window observed for **45 seconds**; window dimensions **1296×759**.
- Window screenshot color sample: **190 distinct pixels/colors** across the 24×16 sampling grid; screenshot visually confirmed the **Suika Game Japanese title** and `ゲームスタート` (Game Start), `マイスコア` (My Score) and `遊び方` (How to Play) menu buttons.
- Loader log present; GPU renderer confirmed; `NCA bad header` / `0007-000C` / `Error 12`: **absent**; critical/fatal errors: **0**; game NSZ SHA-256 unchanged: **true**.
- Output: `NSL_EDEN_LOADER_ONLY_PASS`, process exit code 0.
- Private profile and temporary `prod.keys`/`title.keys` copies: **zero remaining**. Other emulator instances: untouched.
- After the successful loader smoke, the exported QA-only NSZ was independently SHA-256 checked once more and then **removed**, releasing **1,372,555,717 bytes** of disposable storage. The screenshot and JSON report were retained. No original library file or user save was deleted.

## Final code regression

- NSL full Rust suite: **209 PASS / 0 FAIL / 38 ignored**.
- NSRP vendored core: **119 PASS / 0 FAIL / 3 ignored**.
- `cargo fmt --all -- --check`, strict `cargo clippy --locked --all-targets -- -D warnings`, Git whitespace check: **PASS**.
- The Stage 14 repository change is **opt-in QA/test-only export support** and this QA report. The Stage 13 production `target\\release\\nsl.exe` binary and its already validated native merge runtime have not been modified.

## Reproduction

To generate a fresh, separate export, create a new dedicated QA child directory, set `NSL_STAGE13_SOURCE_ROOT`, `NSL_STAGE13_BASE`, `NSL_STAGE13_DLC_DIR`, `NSL_STAGE13_QA_ROOT` and `NSL_STAGE14_EDEN_EXPORT` (new `.nsz` path inside the new QA child). Run the existing ignored opt-in Rust test:

```powershell
cargo test --locked isolated_stage13_real_two_dlc_cumulative_nsz_wrapper -- --ignored --nocapture --test-threads=1
```

Once the exported file exists and its hash matches, invoke the isolation-preserving Eden script:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\qa_eden_loader_smoke.ps1 -Game "<isolated merged 29 DLC NSZ>" -TicketNsp "<same isolated merged NSZ>" -EdenCli "D:\NSW.dev\eden\eden-cli.exe" -ProdKeys "D:\NSW.dev\NSL\prod.keys" -QaDir "<private QA directory>" -ObserveSeconds 45 -RunLabel suika29dlc
```

## Evidence (local; no secrets)

- `docs\qa\QA_NSL_STAGE14_REAL_DLC_EXPORT_LOCAL_2026-11-21.txt`: complete real-content merger and export log
- `docs\qa\QA_NSL_STAGE14_EDEN_SUIKA_2026-11-21.txt`: `NSL_EDEN_LOADER_ONLY_PASS` and window statistics
- `E:\NSL_QA_20261120\stage14_eden_20261121\eden_loader_suika29dlc.png`: independently inspected real title/menu screenshot
- `E:\NSL_QA_20261120\stage14_eden_20261121\eden_loader_suika29dlc.json`: 45-second loader, renderer, error and immutability fields

The user's requirement is met at the **game title/menu loading** level. Gameplay input, in-game DLC activation, Switch-hardware verification and live torrent-peer downloading were **not** tested and should not be implied.
