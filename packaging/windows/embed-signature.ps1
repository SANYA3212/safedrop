# Встраивает подпись обновления (.sig от `cargo tauri signer sign`) в конец
# установщика SafeDrop и удаляет .sig. Формат хвоста — как в src-tauri/src/updsig.rs:
#   [подпись: base64-текст из .sig][u32 LE длина подписи]["SDSIGv1\0"]
# Подпись покрывает установщик без хвоста; NSIS лишние байты в конце не читает.
param([Parameter(Mandatory = $true)][string]$Installer)
$ErrorActionPreference = 'Stop'

$Installer = (Resolve-Path -LiteralPath $Installer).Path
$sigPath = "$Installer.sig"
$magic = [byte[]]([Text.Encoding]::ASCII.GetBytes("SDSIGv1") + [byte]0)

$data = [IO.File]::ReadAllBytes($Installer)
# повторный запуск дописал бы второй хвост поверх первого
if ($data.Length -ge 8 -and [Convert]::ToBase64String($data, $data.Length - 8, 8) -eq [Convert]::ToBase64String($magic)) {
  throw "signature is already embedded: $Installer"
}

$sig = [Text.Encoding]::ASCII.GetBytes(([IO.File]::ReadAllText($sigPath)).Trim())
if ($sig.Length -eq 0 -or $sig.Length -gt 4096) { throw "unexpected signature size: $($sig.Length)" }

# сначала во временный файл, потом замена: при сбое установщик и .sig остаются как были
$tmp = "$Installer.tmp"
$fs = [IO.File]::Create($tmp)
try {
  $fs.Write($data, 0, $data.Length)
  $fs.Write($sig, 0, $sig.Length)
  $fs.Write([BitConverter]::GetBytes([uint32]$sig.Length), 0, 4)
  $fs.Write($magic, 0, $magic.Length)
} finally { $fs.Close() }
if ((Get-Item -LiteralPath $tmp).Length -ne $data.Length + $sig.Length + 12) {
  Remove-Item -LiteralPath $tmp
  throw "embedding failed: wrong output size"
}
Move-Item -LiteralPath $tmp -Destination $Installer -Force
Remove-Item -LiteralPath $sigPath
