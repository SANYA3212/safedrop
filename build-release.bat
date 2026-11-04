@echo off
setlocal
set "ROOT=%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

cd /d "%ROOT%"
for /f "usebackq delims=" %%V in (`powershell -NoProfile -Command "(Get-Content 'src-tauri\tauri.conf.json' -Raw | ConvertFrom-Json).version"`) do set "VERSION=%%V"
if not defined VERSION (
  echo ERROR: cannot read version from src-tauri\tauri.conf.json
  pause
  exit /b 1
)

REM Build the Explorer COM extension first: Tauri embeds it into the installer.
cargo build --release --manifest-path "%ROOT%src-shell\Cargo.toml"
if errorlevel 1 goto :failed
if not exist "%ROOT%src-shell\target\release\safedrop_shell.dll" (
  echo ERROR: safedrop_shell.dll was not produced
  goto :failed
)

cd /d "%ROOT%src-tauri"
REM Remove a stale loose copy from older builds to avoid duplicate MSI components.
if exist "%ROOT%src-tauri\target\release\safedrop-shell.dll" del /q "%ROOT%src-tauri\target\release\safedrop-shell.dll"
cargo tauri build
if errorlevel 1 goto :failed

cd /d "%ROOT%"
if not exist "dist" mkdir "dist"
copy /y "src-tauri\target\release\safedrop.exe" "dist\safedrop.exe" >nul
if errorlevel 1 goto :failed
copy /y "src-shell\target\release\safedrop_shell.dll" "dist\safedrop-shell.dll" >nul
if errorlevel 1 goto :failed
if exist "src-tauri\target\release\WebView2Loader.dll" copy /y "src-tauri\target\release\WebView2Loader.dll" "dist\" >nul
if not exist "dist\WebView2Loader.dll" if exist "src-tauri\target\debug\WebView2Loader.dll" copy /y "src-tauri\target\debug\WebView2Loader.dll" "dist\" >nul
echo %VERSION%> "dist\version.txt"

if not exist "update" mkdir "update"
set "INSTALLER="
set "INSTALLER=%ROOT%src-tauri\target\release\bundle\nsis\SafeDrop_%VERSION%_x64-setup.exe"
if not exist "%INSTALLER%" (
  echo ERROR: NSIS installer not found: %INSTALLER%
  goto :failed
)
for %%F in ("%INSTALLER%") do set "INSTALLER_NAME=%%~nxF"
copy /y "%INSTALLER%" "update\%INSTALLER_NAME%" >nul
if errorlevel 1 goto :failed
echo %VERSION%> "update\version.txt"

REM Sign the installer: since 0.3.31 friends only install updates signed with this key.
REM The private key stays on this PC (never commit it); the public key is in src-tauri\src\updsig.rs.
set "SIGN_KEY=%USERPROFILE%\.tauri\safedrop-update.key"
if not exist "%SIGN_KEY%" (
  echo ERROR: signing key not found: %SIGN_KEY%
  goto :failed
)
cargo tauri signer sign -f "%SIGN_KEY%" -p "" "update\%INSTALLER_NAME%" >nul
if errorlevel 1 goto :failed
if not exist "update\%INSTALLER_NAME%.sig" (
  echo ERROR: signature was not produced
  goto :failed
)

echo.
echo OK: dist\safedrop.exe, dist\safedrop-shell.dll, update\%INSTALLER_NAME% and its .sig
echo The installer embeds safedrop-shell.dll as a Tauri resource.
echo On other PCs: keep safedrop.exe and safedrop-shell.dll together in dist.
echo WebView2 Runtime and Radmin VPN may also be required.
echo Ports TCP: 47831 (files) and 47832 (chat/calls/profiles)
pause
exit /b 0

:failed
echo BUILD FAILED
pause
exit /b 1
