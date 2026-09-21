; Per-user installer for catguard. No administrator rights, no UAC prompt.
; Build from the repo root:  makensis -DVERSION=0.3.0 installer/catguard.nsi

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!define EXE "..\target\x86_64-pc-windows-msvc\release\catguard.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\catguard"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "catguard"
OutFile "..\target\catguard-setup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\catguard"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "catguard"
VIAddVersionKey "CompanyName" "WEBSEED OÜ"
VIAddVersionKey "LegalCopyright" "© 2026 WEBSEED OÜ. MIT License."
VIAddVersionKey "FileDescription" "catguard setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!define MUI_ICON "..\assets\catguard.ico"
!define MUI_UNICON "..\assets\catguard.ico"
!define MUI_WELCOMEPAGE_TEXT "catguard locks the keyboard when a cat steps on it.$\r$\n$\r$\nSetup installs it for your Windows account only, adds it to the Start menu, and starts it in the tray when you sign in. You can switch that off in the app.$\r$\n$\r$\nNo administrator rights are needed."
!define MUI_FINISHPAGE_RUN "$INSTDIR\catguard.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start catguard"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\LICENSE"
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "catguard"
  ; A running catguard holds its exe open.
  nsExec::Exec 'taskkill /IM catguard.exe /F'
  Sleep 500

  SetOutPath "$INSTDIR"
  File "${EXE}"
  File "..\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\catguard.lnk" "$INSTDIR\catguard.exe"

  ; The same value the app's "Start with Windows" switch writes.
  WriteRegStr HKCU "${RUN_KEY}" "catguard" '"$INSTDIR\catguard.exe" --background'

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "catguard"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "WEBSEED OÜ"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\catguard.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" 1100
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM catguard.exe /F'
  Sleep 500
  Delete "$INSTDIR\catguard.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\catguard.lnk"
  DeleteRegValue HKCU "${RUN_KEY}" "catguard"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; Settings and the WebView2 profile.
  RMDir /r "$APPDATA\catguard"
  RMDir /r "$LOCALAPPDATA\catguard"
SectionEnd
