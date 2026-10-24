; -*- coding: utf-8 -*-
Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "WinVer.nsh"
!include "x64.nsh"

!define BRAND "WebCodex Desktop（工具增强版）"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\MyCodexDesktopTools"
!define OWNER_ID "MyCodexDesktopTools-0.4.1-local-build-v1"
!define WEBVIEW_KEY "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!ifndef BOOTSTRAPPER
!error "Pass /DBOOTSTRAPPER=<official MicrosoftEdgeWebview2Setup.exe path>"
!endif
!ifndef PAYLOAD_DIR
!define PAYLOAD_DIR "${__FILEDIR__}\payload"
!endif
!ifndef GENERATED_DIR
!define GENERATED_DIR "${__FILEDIR__}"
!endif
!ifndef APP_ICON
!define APP_ICON "${__FILEDIR__}\..\..\apps\desktop\src-tauri\icons\icon.ico"
!endif
!ifndef OUTPUT_FILE
!ifdef FAST_SMOKE
!define OUTPUT_FILE "${__FILEDIR__}\installer-fast-smoke.exe"
!else
!define OUTPUT_FILE "${__FILEDIR__}\WebCodex-Setup-0.4.1-MyTools.exe"
!endif
!endif

Name "${BRAND}"
Caption "${BRAND} 安装"
!ifdef FAST_SMOKE
SetCompress off
!else
SetCompressor /SOLID lzma
SetCompressorDictSize 64
!endif
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\MyCodex\WebCodex Desktop"
RequestExecutionLevel user
ManifestDPIAware true
ShowInstDetails show
ShowUninstDetails show
VIProductVersion "0.4.1.0"
VIAddVersionKey /LANG=2052 "ProductName" "${BRAND}"
VIAddVersionKey /LANG=2052 "FileDescription" "WebCodex 本地定制安装程序（含办公工具）"
VIAddVersionKey /LANG=2052 "FileVersion" "0.4.1"
VIAddVersionKey /LANG=2052 "LegalCopyright" "WebCodex contributors; see LICENSE"
BrandingText "WebCodex · 本地定制构建"

!define MUI_ICON "${APP_ICON}"
!define MUI_UNICON "${APP_ICON}"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "安装 WebCodex Desktop 工具增强版"
!define MUI_WELCOMEPAGE_TEXT "安装桌面程序、专用 Python 和 Word、Excel、PPT、PDF 工具。$\r$\n$\r$\n已有 WebCodex 正在运行时，请先从托盘退出。安装后，在桌面程序中选择工作项目，并填写您自己账号的 Tunnel ID 和 API Key。$\r$\n$\r$\n如缺少 Microsoft WebView2，安装程序将联网补装。"
!define MUI_FINISHPAGE_RUN "$INSTDIR\WebCodex.exe"
!define MUI_FINISHPAGE_RUN_TEXT "启动 WebCodex Desktop"
!define MUI_FINISHPAGE_TEXT "安装完成。请在桌面程序中选择项目，并配置您自己的 Tunnel ID 和 API Key。"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${PAYLOAD_DIR}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Var CheckPath
Var CheckAttributes
Var CheckParent
Var WebViewVersion
Var WebViewReady
Var OwnedLocation
Var MarkerContents
Var FileHandle

!macro Fail MESSAGE
  DetailPrint "${MESSAGE}"
!ifdef FAST_SMOKE
  FileOpen $9 "$TEMP\WebCodex-installer-debug-errors.txt" a
  FileSeek $9 0 END
  FileWrite $9 "${MESSAGE}$\r$\n"
  FileClose $9
!endif
  IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "${MESSAGE}"
  SetErrorLevel 2
  Quit
!macroend

!macro FailIfErrors MESSAGE
  ${If} ${Errors}
    !insertmacro Fail "${MESSAGE}"
  ${EndIf}
!macroend

!include "${GENERATED_DIR}\payload.nsh"

Function CheckFreshTarget
!ifdef FAST_SMOKE
  FileOpen $9 "$TEMP\WebCodex-installer-debug-errors.txt" a
  FileSeek $9 0 END
  FileWrite $9 "TARGET=$INSTDIR$\r$\n"
  FileClose $9
