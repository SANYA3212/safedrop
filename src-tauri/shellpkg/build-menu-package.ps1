# Пересобрать SafeDropMenu.msix (пакет для пункта в основном меню Проводника Windows 11)
# из AppxManifest.xml. Нужен Windows SDK (makeappx.exe). Готовый .msix лежит в репозитории,
# потому что Tauri встраивает его в установщик как ресурс.
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Recurse -Filter makeappx.exe |
  Where-Object FullName -match '\\x64\\' | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $makeappx) { throw 'makeappx.exe не найден: поставь Windows SDK' }
$stage = Join-Path ([IO.Path]::GetTempPath()) "safedrop-menu-pkg"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $stage | Out-Null
Copy-Item (Join-Path $here 'AppxManifest.xml') $stage
Copy-Item (Join-Path $here '..\icons\128x128.png') (Join-Path $stage 'logo.png')
& $makeappx.FullName pack /o /d $stage /nv /p (Join-Path $here 'SafeDropMenu.msix')
if ($LASTEXITCODE -ne 0) { throw "makeappx завершился с кодом $LASTEXITCODE" }
Remove-Item $stage -Recurse -Force
