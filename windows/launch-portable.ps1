#Requires -Version 7.2
[CmdletBinding()]
param([string]$AppRoot = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This launcher is for Windows only.' }
$AppRoot = (Resolve-Path -LiteralPath $AppRoot).Path
$desktop = Join-Path $AppRoot 'WebCodex.exe'
if (-not (Test-Path -LiteralPath $desktop -PathType Leaf)) { throw "Missing portable application file: $desktop" }
$node = Join-Path $AppRoot 'node/node.exe'
if (-not (Test-Path -LiteralPath $node -PathType Leaf)) {
    $nodeCommand = Get-Command node.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $nodeCommand) { throw 'Missing Node runtime: provide node/node.exe or install node.exe on PATH.' }
    $node = $nodeCommand.Source
}
$saved = @{}
foreach ($key in @('PATH', 'WEBCODEX_NODE', 'WEBCODEX_TUNNEL_CLIENT_BIN')) {
    $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
try {
    $env:PATH = (Split-Path -Parent $node) + ';' + $env:PATH
    $env:WEBCODEX_NODE = $node
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
