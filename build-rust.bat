@echo off
setlocal
cd /d "%~dp0"

echo Building Nintendo Switch Library (Rust)...
cargo build --release
if errorlevel 1 exit /b %errorlevel%

if not exist "dist-rust" mkdir "dist-rust"
copy /Y "target\release\nsl.exe" "dist-rust\nsl.exe" >nul
if errorlevel 1 exit /b %errorlevel%
if not exist "dist-rust\bin" mkdir "dist-rust\bin"
copy /Y "bin\nstool.exe" "dist-rust\bin\nstool.exe" >nul
copy /Y "bin\hactool.exe" "dist-rust\bin\hactool.exe" >nul
if not exist "dist-rust\titledb" mkdir "dist-rust\titledb"
xcopy /E /I /Y "titledb\*.json" "dist-rust\titledb\" >nul

powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0package-rust.ps1"
if errorlevel 1 exit /b %errorlevel%

echo Rust build ready: dist-rust\nsl.exe
echo Rust update asset ready: dist-rust\nsl-rust-win64.zip
echo Place prod.keys beside nsl.exe or under %%USERPROFILE%%\.switch for deep scanning.
endlocal
