"""Stage 20 Windows GUI smoke test in private temporary installations.

Exercises grid, store and downloads at 980, 1280 and 1456px. The shared
fixture makes a read-only snapshot of the user's library, removes all scan
folders and disables auto-updating. No user keys or downloads are used.
"""

from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]
TEMPLATE = ROOT / "docs" / "qa" / "qa-stage9-isolated-catalog-ui.py"
EXPECTED_COMMIT = subprocess.check_output(
    ["git", "rev-parse", "--short=7", "HEAD"], cwd=ROOT, text=True
).strip()


def main() -> None:
    template = TEMPLATE.read_text(encoding="utf-8")
    for tab, label in [
        ("", "GRID"),
        ("CATALOG", "STORE"),
        ("DOWNLOADS", "DOWNLOADS"),
    ]:
        script = (
            template.replace(
                "NSL_STAGE9_REAL_CATALOG_R10", f"NSL_STAGE20_{label}"
            )
            .replace(
                '"last_tab_key": "CATALOG"',
                f'"last_tab_key": "{tab}", "view_mode": "grid"',
            )
            .replace(
                'print("REAL_CATALOG_UI_WINDOW_TITLE", win32gui.GetWindowText(handle))',
                "print('STAGE20_GUI_TITLE', win32gui.GetWindowText(handle))\n"
                f"            assert {EXPECTED_COMMIT!r} in win32gui.GetWindowText(handle), "
                "'GUI executable commit differs from current source'",
            )
        )
        print(f"STAGE20_TAB={label}", flush=True)
        exec(
            compile(script, str(TEMPLATE), "exec"),
            {"__name__": "__main__", "__file__": str(TEMPLATE)},
        )
        print(f"STAGE20_{label}_GUI=PASS", flush=True)


if __name__ == "__main__":
    main()
