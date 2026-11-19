@echo off
setlocal
cd /d "%~dp0"

echo [NSL] Building release...
cargo build --release --locked
if errorlevel 1 exit /b %errorlevel%

echo [NSL] Verifying and packaging release...
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0scripts\package.ps1"
if errorlevel 1 exit /b %errorlevel%

echo [NSL] Executable: target\release\nsl.exe
echo [NSL] Package: dist\nsl-win64.zip
endlocal