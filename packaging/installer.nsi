; den's installer: per user (no admin), into %LOCALAPPDATA%\Programs\den,
; with a Start menu shortcut and an uninstaller. Built by scripts/release.ts:
;
;   makensis /DVERSION=0.2.0 /DEXE=target\release\den.exe /DOUTFILE=... packaging\installer.nsi
;
; The in-app updater runs it with /S /UPDATE: silent, then den starts again.

Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !error "Pass /DVERSION=x.y.z"
!endif
!ifndef EXE
  !define EXE "..\target\release\den.exe"
!endif
!ifndef OUTFILE
  !define OUTFILE "..\target\release\den_${VERSION}_x64-setup.exe"
!endif

!define APP "den"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "Software\${APP}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "LegalCopyright" "Patrick Demichiel"

!define MUI_ICON "..\assets\icons\app.ico"
!define MUI_UNICON "..\assets\icons\app.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP}.exe"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"
  ; An update starts while the old den is closing: retry until its exe is free.
  StrCpy $0 0
  retry:
    ClearErrors
    File "/oname=${APP}.exe" "${EXE}"
    IfErrors 0 copied
    IntOp $0 $0 + 1
    IntCmp $0 50 copied
    Sleep 200
    Goto retry
  copied:
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${APP}.exe"
  WriteRegStr HKCU "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Patrick Demichiel"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${APP}.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $1 $2 $3
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $1
SectionEnd

Function .onInstSuccess
  ; From the updater: start the new version.
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/UPDATE" $1
  IfErrors done
  Exec '"$INSTDIR\${APP}.exe"'
  done:
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\${APP}.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\${APP}"
  ; Settings and sessions in %APPDATA%\den stay.
SectionEnd
