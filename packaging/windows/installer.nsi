Unicode true
!include "MUI2.nsh"
!include "x64.nsh"
!include "WinVer.nsh"

Name "Lapui"
OutFile "${SETUP_FILE}"
InstallDir "$LOCALAPPDATA\Programs\Lapui"
InstallDirRegKey HKCU "Software\Lapui" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey "ProductName" "Lapui"
VIAddVersionKey "FileDescription" "Lapui desktop installer"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "LegalCopyright" "Lapui contributors"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\lapui.exe"
!define MUI_FINISHPAGE_RUN_NOTCHECKED
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "Lapui requires 64-bit Windows."
    Abort
  ${EndIf}
  ${IfNot} ${AtLeastWin10}
    MessageBox MB_ICONSTOP "Lapui requires Windows 10 or later."
    Abort
  ${EndIf}
  SetRegView 64
  SetShellVarContext current
FunctionEnd

Section "Lapui"
  SetOutPath "$INSTDIR"
  File /r "${PACKAGE_DIR}\*"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Lapui"
  CreateShortcut "$SMPROGRAMS\Lapui\Lapui.lnk" "$INSTDIR\lapui.exe"
  CreateShortcut "$SMPROGRAMS\Lapui\Uninstall.lnk" "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Lapui" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lapui" "DisplayName" "Lapui"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lapui" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lapui" "DisplayIcon" "$INSTDIR\lapui.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lapui" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lapui" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lapui" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetRegView 64
  SetShellVarContext current
  !include "${UNINSTALL_FILES}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Lapui\Lapui.lnk"
  Delete "$SMPROGRAMS\Lapui\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Lapui"
  DeleteRegKey HKCU "Software\Lapui"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lapui"
SectionEnd
