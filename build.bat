@echo off

echo 1. Build
echo 2. Clean build
set /p sel="Input: "

if "%sel%"=="2" (
    pyinstaller --clean -w -F --icon="assets/icon.ico" ^
     --add-data="assets;assets" ^
     --add-binary="bin/nstool.exe;bin" ^
     --add-binary="bin/hactool.exe;bin" ^
     nsl.py
) else (
    pyinstaller -w -F --icon="assets/icon.ico" ^
     --add-data="assets;assets" ^
     --add-binary="bin/nstool.exe;bin" ^
     --add-binary="bin/hactool.exe;bin" ^
     nsl.py
)

pause