!endif
  ; Reject any existing installation path, registration, or shortcut: no upgrade/overwrite.
  SetRegView 64
  StrCpy $1 0
  ${Do}
    ClearErrors
    EnumRegKey $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall" $1
    ${If} ${Errors}
      ${Break}
    ${EndIf}
    ${If} $0 == ""
      ${Break}
    ${EndIf}
    ${If} $0 == "MyCodexDesktopTools"
      !insertmacro Fail "工具增强版已安装。请先从 Windows 设置中卸载；原有配置和作品会保留。"
    ${EndIf}
    IntOp $1 $1 + 1
  ${Loop}
  ${If} ${FileExists} "$DESKTOP\${BRAND}.lnk"
    !insertmacro Fail "桌面上已有同名快捷方式；请先检查并移走该快捷方式。"
  ${EndIf}
  ${If} ${FileExists} "$SMPROGRAMS\${BRAND}"
    !insertmacro Fail "开始菜单中已有同名目录；请先检查已有安装。"
  ${EndIf}
  StrCpy $0 $INSTDIR 2 1
!ifdef FAST_SMOKE
  FileOpen $9 "$TEMP\WebCodex-installer-debug-errors.txt" a
  FileSeek $9 0 END
  FileWrite $9 "DRIVE=$0$\r$\n"
  FileClose $9
!endif
  ${If} $0 != ":\"
    !insertmacro Fail "请选择本机磁盘中的完整安装目录，不支持网络或相对路径。"
  ${EndIf}
  StrLen $0 $INSTDIR
  ${If} $0 < 4
    !insertmacro Fail "不能安装到磁盘根目录，请选择新的子目录。"
  ${EndIf}
  ; Win32 resolves a not-yet-created directory without requiring it to exist.
  System::Call 'kernel32::GetFullPathNameW(w "$INSTDIR", i ${NSIS_MAX_STRLEN}, w .r0, p 0) i.r1'
  ${If} $1 <= 0
  ${OrIf} $1 >= ${NSIS_MAX_STRLEN}
    !insertmacro Fail "无法解析安装目录，请选择较短的本地路径。"
  ${EndIf}
  StrCpy $CheckPath $0
  StrCpy $INSTDIR $CheckPath
  System::Call 'kernel32::GetFileAttributesW(w "$CheckPath") i.s'
  Pop $CheckAttributes
  ${If} $CheckAttributes != -1
    !insertmacro Fail "安装目录已经存在。为保护原有文件，请选择一个不存在的新目录。"
  ${EndIf}
  Call CheckNoReparse
FunctionEnd

!macro DefineNoReparse PREFIX
Function ${PREFIX}CheckNoReparse
  ; Check every existing ancestor; never follow a junction or symbolic link.
  ${Do}
    System::Call 'kernel32::GetFileAttributesW(w "$CheckPath") i.s'
    Pop $CheckAttributes
    ${If} $CheckAttributes != -1
      IntOp $CheckAttributes $CheckAttributes & 0x400
      ${If} $CheckAttributes != 0
        !insertmacro Fail "目录包含符号链接或目录联接，无法继续。请使用普通本地目录。"
      ${EndIf}
    ${EndIf}
    ${GetParent} "$CheckPath" $CheckParent
    ${If} $CheckParent == $CheckPath
      ${Break}
    ${EndIf}
    ${If} $CheckParent == ""
      ${Break}
    ${EndIf}
    StrLen $0 $CheckParent
    ${If} $0 == 2
      StrCpy $CheckParent "$CheckParent\"
    ${EndIf}
    StrCpy $CheckPath $CheckParent
  ${Loop}
FunctionEnd
!macroend
!insertmacro DefineNoReparse ""
!insertmacro DefineNoReparse "un."

Function .onInit
  SetShellVarContext current
  ${IfNot} ${AtLeastWin10}
    !insertmacro Fail "此安装包需要 Windows 10 或 Windows 11。"
  ${EndIf}
  ${IfNot} ${IsNativeAMD64}
    !insertmacro Fail "此安装包适用于 x64 电脑。"
  ${EndIf}
FunctionEnd

Function DetectWebView
  StrCpy $WebViewReady "0"
  SetRegView 32
  ReadRegStr $WebViewVersion HKLM "${WEBVIEW_KEY}" "pv"
  Call CheckWebViewVersion
  ReadRegStr $WebViewVersion HKCU "${WEBVIEW_KEY}" "pv"
  Call CheckWebViewVersion
  SetRegView 64
  ReadRegStr $WebViewVersion HKCU "${WEBVIEW_KEY}" "pv"
  Call CheckWebViewVersion
  ClearErrors
FunctionEnd

Function CheckWebViewVersion
  ${If} $WebViewVersion != ""
  ${AndIf} $WebViewVersion != "0.0.0.0"
    StrCpy $WebViewReady "1"
  ${EndIf}
FunctionEnd

