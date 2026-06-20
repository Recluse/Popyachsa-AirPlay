; Popyachsa AirPlay — Windows installer (NSIS / Modern UI 2).
; Per-user install (no administrator rights). Bundles the full in-process
; engine distribution produced by make-dist.ps1 (dist\PopyachsaAirPlay\).
;
; Build:  makensis /DVERSION=0.2.1 installer\popyachsa-airplay.nsi
; (run from the project root, or adjust the relative paths below).

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.2.1"
!endif

!define APP        "Popyachsa AirPlay"
!define APPEXE     "popyachsa-airplay.exe"
!define COMPANY    "Recluse"
!define SITE       "https://airplay.popyachsa.com"
!define REGUNINST  "Software\Microsoft\Windows\CurrentVersion\Uninstall\PopyachsaAirPlay"
!define REGAPP     "Software\PopyachsaAirPlay"

Name "${APP}"
OutFile "..\dist\PopyachsaAirPlay-Setup.exe"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\PopyachsaAirPlay"
InstallDirRegKey HKCU "${REGAPP}" "InstallDir"
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUnInstDetails show
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName"     "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion"     "${VERSION}"
VIAddVersionKey "LegalCopyright"  "GPL-3.0 — ${COMPANY}"
VIAddVersionKey "CompanyName"     "${COMPANY}"

; --- UI / branding ---
!define MUI_ICON   "..\icons\app.ico"
!define MUI_UNICON "..\icons\app.ico"
!define MUI_ABORTWARNING
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "art\header.bmp"
!define MUI_HEADERIMAGE_RIGHT
!define MUI_WELCOMEFINISHPAGE_BITMAP "art\welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "art\welcome.bmp"

; Finish page: offer to launch + visit site.
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APPEXE}"
!define MUI_FINISHPAGE_RUN_TEXT "$(RUN_TEXT)"
!define MUI_FINISHPAGE_LINK "$(LINK_TEXT)"
!define MUI_FINISHPAGE_LINK_LOCATION "${SITE}"

; --- pages ---
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE.txt"
!insertmacro MUI_PAGE_COMPONENTS
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE DirLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; --- languages (installer chrome auto-localizes by system locale) ---
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Russian"

LangString RUN_TEXT  ${LANG_ENGLISH} "Launch Popyachsa AirPlay"
LangString RUN_TEXT  ${LANG_RUSSIAN} "Запустить Popyachsa AirPlay"
LangString LINK_TEXT ${LANG_ENGLISH} "Visit airplay.popyachsa.com"
LangString LINK_TEXT ${LANG_RUSSIAN} "Открыть airplay.popyachsa.com"
LangString SEC_CORE  ${LANG_ENGLISH} "Popyachsa AirPlay (required)"
LangString SEC_CORE  ${LANG_RUSSIAN} "Popyachsa AirPlay (обязательно)"
LangString SEC_DESK  ${LANG_ENGLISH} "Desktop shortcut"
LangString SEC_DESK  ${LANG_RUSSIAN} "Ярлык на рабочем столе"
LangString DIR_PF    ${LANG_ENGLISH} "Please don't install into Program Files — the auto-updater runs without administrator rights and can't replace files there. Reverting to the default per-user location."
LangString DIR_PF    ${LANG_RUSSIAN} "Не устанавливай в Program Files — автообновление работает без прав администратора и не сможет заменять там файлы. Возвращаю папку по умолчанию (для пользователя)."

; Block Program Files installs (would break the per-user auto-updater).
Function DirLeave
  StrLen $0 "$PROGRAMFILES"
  StrCpy $1 "$INSTDIR" $0
  StrCmp $1 "$PROGRAMFILES" pf 0
  StrLen $0 "$PROGRAMFILES64"
  StrCpy $1 "$INSTDIR" $0
  StrCmp $1 "$PROGRAMFILES64" pf done
  pf:
    MessageBox MB_OK|MB_ICONEXCLAMATION "$(DIR_PF)"
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\PopyachsaAirPlay"
    Abort
  done:
FunctionEnd

; Close any running instance so files aren't locked.
!macro StopApp
  nsExec::Exec 'taskkill /F /IM ${APPEXE}'
  nsExec::Exec 'taskkill /F /IM updater.exe'
  Sleep 600
!macroend

Section "$(SEC_CORE)" SecCore
  SectionIn RO
  !insertmacro StopApp
  SetOutPath "$INSTDIR"
  ; Payload: the whole portable distribution built by make-dist.ps1.
  File /r "..\dist\PopyachsaAirPlay\*.*"

  ; Pre-build the GStreamer plugin registry into the per-user cache the app
  ; reads (GST_REGISTRY). Without this, the first launch scans 241 plugins
  ; before it starts advertising — so the receiver is invisible for several
  ; seconds. Doing it here (once, at install) makes the first launch instant.
  CreateDirectory "$APPDATA\PopyachsaAirPlay"
  System::Call 'kernel32::SetEnvironmentVariable(t "GST_REGISTRY", t "$APPDATA\PopyachsaAirPlay\gstreamer-registry.bin")'
  System::Call 'kernel32::SetEnvironmentVariable(t "GST_PLUGIN_SYSTEM_PATH", t "$INSTDIR\lib\gstreamer-1.0")'
  DetailPrint "Building media plugin cache (one-time)…"
  nsExec::ExecToLog '"$INSTDIR\gst-inspect-1.0.exe"'
  Pop $0

  ; Start Menu shortcut.
  CreateShortCut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\${APPEXE}" 0

  ; Uninstaller + Add/Remove Programs entry (per-user hive).
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr   HKCU "${REGAPP}"    "InstallDir"     "$INSTDIR"
  WriteRegStr   HKCU "${REGUNINST}" "DisplayName"     "${APP}"
  WriteRegStr   HKCU "${REGUNINST}" "DisplayVersion"  "${VERSION}"
  WriteRegStr   HKCU "${REGUNINST}" "Publisher"       "${COMPANY}"
  WriteRegStr   HKCU "${REGUNINST}" "DisplayIcon"     "$INSTDIR\${APPEXE}"
  WriteRegStr   HKCU "${REGUNINST}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr   HKCU "${REGUNINST}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegStr   HKCU "${REGUNINST}" "URLInfoAbout"    "${SITE}"
  WriteRegDWORD HKCU "${REGUNINST}" "NoModify" 1
  WriteRegDWORD HKCU "${REGUNINST}" "NoRepair" 1
  WriteRegDWORD HKCU "${REGUNINST}" "EstimatedSize" 237568
SectionEnd

Section "$(SEC_DESK)" SecDesktop
  CreateShortCut "$DESKTOP\${APP}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\${APPEXE}" 0
SectionEnd

Section "Uninstall"
  !insertmacro StopApp
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  ; Remove the app's own autostart entry (Run value name = PopyachsaAirPlay).
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "PopyachsaAirPlay"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKCU "${REGUNINST}"
  DeleteRegKey HKCU "${REGAPP}"
  ; User config + logs under %APPDATA%\PopyachsaAirPlay are intentionally kept.
SectionEnd
