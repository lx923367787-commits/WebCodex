#Requires -Version 7.2
# Re-run a frozen local validation source with its recorded toolchain and build
# identity. The manifest is produced when the source snapshot is captured.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Manifest,
    [ValidateSet('preflight', 'tests', 'build', 'all')][string]$Lane = 'all'
)
$ErrorActionPreference = 'Stop'
$manifestPath = (Resolve-Path -LiteralPath $Manifest).Path
$record = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$source = [IO.Path]::GetFullPath($record.source)
$evidence = Split-Path -Parent $manifestPath

function Assert-FrozenSource {
    [string[]]$names = @($record.files.PSObject.Properties.Name)
    [Array]::Sort($names, [StringComparer]::Ordinal)
    $canonical = [Text.StringBuilder]::new()
    foreach ($name in $names) {
        [void]$canonical.Append($name).Append([char]0).Append([string]$record.files.$name).Append("`n")
    }
    $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($canonical.ToString())))
    if ($digest -ne $record.source_manifest_sha256) { throw 'Source manifest identity mismatch' }
    foreach ($entry in $record.files.PSObject.Properties) {
        $path = [IO.Path]::GetFullPath((Join-Path $source $entry.Name))
        if (-not $path.StartsWith($source + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Source manifest path escapes snapshot: $($entry.Name)"
        }
        if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $entry.Value) {
            throw "Frozen source changed: $($entry.Name)"
        }
    }
    $actualFiles = @(Get-ChildItem -LiteralPath $source -Recurse -File -Force)
    if ($actualFiles.Count -ne @($record.files.PSObject.Properties).Count) {
        throw 'Frozen source contains unrecorded files'
    }
}

