#Requires -Version 7.2
[CmdletBinding()]
param(
    [string]$ToolRoot = $PSScriptRoot,
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'These providers require Windows.' }
$ToolRoot = (Resolve-Path -LiteralPath $ToolRoot).Path
$node = (Get-Command node.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
$npm = (Get-Command npm.cmd -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
Get-Command $Python -ErrorAction Stop | Out-Null
foreach ($relative in @('chat-handoff/requirements.lock',
    'chat-handoff/extension-runtime/package-lock.json', 'goal-provider/package-lock.json',
    'chat-handoff/edge-handoff.provider.example.toml',
    'chat-agents/chat-agents.provider.example.toml', 'goal-provider/codex.acp.example.toml')) {
    if (-not (Test-Path -LiteralPath (Join-Path $ToolRoot $relative) -PathType Leaf)) {
        throw "Missing provider source: $relative"
    }
}
function Invoke-SetupCommand([string]$Program, [string[]]$ArgumentList) {
    & $Program @ArgumentList
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
Invoke-SetupCommand $Python @('-c', 'import sys; assert sys.version_info[:2] == (3, 11), "Python 3.11 required"')
$venv = Join-Path $ToolRoot '.venv'
$venvPython = Join-Path $venv 'Scripts/python.exe'
if (-not (Test-Path -LiteralPath $venvPython -PathType Leaf)) {
    if (Test-Path -LiteralPath $venv) { throw 'Existing .venv has no Python; use a complete environment or a fresh tool directory.' }
    Invoke-SetupCommand $Python @('-m', 'venv', $venv)
}
Invoke-SetupCommand $venvPython @('-c', 'import sys; assert sys.version_info[:2] == (3, 11), "Python 3.11 required"')
Invoke-SetupCommand $venvPython @('-m', 'pip', 'install', '-r', (Join-Path $ToolRoot 'chat-handoff/requirements.lock'))
Invoke-SetupCommand $venvPython @('-m', 'pip', 'check')
foreach ($relative in @('chat-handoff/extension-runtime', 'goal-provider')) {
    Invoke-SetupCommand $npm @('ci', '--prefix', (Join-Path $ToolRoot $relative))
}
$output = Join-Path $ToolRoot 'provider-config'
New-Item -ItemType Directory -Path $output -Force | Out-Null
foreach ($relative in @('chat-handoff/edge-handoff.provider.example.toml',
    'chat-agents/chat-agents.provider.example.toml', 'goal-provider/codex.acp.example.toml')) {
    $template = Get-Content -LiteralPath (Join-Path $ToolRoot $relative) -Raw
    $rendered = [regex]::Replace($template, "'C:/WebCodex/windows([^']*)'", {
        param($match)
        $path = $ToolRoot.Replace('\', '/') + $match.Groups[1].Value
        ConvertTo-Json -InputObject $path -Compress
    })
    $rendered = $rendered.Replace("'C:/Program Files/nodejs/node.exe'",
        (ConvertTo-Json -InputObject $node.Replace('\', '/') -Compress))
    $name = [IO.Path]::GetFileName($relative).Replace('.example.toml', '.local.toml')
    Set-Content -LiteralPath (Join-Path $output $name) -Value $rendered -Encoding utf8NoBOM
}
Write-Output "Dependencies installed. Review generated templates in: $output"
Write-Output 'Runner configuration, credentials, Desktop, browser, and services were not changed or started.'
