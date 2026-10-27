@echo off
setlocal DisableDelayedExpansion
if not exist "%~dp0WebCodex.exe" (
  echo Missing portable application: WebCodex.exe
  exit /b 1
)
set "WEBCODEX_NODE="
if exist "%~dp0node\node.exe" set "WEBCODEX_NODE=%~dp0node\node.exe"
if not defined WEBCODEX_NODE for /f "delims=" %%I in ('"%SystemRoot%\System32\where.exe" $PATH:node.exe 2^>nul') do if not defined WEBCODEX_NODE set "WEBCODEX_NODE=%%I"
if not defined WEBCODEX_NODE (
  echo Missing Node runtime: provide node\node.exe or install node.exe on PATH.
  exit /b 1
)
for %%I in ("%WEBCODEX_NODE%") do set "PATH=%%~dpI;%PATH%"
if exist "%~dp0tunnel\tunnel-client.exe" set "WEBCODEX_TUNNEL_CLIENT_BIN=%~dp0tunnel\tunnel-client.exe"
start "" /D "%~dp0" "%~dp0WebCodex.exe"
endlocal
