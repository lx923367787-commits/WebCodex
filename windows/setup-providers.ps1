#Requires -Version 7.2
[CmdletBinding()]
param(
    [string]$ToolRoot = $PSScriptRoot,
    [string]$Python = 'python',
    [string]$Wheelhouse,
    [switch]$Offline,
    [switch]$IncludeGoalProvider,
    [string]$ProjectRoot
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'These providers require Windows.' }
$ToolRoot = (Resolve-Path -LiteralPath $ToolRoot).Path
$node = Join-Path $ToolRoot 'node/node.exe'
if (-not (Test-Path -LiteralPath $node -PathType Leaf)) {
    $node = (Get-Command node.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}
if ($Offline) {
    if (-not $Wheelhouse) { throw '-Offline requires -Wheelhouse with the locked Python wheels.' }
    $requiredNpm = @('chat-handoff/extension-runtime/node_modules/@playwright/mcp/cli.js')
    if ($IncludeGoalProvider) { $requiredNpm += 'goal-provider/node_modules/@agentclientprotocol/codex-acp/dist/index.js' }
    foreach ($relative in $requiredNpm) {
        if (-not (Test-Path -LiteralPath (Join-Path $ToolRoot $relative) -PathType Leaf)) {
            throw "Offline setup requires dependencies already installed on this computer: $relative"
        }
    }
} else {
    $npm = (Get-Command npm.cmd -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}
if ($Wheelhouse) { $Wheelhouse = (Resolve-Path -LiteralPath $Wheelhouse).Path }
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
$pipArguments = @('-m', 'pip', 'install', '--disable-pip-version-check', '--only-binary=:all:',
    '-r', (Join-Path $ToolRoot 'chat-handoff/requirements.lock'))
if ($Wheelhouse) { $pipArguments += @('--no-index', '--find-links', $Wheelhouse) }
Invoke-SetupCommand $venvPython $pipArguments
Invoke-SetupCommand $venvPython @('-m', 'pip', 'check')
if (-not $Offline) {
    $npmProviders = @('chat-handoff/extension-runtime')
    if ($IncludeGoalProvider) { $npmProviders += 'goal-provider' }
    foreach ($relative in $npmProviders) {
        Invoke-SetupCommand $npm @('ci', '--prefix', (Join-Path $ToolRoot $relative))
    }
}
& (Join-Path $PSScriptRoot 'configure-providers.ps1') -ToolRoot $ToolRoot -Python $venvPython -Node $node -ProjectRoot $ProjectRoot -IncludeGoalProvider:$IncludeGoalProvider
Write-Output 'Provider dependencies installed and local path templates generated.'
Write-Output 'Runner configuration, credentials, Desktop, browser, and services were not changed or started.'
