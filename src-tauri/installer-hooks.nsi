; installer-hooks.nsi — two-tier display-name hooks for the Volt Admin NSIS installer.
;
; Machine identifiers stay ASCII (productName="Volt Admin", mainBinaryName="VoltAdmin",
; identifier="ru.metalmon.volt-admin") — untouched, so INSTALLDIR, the .exe name, and the
; "...\Uninstall\Volt Admin" registry key PATH all stay Latin. Only the *displayed* strings
; (ARP DisplayName, Start-menu/Desktop shortcut labels) are rewritten to the Cyrillic
; human-facing name "Вольт Админ" via these hooks.
;
; Contract (adapted verbatim from the Thunderbolt fork's cross-platform display-name design):
;   - NSIS_HOOK_POSTINSTALL runs AFTER Tauri's default installer.nsi creates the ASCII
;     shortcuts, so it renames them in place (same target, new label) and deletes the
;     ASCII originals; it also overwrites the ARP DisplayName value (key path unchanged).
;   - NSIS_HOOK_PREUNINSTALL removes the renamed Cyrillic shortcuts (the stock uninstaller
;     only knows to delete "${PRODUCTNAME}.lnk", so the renamed ones would otherwise orphan).
;
; ENCODING (load-bearing): this file MUST be saved UTF-8-with-BOM (or UTF-16LE). Tauri's
; base installer.nsi declares `Unicode true`; without the BOM, makensis mis-decodes the
; Cyrillic literals below into mojibake in the built installer. Verify with a hexdump of
; the first 3 bytes: must be `ef bb bf`.
;
; Fragility: this mirrors Tauri's current shortcut/registry layout (PRODUCTNAME define,
; AppStartMenuFolder variable, shortcut paths, hook call sites). Re-verify these three
; contract points on any Tauri upgrade.

!include LogicLib.nsh

!macro NSIS_HOOK_POSTINSTALL
  ; "Программы и компоненты" (ARP) label — key path stays ...\Uninstall\Volt Admin
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}" "DisplayName" "Вольт Админ"
  ; Rename the Latin shortcuts Tauri just created -> Cyrillic label, same target
  ${If} ${FileExists} "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
    CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\Вольт Админ.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Delete "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
  ${EndIf}
  ${If} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
    CreateShortcut "$DESKTOP\Вольт Админ.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Delete "$DESKTOP\${PRODUCTNAME}.lnk"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; The stock uninstaller only deletes "${PRODUCTNAME}.lnk"; remove our renamed ones too
  Delete "$SMPROGRAMS\$AppStartMenuFolder\Вольт Админ.lnk"
  Delete "$DESKTOP\Вольт Админ.lnk"
!macroend
