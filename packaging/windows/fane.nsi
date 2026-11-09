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

Page components
Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fane"

Section "Fane" SecFane
  SectionIn RO
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

; Run after the files are in place: Fane starts and gets llama.cpp and Gemma 4 26B (using
; Ollama's copy if it has one), with a progress card. The optional extras come first so they
; are part of the same setup.
Section /o "Coding model: Qwen 3.8 27B (about 19 GB)" SecCode
  WriteINIStr "$INSTDIR\setup.ini" "want" "code" "1"
SectionEnd

Section /o "Image generation: Z-Image Turbo (about 11 GB)" SecImages
  WriteINIStr "$INSTDIR\setup.ini" "want" "images" "1"
SectionEnd

Section /o "Video generation: Wan 2.2 (about 13 GB)" SecVideo
  WriteINIStr "$INSTDIR\setup.ini" "want" "video" "1"
SectionEnd

Section "Get llama.cpp and Gemma 4 (about 16 GB)" SecSetup
  ReadINIStr $0 "$INSTDIR\setup.ini" "want" "code"
  ReadINIStr $1 "$INSTDIR\setup.ini" "want" "images"
  ReadINIStr $2 "$INSTDIR\setup.ini" "want" "video"
  StrCpy $3 ""
  StrCmp $0 "1" 0 +2
    StrCpy $3 "$3 --code"
  StrCmp $1 "1" 0 +2
    StrCpy $3 "$3 --images"
  StrCmp $2 "1" 0 +2
    StrCpy $3 "$3 --video"
  Delete "$INSTDIR\setup.ini"
  Exec '"$INSTDIR\fane.exe" run --setup$3'
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
  ; Background model server entry written by `fane service on`.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "FaneServe"
  DeleteRegKey HKCU "${UNINST_KEY}"
SectionEnd
