[CmdletBinding()]
param([string] $Distribution = 'Ubuntu')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$proofsDirectory = Split-Path -Parent $PSScriptRoot
$repositoryDirectory = Split-Path -Parent $proofsDirectory
$pins = Get-Content -Raw (Join-Path $proofsDirectory 'toolchain-pins.json') | ConvertFrom-Json

function Invoke-Checked {
    param([string] $Program, [string[]] $Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

function Get-VerifiedArchive {
    $downloadDirectory = Join-Path $repositoryDirectory '.tools/downloads'
    New-Item -ItemType Directory -Force -Path $downloadDirectory | Out-Null
    $archivePath = Join-Path $downloadDirectory $pins.archive
    if (-not (Test-Path -LiteralPath $archivePath)) {
        $url = "https://github.com/AeneasVerif/aeneas/releases/download/$($pins.release)/$($pins.archive)"
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $archivePath
    }
    if ((Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $pins.sha256) {
        throw "Archive checksum mismatch: $archivePath. Remove that file and run setup again."
    }
    return $archivePath
}

function Install-Translator {
    param([string] $ArchivePath)
    $translatorDirectory = Join-Path $repositoryDirectory '.tools/translator'
    New-Item -ItemType Directory -Force -Path $translatorDirectory | Out-Null
    Invoke-Checked 'tar.exe' @('-xzf', $ArchivePath, '-C', $translatorDirectory)
    $bundledToolchain = Get-Content -Raw (Join-Path $translatorDirectory 'rust-toolchain')
    if (-not $bundledToolchain.Contains('channel = "' + $pins.rust + '"')) {
        throw 'The translator bundle requires a different Rust toolchain.'
    }
    $linuxTranslator = Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', 'wslpath', '-u', $translatorDirectory)
    $version = Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', "$linuxTranslator/aeneas", '-version')
    if ($version.Trim() -ne "aeneas $($pins.release)") { throw 'Unexpected Aeneas version.' }
    $linuxPath = Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', 'sh', '-lc', 'printenv PATH')
    Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', 'env', "PATH=$linuxPath", 'rustup',
        'toolchain', 'install', $pins.rust, '--profile', 'minimal', '--component', 'rustc-dev,rust-src,llvm-tools,miri',
        '--no-self-update')
}

if ((Get-Content -Raw (Join-Path $proofsDirectory 'lean-toolchain')).Trim() -ne $pins.lean) {
    throw 'lean-toolchain and toolchain-pins.json disagree.'
}
$architecture = Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', 'uname', '-m')
if ($architecture.Trim() -ne 'x86_64') { throw 'This pinned translator bundle requires x86_64 WSL.' }
Install-Translator (Get-VerifiedArchive)
Push-Location -LiteralPath $proofsDirectory
try {
    Invoke-Checked 'elan.exe' @('toolchain', 'install', $pins.lean)
    Invoke-Checked 'lake.exe' @('exe', 'cache', 'get')
    $manifest = Get-Content -Raw (Join-Path $proofsDirectory 'lake-manifest.json') | ConvertFrom-Json
    foreach ($package in @('aeneas', 'mathlib')) {
        $resolvedPackage = $manifest.packages | Where-Object name -EQ $package
        if ($resolvedPackage.rev -ne $pins.$package) { throw "Unexpected $package revision in Lake manifest." }
    }
    $backendDirectory = Join-Path $proofsDirectory '.lake/packages/aeneas/backends/lean'
    if ((Get-Content -Raw (Join-Path $backendDirectory 'lean-toolchain')).Trim() -ne $pins.lean) {
        throw 'The Aeneas backend requires a different Lean toolchain.'
    }
    $charonPin = Get-Content (Join-Path $proofsDirectory '.lake/packages/aeneas/charon-pin') |
        Where-Object { $_ -match '^[0-9a-f]{40}$' }
    if ($charonPin -ne $pins.charon) { throw 'The Aeneas source requires a different Charon revision.' }
    # Upstream publishes no Windows build archive; compile its Lean backend locally.
    Invoke-Checked 'lake.exe' @('--no-cache', 'build')
} finally { Pop-Location }
