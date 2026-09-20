[CmdletBinding()]
param(
    [string]$Python = 'python',
    [string]$VenvPath = 'E:\wbecodex\.local\venvs\mycodex-tools'
)
$ErrorActionPreference = 'Stop'
& $Python -c 'import sys; assert sys.version_info[:2] == (3, 11), "Python 3.11 is required"'
if ($LASTEXITCODE -ne 0) { throw 'Python 3.11 check failed.' }
$venvPython = Join-Path $VenvPath 'Scripts\python.exe'
if (-not (Test-Path -LiteralPath $venvPython)) {
    if (Test-Path -LiteralPath $VenvPath) { throw 'Target exists without a venv Python; choose an empty location.' }
    & $Python -m venv $VenvPath
    if ($LASTEXITCODE -ne 0) { throw 'Virtual environment creation failed.' }
}
& $venvPython -c 'import sys; assert sys.version_info[:2] == (3, 11), "Existing venv must use Python 3.11"'
if ($LASTEXITCODE -ne 0) { throw 'Existing virtual environment version mismatch.' }
& $venvPython -m pip install --disable-pip-version-check -r (Join-Path $PSScriptRoot 'requirements.lock')
if ($LASTEXITCODE -ne 0) { throw 'Dependency installation failed.' }
& $venvPython -m pip check
if ($LASTEXITCODE -ne 0) { throw 'Dependency validation failed.' }
Write-Output "Installed MyCodex tools: $venvPython"
Write-Output 'Runner configuration and running services were not changed.'
