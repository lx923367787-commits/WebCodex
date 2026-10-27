#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$ToolRoot = $PSScriptRoot,
    [string]$Python,
    [string]$Node,
    [string]$ProjectRoot,
    [string]$OfficeRoot,
    [string]$OutputDirectory,
    [switch]$IncludeGoalProvider
)
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'These providers require Windows.' }
$ToolRoot = (Resolve-Path -LiteralPath $ToolRoot).Path
if (-not $Python) {
    $Python = Join-Path $ToolRoot 'provider-python/python.exe'
    if (-not (Test-Path -LiteralPath $Python -PathType Leaf)) {
        $Python = Join-Path $ToolRoot '.venv/Scripts/python.exe'
    }
}
if (-not $Node) {
    $Node = Join-Path $ToolRoot 'node/node.exe'
    if (-not (Test-Path -LiteralPath $Node -PathType Leaf)) {
        $Node = (Get-Command node.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    }
}
$Python = (Resolve-Path -LiteralPath $Python).Path
$Node = (Resolve-Path -LiteralPath $Node).Path
if ($ProjectRoot) { $ProjectRoot = (Resolve-Path -LiteralPath $ProjectRoot).Path }
if (-not $OfficeRoot) { $OfficeRoot = Join-Path $ToolRoot 'mycodex-tools' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $ToolRoot 'provider-config' }

function Toml-Path([string]$Path) {
    ConvertTo-Json -InputObject $Path.Replace('\', '/') -Compress
}
$templates = @('chat-handoff/edge-handoff.provider.example.toml',
    'chat-agents/chat-agents.provider.example.toml')
if ($IncludeGoalProvider) {
    if (-not (Test-Path -LiteralPath (Join-Path $ToolRoot 'goal-provider/node_modules/@agentclientprotocol/codex-acp/dist/index.js') -PathType Leaf)) {
        throw 'Install the optional Goal ACP dependencies on this computer before generating its Runner template.'
    }
    $templates += 'goal-provider/codex.acp.example.toml'
}
$files = @{}
foreach ($relative in $templates) {
    $template = Get-Content -LiteralPath (Join-Path $ToolRoot $relative) -Raw
    $template = $template.Replace("'C:/WebCodex/windows/.venv/Scripts/python.exe'", (Toml-Path $Python))
    $rendered = [regex]::Replace($template, "'C:/WebCodex/windows([^']*)'", {
        param($match)
        Toml-Path ($ToolRoot + $match.Groups[1].Value)
    })
    $rendered = $rendered.Replace("'C:/Program Files/nodejs/node.exe'", (Toml-Path $Node))
    $name = [IO.Path]::GetFileName($relative).Replace('.example.toml', '.local.toml')
    $files[$name] = $rendered
}
if ($ProjectRoot -and (Test-Path -LiteralPath $OfficeRoot -PathType Container)) {
    $OfficeRoot = (Resolve-Path -LiteralPath $OfficeRoot).Path
    foreach ($relative in @('python/python.exe', 'plugin.py', 'document_worker.py',
        'fonts/ZCOOLXiaoWei-Regular.ttf', 'tesseract/tesseract.exe', 'tesseract/tessdata')) {
        if (-not (Test-Path -LiteralPath (Join-Path $OfficeRoot $relative))) {
            throw "Missing Office runtime input: $relative"
        }
    }
    $files['mycodex-tools.provider.local.toml'] = @"
# Merge this provider into the selected Runner; do not duplicate its id.
# cwd must equal the registered project root. This file contains no credentials.
[[plugins.providers]]
id = "mycodex-tools"
name = "MyCodex Office and PDF"
command = $(Toml-Path (Join-Path $OfficeRoot 'python/python.exe'))
args = [$(Toml-Path (Join-Path $OfficeRoot 'plugin.py')), '--font-path', $(Toml-Path (Join-Path $OfficeRoot 'fonts/ZCOOLXiaoWei-Regular.ttf'))]
cwd = $(Toml-Path $ProjectRoot)
timeout_secs = 60
document_worker = $(Toml-Path (Join-Path $OfficeRoot 'document_worker.py'))
document_worker_args = ['--font-path', $(Toml-Path (Join-Path $OfficeRoot 'fonts/ZCOOLXiaoWei-Regular.ttf')), '--tesseract-path', $(Toml-Path (Join-Path $OfficeRoot 'tesseract/tesseract.exe')), '--tessdata-path', $(Toml-Path (Join-Path $OfficeRoot 'tesseract/tessdata'))]
"@
} elseif (Test-Path -LiteralPath $OfficeRoot -PathType Container) {
    Write-Warning 'Supply -ProjectRoot with the registered project directory to also generate the Office provider.'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
foreach ($name in $files.Keys) {
    [IO.File]::WriteAllText((Join-Path $OutputDirectory $name), $files[$name], [Text.UTF8Encoding]::new($false))
}
Write-Output "Generated local path templates in: $OutputDirectory"
Write-Output 'No dependencies downloaded, accounts read, Runner settings changed, or services started.'
