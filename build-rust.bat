@echo off
setlocal
cd /d "%~dp0"

echo Building Nintendo Switch Library (Rust)...
cargo build --release
if errorlevel 1 exit /b %errorlevel%

powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0package-rust.ps1"
if errorlevel 1 exit /b %errorlevel%

echo Rust build ready: dist-rust\nsl.exe
echo Rust update asset ready: dist-rust\nsl-rust-win64.zip
echo Place prod.keys beside nsl.exe or under %%USERPROFILE%%\.switch for deep scanning.
endlocal
