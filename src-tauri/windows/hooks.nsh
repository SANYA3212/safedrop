; Хуки установщика SafeDrop (Tauri NSIS, bundle.windows.nsis.installerHooks).

; Перед удалением: убрать пункт «Отправить в SafeDrop» из основного меню Windows 11
; (пакет SafeDrop.ExplorerMenu, см. src-tauri/shellpkg). Регистрация текущего
; пользователя снимается без прав администратора; если пакета нет — ничего не делает.
!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -Command "Get-AppxPackage SafeDrop.ExplorerMenu | Remove-AppxPackage -ErrorAction SilentlyContinue"'
  Pop $0
!macroend
