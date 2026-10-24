#Requires -Version 7.2
[CmdletBinding()]
param([string]$AppRoot = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This launcher is for Windows only.' }
$AppRoot = (Resolve-Path -LiteralPath $AppRoot).Path
$desktop = Join-Path $AppRoot 'WebCodex.exe'
$node = Join-Path $AppRoot 'node/node.exe'
foreach ($file in @($desktop, $node)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing portable application file: $file" }
}
$saved = @{}
foreach ($key in @('PATH', 'WEBCODEX_NODE', 'WEBCODEX_CODEBUDDY', 'WEBCODEX_CODEX', 'WEBCODEX_TUNNEL_CLIENT_BIN')) {
    $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
try {
    $env:PATH = (Split-Path -Parent $node) + ';' + $env:PATH
    $env:WEBCODEX_NODE = $node
    $cliPaths = @{
        WEBCODEX_CODEBUDDY = 'cli/codebuddy/codebuddy.exe'
        WEBCODEX_CODEX = 'cli/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe'
    }
    foreach ($key in $cliPaths.Keys) {
        $exe = Join-Path $AppRoot $cliPaths[$key]
        if (Test-Path -LiteralPath $exe -PathType Leaf) {
            [Environment]::SetEnvironmentVariable($key, $exe, 'Process')
            $env:PATH = (Split-Path -Parent $exe) + ';' + $env:PATH
        }
    }
    $agy = Join-Path $AppRoot 'cli/agy/agy.exe'
    if (Test-Path -LiteralPath $agy -PathType Leaf) { $env:PATH = (Split-Path -Parent $agy) + ';' + $env:PATH }
    $tunnel = Join-Path $AppRoot 'tunnel/tunnel-client.exe'
    if (Test-Path -LiteralPath $tunnel -PathType Leaf) { $env:WEBCODEX_TUNNEL_CLIENT_BIN = $tunnel }
    # Only the child process inherits these paths; no registry or machine PATH change.
    Start-Process -FilePath $desktop -WorkingDirectory $AppRoot -WindowStyle Hidden
} finally {
    foreach ($key in $saved.Keys) {
        if ($null -eq $saved[$key]) {
            Remove-Item -LiteralPath ("Env:" + $key) -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process')
        }
    }
}
