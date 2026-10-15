@echo off
rem Launch Sheet Music Convertor.
rem Builds (or rebuilds) the release exe first. On a fresh clone this compiles everything,
rem which takes a few minutes; after that cargo only rebuilds what changed, e.g. after a git pull.
setlocal
cd /d "%~dp0"
set "EXE=%~dp0target\release\sheet-music-convertor.exe"

where cargo >nul 2>nul
if errorlevel 1 (
    if exist "%EXE%" goto run
    echo Rust is not installed, so the app can't be built.
    echo Install it from https://rustup.rs, then run this file again.
    pause
    exit /b 1
)

if not exist "%EXE%" echo Building Sheet Music Convertor for the first time. This takes a few minutes...
cargo build --release --bin sheet-music-convertor
if errorlevel 1 (
    echo.
    echo Build failed. See the errors above.
    pause
    exit /b 1
)

:run
start "" "%EXE%"
