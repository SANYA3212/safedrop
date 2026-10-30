@echo off
REM SafeDrop dev launcher
set PATH=%USERPROFILE%\.cargo\bin;%PATH%
cd /d "%~dp0src-tauri"
cargo tauri dev
pause
