#Requires -Version 7.2
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PythonBase,
    [Parameter(Mandatory)][string]$TesseractBase,
    [Parameter(Mandatory)][string]$GraphvizRoot,
    [Parameter(Mandatory)][string]$GraphvizArchive,
    [string]$Python = 'python',
    [string]$Fonts = (Join-Path $PSScriptRoot 'assets/fonts'),
    [string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This build is for Windows x64 only.' }
$root = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $root ('ignore/windows-build-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Choose a new output directory.' }
foreach ($tool in @('git', 'cargo', 'node', 'npm.cmd', $Python)) {
    Get-Command $tool -ErrorAction Stop | Out-Null
}
foreach ($path in @($PythonBase, $TesseractBase, $GraphvizRoot, $GraphvizArchive, $Fonts)) {
    if (-not (Test-Path -LiteralPath $path)) { throw "Missing build input: $path" }
}
function Invoke-BuildCommand([string]$Program, [string[]]$ArgumentList) {
    & $Program @ArgumentList
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
$saved = @{}
foreach ($key in @('CARGO_TARGET_DIR', 'WEBCODEX_GIT_COMMIT', 'WEBCODEX_GIT_DIRTY', 'WEBCODEX_BUILT_AT')) {
    $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
Push-Location -LiteralPath $root
try {
    $providerNames = @('chat-handoff', 'chat-agents', 'subagent-pilot', 'goal-provider')
    foreach ($name in $providerNames) {
        $tracked = @(git ls-files -- "windows/$name/")
        if ($LASTEXITCODE -ne 0 -or -not $tracked.Count) { throw "Missing tracked provider source: $name. Build a committed checkout." }
    }
    Invoke-BuildCommand $Python @('-c', 'import sys; assert sys.version_info[:2] == (3, 11), "Python 3.11 required"')
    $env:WEBCODEX_GIT_COMMIT = (git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Build from a Git checkout.' }
    $changes = @(git status --porcelain --untracked-files=normal)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect source status.' }
    $env:WEBCODEX_GIT_DIRTY = if ($changes.Count) { 'true' } else { 'false' }
    $env:WEBCODEX_BUILT_AT = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds().ToString()
    $env:CARGO_TARGET_DIR = Join-Path $root 'target/windows-snapshot'
    $target = 'x86_64-pc-windows-msvc'
    $bin = Join-Path $env:CARGO_TARGET_DIR "$target/dogfood"

    foreach ($frontend in @('frontend', 'apps/desktop')) {
        Push-Location -LiteralPath (Join-Path $root $frontend)
        try {
            Invoke-BuildCommand 'npm.cmd' @('ci')
            Invoke-BuildCommand 'npm.cmd' @('run', 'build')
            Invoke-BuildCommand 'npm.cmd' @('test')
        } finally { Pop-Location }
    }
    Invoke-BuildCommand 'cargo' @('build', '--locked', '--profile', 'dogfood', '--target', $target,
        '-p', 'webcodex', '-p', 'webcodex-cli', '-p', 'webcodex-runner')
    Invoke-BuildCommand 'cargo' @('build', '--locked', '--profile', 'dogfood', '--target', $target,
        '--manifest-path', 'apps/desktop/src-tauri/Cargo.toml', '--features', 'tauri/custom-protocol',
        '--bin', 'webcodex-desktop')
    Invoke-BuildCommand (Join-Path $bin 'webcodex-desktop.exe') @('--check-embedded-assets')

    $app = Join-Path $OutputDirectory 'WebCodex Desktop'
    $runtime = Join-Path $app 'webcodex-runtime'
    New-Item -ItemType Directory -Path $runtime -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $bin 'webcodex-desktop.exe') -Destination (Join-Path $app 'WebCodex.exe')
    foreach ($name in @('webcodex', 'webcodex-server', 'webcodex-runner')) {
        $exe = Join-Path $bin "$name.exe"
        Invoke-BuildCommand $exe @('--version')
        Copy-Item -LiteralPath $exe -Destination $runtime
    }
    $office = Join-Path $app 'mycodex-tools'
    Invoke-BuildCommand $Python @('plugins/mycodex-tools/build_local_bundle.py', '--output', $office,
        '--python-base', $PythonBase, '--font-dir', $Fonts, '--tesseract-base', $TesseractBase)
    $drawing = Join-Path $OutputDirectory 'drawing-addon'
    Invoke-BuildCommand $Python @('plugins/mycodex-tools/build_drawing_addon.py', '--destination', $drawing,
        '--graphviz-root', $GraphvizRoot, '--graphviz-archive', $GraphvizArchive)
    foreach ($item in Get-ChildItem -LiteralPath $drawing) {
        $destination = Join-Path $office $item.Name
        if (Test-Path -LiteralPath $destination) { throw "Drawing add-on collides with $destination" }
        Copy-Item -LiteralPath $item.FullName -Destination $destination -Recurse
    }
    foreach ($name in $providerNames) {
        # Copy source and lockfiles only; dependency installation follows the
        # adjacent provider guide and never copies an existing runtime's state.
        $destination = Join-Path $app $name
        New-Item -ItemType Directory -Path $destination | Out-Null
        $source = Join-Path $PSScriptRoot $name
        $tracked = @(git -c core.quotepath=false ls-files -- "windows/$name/")
        if ($LASTEXITCODE -ne 0 -or -not $tracked.Count) { throw "Missing tracked provider source: $name" }
        foreach ($relativeToRoot in $tracked) {
            $original = Join-Path $root $relativeToRoot
            $relative = [IO.Path]::GetRelativePath($source, $original)
            $copy = Join-Path $destination $relative
            New-Item -ItemType Directory -Path (Split-Path -Parent $copy) -Force | Out-Null
            Copy-Item -LiteralPath $original -Destination $copy
        }
    }
    Copy-Item -LiteralPath (Join-Path $root 'plugins/safe-delete') -Destination (Join-Path $app 'safe-delete') -Recurse
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'setup-providers.ps1') -Destination $app
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $app
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README.md') -Destination (Join-Path $OutputDirectory 'BUILD-GUIDE.md')
    $files = [ordered]@{}
    foreach ($file in Get-ChildItem -LiteralPath $app -File -Recurse) {
        $files[[IO.Path]::GetRelativePath($app, $file.FullName).Replace('\', '/')] = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    [ordered]@{
        platform = $target; source_commit = $env:WEBCODEX_GIT_COMMIT
        source_dirty = ($env:WEBCODEX_GIT_DIRTY -eq 'true'); built_at = $env:WEBCODEX_BUILT_AT
        files = $files
    } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'BUILD-MANIFEST.json') -Encoding utf8
    Write-Output "Windows package: $app"
    Write-Output 'Follow windows/README.md to provision provider dependencies and configure your own credentials.'
} finally {
    Pop-Location
    foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process') }
}
