# Downloads the Windows Rosen binaries from the Rosen release Velocity pins (#433) into
# -Destination as rosen.exe and rosen-server.exe, and prints the step output `found`.
#
# Rosen's release workflow attaches each binary per target, SHA256SUMS.txt over them, and
# rosen-client-commit.txt naming the commit the release was built from (mESmaC/rosen#398).
# The commit must be the pinned rev: a tag that names another commit is a wrong pin and
# fails the release. Every binary must match its sum. `found=false` (no release pinned,
# or its binaries not attached yet) sends the caller to its one source build instead.
param(
    [Parameter(Mandatory)] [string] $Url,
    [Parameter(Mandatory)] [string] $Rev,
    [string] $Release = '',
    [Parameter(Mandatory)] [string] $Destination
)
$ErrorActionPreference = 'Stop'

$target = 'x86_64-pc-windows-msvc'
# Release asset name to the file name Velocity's bundle and stress harness expect.
$binaries = [ordered]@{
    "rosen-$target.exe" = 'rosen.exe'
    "rosen-server-$target.exe" = 'rosen-server.exe'
}

function Write-Found([bool] $found) {
    "found=$($found.ToString().ToLower())" >> $env:GITHUB_OUTPUT
}

# Returns $false when the asset does not exist; other failures stop the step.
function Get-Asset([string] $base, [string] $name, [string] $path) {
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$name" -OutFile $path `
            -MaximumRetryCount 2 -RetryIntervalSec 2
        return $true
    } catch {
        $status = $_.Exception.Response.StatusCode.value__
        if ($status -eq 404) { return $false }
        throw
    }
}

if (-not $Release) {
    Write-Host "No Rosen release is pinned for $Rev; building it from source."
    Write-Found $false
    exit 0
}

$base = "$($Url -replace '\.git$', '')/releases/download/$Release"
$download = New-Item -ItemType Directory -Force (Join-Path $env:RUNNER_TEMP 'rosen-download')

$commitFile = Join-Path $download 'rosen-client-commit.txt'
if (-not (Get-Asset $base 'rosen-client-commit.txt' $commitFile)) {
    Write-Warning "Rosen $Release has no binaries attached; building $Rev from source."
    Write-Found $false
    exit 0
}
$built = (Get-Content -Raw $commitFile).Trim()
if ($built -cne $Rev) {
    throw "Rosen $Release was built from $built, but Cargo.toml pins $Rev. Fix [workspace.metadata.rosen] release."
}

$sumsFile = Join-Path $download 'SHA256SUMS.txt'
if (-not (Get-Asset $base 'SHA256SUMS.txt' $sumsFile)) { throw "Rosen $Release has no SHA256SUMS.txt" }
$sums = @{}
foreach ($line in Get-Content $sumsFile) {
    # sha256sum writes `<hash>  <name>`, or `<hash> *<name>` in binary mode.
    if ($line -match '^([0-9a-fA-F]{64}) [ *](.+)$') { $sums[$Matches[2].Trim()] = $Matches[1].ToLower() }
}

$bin = New-Item -ItemType Directory -Force $Destination
foreach ($asset in $binaries.Keys) {
    $path = Join-Path $download $asset
    if (-not (Get-Asset $base $asset $path)) { throw "Rosen $Release has no $asset" }
    $expected = $sums[$asset]
    if (-not $expected) { throw "SHA256SUMS.txt of Rosen $Release lists no $asset" }
    $actual = (Get-FileHash $path -Algorithm SHA256).Hash.ToLower()
    if ($actual -cne $expected) { throw "$asset is $actual, but Rosen $Release lists $expected" }
    $installed = Join-Path $bin $binaries[$asset]
    Move-Item -Force $path $installed
    & $installed --version
    if ($LASTEXITCODE -ne 0) { throw "$($binaries[$asset]) from Rosen $Release does not run" }
}
Write-Host "Rosen $Release ($Rev) verified against its SHA256SUMS.txt."
Write-Found $true