Assert-FrozenSource
foreach ($config in $record.cargo_configs) {
    $digest = if (Test-Path -LiteralPath $config.path -PathType Leaf) { (Get-FileHash -LiteralPath $config.path).Hash } else { $null }
    if ($digest -ne $config.sha256) { throw "Cargo configuration changed: $($config.path)" }
}
foreach ($tool in $record.tools.PSObject.Properties) {
    if ((Get-FileHash -LiteralPath $tool.Value.path -Algorithm SHA256).Hash -ne $tool.Value.sha256) {
        throw "Recorded tool changed: $($tool.Name)"
    }
}
# Cargo's dep-info can retain paths from another checkout when a target
# directory is shared. Reuse is allowed only for this exact frozen source.
$target = [IO.Path]::GetFullPath($record.environment.CARGO_TARGET_DIR)
if ($target -eq $source -or $target.StartsWith($source + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Build output must stay outside the frozen source'
}
$stampPath = Join-Path $target '.webcodex-validation-source.json'
$expectedStamp = [ordered]@{
    source = $source
    source_sha256 = $record.source_manifest_sha256
    rustc_sha256 = $record.tools.rustc.sha256
    git_commit = $record.environment.WEBCODEX_GIT_COMMIT
    git_dirty = $record.environment.WEBCODEX_GIT_DIRTY
    built_at = $record.environment.WEBCODEX_BUILT_AT
}
if (Test-Path -LiteralPath $stampPath) {
    $stamp = Get-Content -LiteralPath $stampPath -Raw | ConvertFrom-Json
    foreach ($key in $expectedStamp.Keys) {
        if ($stamp.$key -ne $expectedStamp[$key]) { throw "Build cache belongs to another source/build: $key" }
    }
} else {
    if ((Test-Path -LiteralPath $target) -and @(Get-ChildItem -LiteralPath $target -Force).Count) {
        throw 'Build cache has no source provenance; select an empty target directory'
    }
    New-Item -ItemType Directory -Path $target -Force | Out-Null
    $expectedStamp | ConvertTo-Json | Set-Content -LiteralPath $stampPath -Encoding utf8
}
$savedEnvironment = @{}
foreach ($entry in $record.environment.PSObject.Properties) {
    $savedEnvironment[$entry.Name] = [Environment]::GetEnvironmentVariable($entry.Name, 'Process')
    if ($null -eq $entry.Value) {
        Remove-Item -LiteralPath "Env:$($entry.Name)" -ErrorAction SilentlyContinue
    } else {
        [Environment]::SetEnvironmentVariable($entry.Name, [string]$entry.Value, 'Process')
    }
}
$previousLocation = Get-Location
$results = [Collections.Generic.List[object]]::new()
function Invoke-RecordedCargo {
    param([string]$Name, [string[]]$Arguments)
    $log = Join-Path $evidence "$Name.log"
    Write-Output "Running $Name"
    & $record.tools.cargo.path @Arguments *> $log
    $code = $LASTEXITCODE
    $results.Add([pscustomobject]@{ name = $Name; command = @($record.tools.cargo.path) + $Arguments; exit_code = $code; log = $log })
    $results | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $evidence "$Lane-results.json") -Encoding utf8
    Write-Output "$Name exit=$code"
    if ($code -ne 0) { Get-Content -LiteralPath $log -Tail 20 }
}
try {
    Set-Location -LiteralPath $source
    $observedRust = (& $record.tools.rustc.path --version --verbose | Out-String).Trim().Replace("`r`n", "`n")
    if ($observedRust -ne $record.rustc_version.Replace("`r`n", "`n")) {
        throw 'Rust compiler version differs from the recorded toolchain'
    }
    if ($Lane -in @('tests', 'all')) {
        Invoke-RecordedCargo 'format' @('fmt', '--check')
        Invoke-RecordedCargo 'core-default' @('test', '--locked', '-p', 'webcodex-core', '--lib', 'validation_identity')
        Invoke-RecordedCargo 'core-preserve-order' @('test', '--locked', '-p', 'webcodex-core', '--features', 'serde_json/preserve_order', '--lib', 'validation_identity')
        Invoke-RecordedCargo 'workspace-tests' @('test', '--locked', '--workspace', '--no-fail-fast', '--', '--test-threads=4')
        Invoke-RecordedCargo 'runner-real-process' @('test', '--locked', '-p', 'webcodex-runner', 'runner_real_process', '--', '--ignored', '--test-threads=1')
        Invoke-RecordedCargo 'runner-toolchain' @('test', '--locked', '-p', 'webcodex-runner', 'runner_toolchain_real_process', '--', '--ignored', '--test-threads=1')
    }
    if ($Lane -in @('build', 'all')) {
        Invoke-RecordedCargo 'dogfood-build' @('build', '--locked', '--profile', 'dogfood', '-p', 'webcodex', '-p', 'webcodex-runner', '-p', 'webcodex-cli', '--bins')
        if ($results[-1].exit_code -eq 0) {
            $binaries = foreach ($name in @('webcodex-server', 'webcodex-runner', 'webcodex')) {
                $path = Join-Path $record.environment.CARGO_TARGET_DIR "dogfood/$name.exe"
                $identity = (& $path --version | Out-String).Trim()
                if ($LASTEXITCODE -ne 0 -or -not $identity.Contains("commit $($record.environment.WEBCODEX_GIT_COMMIT), dirty=$($record.environment.WEBCODEX_GIT_DIRTY), built_at=$($record.environment.WEBCODEX_BUILT_AT)")) {
                    throw "Binary identity mismatch: $name"
                }
                [pscustomobject]@{ path = $path; sha256 = (Get-FileHash -LiteralPath $path).Hash; identity = $identity }
            }
            $binaries | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $evidence 'binaries.json') -Encoding utf8
        }
    }
    Assert-FrozenSource
    if (@($results | Where-Object exit_code -ne 0).Count) { throw 'Validation failed; inspect recorded logs' }
} finally {
    Set-Location -LiteralPath $previousLocation
    foreach ($name in $savedEnvironment.Keys) {
        if ($null -eq $savedEnvironment[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
        }
    }
}
