; Popyachsa AirPlay -- Windows installer (NSIS 3.x).
;
; Build with:
;   winget install NSIS.NSIS         (once)
;   .\make-dist.ps1                  (builds .\dist\PopyachsaAirPlay\)
;   makensis installer.nsi           (produces PopyachsaAirPlay-Setup.exe)

!include "MUI2.nsh"
!include "FileFunc.nsh"

!define PRODUCT_NAME         "Popyachsa AirPlay"
; Overridable from the command line: makensis /DPRODUCT_VERSION=0.2.9 installer.nsi
!ifndef PRODUCT_VERSION
  !define PRODUCT_VERSION    "0.2.9"
!endif
!define PRODUCT_PUBLISHER    "Recluse"
!define PRODUCT_URL          "https://github.com/Recluse"
!define EXE_NAME             "popyachsa-airplay.exe"
!define UNINSTALL_KEY        "Software\Microsoft\Windows\CurrentVersion\Uninstall\PopyachsaAirPlay"

Name                "${PRODUCT_NAME}"
OutFile             "PopyachsaAirPlay-Setup.exe"
InstallDir          "$PROGRAMFILES64\PopyachsaAirPlay"
InstallDirRegKey   HKLM "Software\PopyachsaAirPlay" "InstallDir"
RequestExecutionLevel admin
ShowInstDetails    show
ShowUninstDetails  show
SetCompressor      /SOLID lzma
BrandingText       "${PRODUCT_NAME} v${PRODUCT_VERSION}"

; --- UI ---------------------------------------------------------------------
!define MUI_ABORTWARNING
!define MUI_ICON   "icons\app.ico"
!define MUI_UNICON "icons\app.ico"
!define MUI_HEADERIMAGE
!define MUI_WELCOMEPAGE_TITLE "Welcome to ${PRODUCT_NAME}"
!define MUI_WELCOMEPAGE_TEXT \
    "${PRODUCT_NAME} is a low-latency AirPlay receiver for Windows.$\r$\n$\r$\nThis will install the tray manager, the patched UxPlay engine, the mDNS shim and the bundled GStreamer runtime into your Program Files."

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE_NAME}"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${PRODUCT_NAME} now"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; --- Sections ---------------------------------------------------------------
Section "${PRODUCT_NAME} (required)" SecCore
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; Bring everything from the make-dist.ps1 output into INSTDIR.
  File /r "dist\PopyachsaAirPlay\*.*"

  ; Per-user data dir is created by the app on first run at %APPDATA%.
  WriteRegStr HKLM "Software\PopyachsaAirPlay" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName"     "${PRODUCT_NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion"  "${PRODUCT_VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher"       "${PRODUCT_PUBLISHER}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "URLInfoAbout"    "${PRODUCT_URL}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon"     "$INSTDIR\${EXE_NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1

  ; Reported "size" in Apps & Features.
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "EstimatedSize" "$0"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" "$INSTDIR\uninstall.exe"
SectionEnd

Section "Start menu shortcut" SecStartMenu
  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortcut  "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk" \
                  "$INSTDIR\${EXE_NAME}" "" "$INSTDIR\${EXE_NAME}" 0
  CreateShortcut  "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall ${PRODUCT_NAME}.lnk" \
                  "$INSTDIR\uninstall.exe"
SectionEnd

Section "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${PRODUCT_NAME}.lnk" \
                 "$INSTDIR\${EXE_NAME}" "" "$INSTDIR\${EXE_NAME}" 0
SectionEnd

Section "Start with Windows" SecAutostart
  ; The tray app also manages the HKCU\...\Run entry from inside its Settings
  ; dialog ("Start with Windows" checkbox).  Here we just seed it.
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" \
                   "PopyachsaAirPlay" '"$INSTDIR\${EXE_NAME}"'
SectionEnd

Section "Windows Firewall rules" SecFirewall
  ; AirPlay listens on TCP 7000/7100 (RTSP + mirror) and UDP 5353 + 6000-7011 ranges.
  ExecWait 'netsh advfirewall firewall add rule name="Popyachsa AirPlay (TCP)" dir=in action=allow protocol=TCP localport=7000,7100 program="$INSTDIR\popyachsa-airplay.exe" enable=yes profile=any' $0
  ExecWait 'netsh advfirewall firewall add rule name="Popyachsa AirPlay (UDP mDNS)" dir=in action=allow protocol=UDP localport=5353 program="$INSTDIR\${EXE_NAME}" enable=yes profile=any' $0
SectionEnd

; --- Section descriptions ---------------------------------------------------
LangString DESC_SecCore      ${LANG_ENGLISH} "Tray manager, UxPlay engine, dnssd.dll shim, GStreamer runtime."
LangString DESC_SecStartMenu ${LANG_ENGLISH} "Place a shortcut in the Start menu."
LangString DESC_SecDesktop   ${LANG_ENGLISH} "Place a shortcut on the desktop."
LangString DESC_SecAutostart ${LANG_ENGLISH} "Launch ${PRODUCT_NAME} when Windows starts."
LangString DESC_SecFirewall  ${LANG_ENGLISH} "Open inbound TCP 7000/7100 and UDP 5353 for AirPlay discovery + mirror."

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore}      $(DESC_SecCore)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStartMenu} $(DESC_SecStartMenu)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop}   $(DESC_SecDesktop)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAutostart} $(DESC_SecAutostart)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecFirewall}  $(DESC_SecFirewall)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; --- Uninstaller ------------------------------------------------------------
Section "Uninstall"
  ; Kill any running instances so we can remove the files.
  ExecWait 'taskkill /F /IM "${EXE_NAME}"' $0
  ExecWait 'taskkill /F /IM "popyachsa-airplay.exe"'  $0

  ; Remove firewall rules + autostart.
  ExecWait 'netsh advfirewall firewall delete rule name="Popyachsa AirPlay (TCP)"'      $0
  ExecWait 'netsh advfirewall firewall delete rule name="Popyachsa AirPlay (UDP mDNS)"' $0
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "PopyachsaAirPlay"

  Delete   "$DESKTOP\${PRODUCT_NAME}.lnk"
  RMDir /r "$SMPROGRAMS\${PRODUCT_NAME}"

  ; Wipe the install dir.
  RMDir /r "$INSTDIR"

  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "Software\PopyachsaAirPlay"
SectionEnd
