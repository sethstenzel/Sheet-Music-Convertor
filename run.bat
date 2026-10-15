@echo off
rem Launch Sheet Music Convertor. Builds the release exe first if it doesn't exist yet.
setlocal
cd /d "%~dp0"
set "EXE=%~dp0target\release\sheet-music-convertor.exe"

if not exist "%EXE%" (
    echo Building Sheet Music Convertor ^(first run only, this takes a few minutes^)...
    cargo build --release --bin sheet-music-convertor
    if errorlevel 1 (
        echo.
        echo Build failed. Make sure Rust is installed: https://rustup.rs
        pause
        exit /b 1
    )
)

start "" "%EXE%"
