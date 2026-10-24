@echo off
setlocal
if not exist "%~dp0WebCodex.exe" (
  echo Missing portable application: WebCodex.exe
  exit /b 1
)
if not exist "%~dp0node\node.exe" (
  echo Missing portable Node runtime: node\node.exe
  exit /b 1
)
set "WEBCODEX_NODE=%~dp0node\node.exe"
set "PATH=%~dp0node;%PATH%"
if exist "%~dp0cli\codebuddy\codebuddy.exe" (
  set "WEBCODEX_CODEBUDDY=%~dp0cli\codebuddy\codebuddy.exe"
  set "PATH=%~dp0cli\codebuddy;%PATH%"
)
if exist "%~dp0cli\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe" (
  set "WEBCODEX_CODEX=%~dp0cli\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe"
  set "PATH=%~dp0cli\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin;%PATH%"
)
if exist "%~dp0cli\agy\agy.exe" set "PATH=%~dp0cli\agy;%PATH%"
if exist "%~dp0tunnel\tunnel-client.exe" set "WEBCODEX_TUNNEL_CLIENT_BIN=%~dp0tunnel\tunnel-client.exe"
start "" /D "%~dp0" "%~dp0WebCodex.exe"
endlocal
