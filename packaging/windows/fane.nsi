; Fane installer for Windows (per-user, no admin rights needed).
;   makensis /DVERSION=0.2.0 packaging\windows\fane.nsi
; Expects target\release\fane.exe and packaging\windows\fane.ico (from `fane render-icon`).

Unicode true
!ifndef VERSION
  !define VERSION "0.0.0"
!endif

Name "Fane"
OutFile "..\..\dist\Fane-${VERSION}-windows-x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Fane"
RequestExecutionLevel user
Icon "fane.ico"
UninstallIcon "fane.ico"
SetCompressor /SOLID lzma

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fane"

Section "Fane"
  SetOutPath "$INSTDIR"
  File "..\..\target\release\fane.exe"
  File "fane.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Fane.lnk" "$INSTDIR\fane.exe" "run" "$INSTDIR\fane.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "Fane"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Alex Moran"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\fane.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  ExecWait '"$INSTDIR\fane.exe" quit'
  Delete "$SMPROGRAMS\Fane.lnk"
  Delete "$INSTDIR\fane.exe"
  Delete "$INSTDIR\fane.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  ; Autostart entry written by `fane autostart on`.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Fane"
  DeleteRegKey HKCU "${UNINST_KEY}"
SectionEnd