Section "WebCodex Desktop 与办公工具" Main
  Call CheckFreshTarget
  Call DetectWebView
  ${If} $WebViewReady != "1"
    DetailPrint "正在安装 Microsoft WebView2，请保持网络连接……"
    InitPluginsDir
    SetOutPath "$PLUGINSDIR"
    File /oname=MicrosoftEdgeWebview2Setup.exe "${BOOTSTRAPPER}"
    !insertmacro FailIfErrors "无法准备 WebView2 安装组件。"
    ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" /silent /install' $0
    Call DetectWebView
    ${If} $WebViewReady != "1"
      !insertmacro Fail "WebView2 未能安装。请检查网络，安装 Microsoft WebView2 Runtime 后重新运行本安装程序。"
    ${EndIf}
  ${EndIf}
  ; Check again after any long-running prerequisite operation.
  Call CheckFreshTarget
  ClearErrors
  CreateDirectory "$INSTDIR"
  !insertmacro FailIfErrors "无法创建安装目录。请检查目录权限及可用磁盘空间。"
  SetOutPath "$INSTDIR"
  FileOpen $0 "$INSTDIR\.mycodex-install-owner" w
  !insertmacro FailIfErrors "无法写入安装记录。"
  FileWrite $0 "${OWNER_ID}"
  FileClose $0
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  !insertmacro FailIfErrors "无法创建卸载程序。"
  ; Register the uninstaller before copying the large payload so interrupted
  ; installs can be removed through Windows Settings instead of manual cleanup.
  SetRegView 64
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${BRAND}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "0.4.1"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "WebCodex 本地定制构建"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\WebCodex.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  !insertmacro FailIfErrors "无法注册卸载信息。"
  !insertmacro InstallPayload
  SetOutPath "$INSTDIR"
  CreateDirectory "$SMPROGRAMS\${BRAND}"
  CreateShortcut "$SMPROGRAMS\${BRAND}\WebCodex.lnk" "$INSTDIR\WebCodex.exe"
  CreateShortcut "$SMPROGRAMS\${BRAND}\卸载.lnk" "$INSTDIR\Uninstall.exe"
  CreateShortcut "$DESKTOP\${BRAND}.lnk" "$INSTDIR\WebCodex.exe"
  !insertmacro FailIfErrors "无法创建快捷方式。"
  SetErrorLevel 0
SectionEnd

Function un.onInit
  SetShellVarContext current
  SetRegView 64
  ReadRegStr $OwnedLocation HKCU "${UNINSTALL_KEY}" "InstallLocation"
  ${If} $OwnedLocation != $INSTDIR
    !insertmacro Fail "安装位置与记录不一致，已停止卸载以保护其他文件。"
  ${EndIf}
  StrCpy $CheckPath $INSTDIR
  Call un.CheckNoReparse
  FileOpen $0 "$INSTDIR\.mycodex-install-owner" r
  !insertmacro FailIfErrors "缺少有效安装记录，已停止卸载。"
  FileRead $0 $MarkerContents
  FileClose $0
  ${If} $MarkerContents != "${OWNER_ID}"
    !insertmacro Fail "安装记录不匹配，已停止卸载。"
  ${EndIf}
FunctionEnd

Function un.CheckFileAvailable
  Pop $1
  StrCpy $CheckPath $1
  Call un.CheckNoReparse
  System::Call 'kernel32::GetFileAttributesW(w r1) i .r0'
  ${If} $0 != -1
    ; Exclusive read check detects files still opened by the application.
    System::Call 'kernel32::CreateFileW(w r1, i 0x80000000, i 0, p 0, i 3, i 0, p 0) p.s'
    Pop $FileHandle
    ${If} $FileHandle == -1
      !insertmacro Fail "程序文件仍被占用或无法读取。请完全退出 WebCodex（包括托盘）后重试卸载。"
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p $FileHandle)'
  ${EndIf}
FunctionEnd

Section "Uninstall"
  !insertmacro CheckUninstallPayload
  ClearErrors
  !insertmacro DeletePayload
  Delete "$SMPROGRAMS\${BRAND}\WebCodex.lnk"
  Delete "$SMPROGRAMS\${BRAND}\卸载.lnk"
  RMDir "$SMPROGRAMS\${BRAND}"
  Delete "$DESKTOP\${BRAND}.lnk"
  Delete "$INSTDIR\.mycodex-install-owner"
  Delete "$INSTDIR\Uninstall.exe"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; Never use RMDir /r: leave all files created after installation untouched.
  RMDir "$INSTDIR"
  ClearErrors
  SetErrorLevel 0
SectionEnd
