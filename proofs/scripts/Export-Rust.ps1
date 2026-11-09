[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $CratePath,
    [Parameter(Mandatory)] [ValidatePattern('^[A-Z][A-Za-z0-9]*$')] [string] $Feature,
    [string] $Distribution = 'Ubuntu',
    # Item paths to translate, e.g. 'crate::theme'. Each feature translates only its
    # own module: two translations of the same item collide when Lean imports both.
    [string[]] $StartFrom = @()
)

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

function ConvertTo-LinuxPath {
    param([string] $WindowsPath)
    $resolvedPath = (Resolve-Path -LiteralPath $WindowsPath).ProviderPath
    return Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', 'wslpath', '-u', $resolvedPath)
}

$resolvedCrate = (Resolve-Path -LiteralPath $CratePath).ProviderPath
if (-not (Test-Path -LiteralPath (Join-Path $resolvedCrate 'Cargo.toml'))) {
    throw 'CratePath must point to a Rust crate containing Cargo.toml.'
}
$translatorDirectory = ConvertTo-LinuxPath (Join-Path $repositoryDirectory '.tools/translator')
$version = Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', "$translatorDirectory/aeneas", '-version')
if ($version.Trim() -ne "aeneas $($pins.release)") { throw 'Run Setup-Toolchain.ps1 for the pinned release.' }
$linuxPath = Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', 'sh', '-lc', 'printenv PATH')
$intermediateDirectory = Join-Path $proofsDirectory 'generated-llbc'
New-Item -ItemType Directory -Force -Path $intermediateDirectory | Out-Null
$intermediateFile = "$(ConvertTo-LinuxPath $intermediateDirectory)/$Feature.llbc"
$linuxCrate = ConvertTo-LinuxPath $resolvedCrate
$linuxProofs = ConvertTo-LinuxPath $proofsDirectory

# Argument arrays keep Windows drive letters, spaces and shell metacharacters out of shell code.
$charonArguments = @('-d', $Distribution, '--cd', $linuxCrate, '--exec', 'env', "PATH=$linuxPath",
    "$translatorDirectory/charon", 'cargo', '--preset=aeneas', '--dest-file', $intermediateFile)
foreach ($item in $StartFrom) { $charonArguments += @('--start-from', $item) }
Invoke-Checked 'wsl.exe' ($charonArguments + @('--', '--lib'))
Invoke-Checked 'wsl.exe' @('-d', $Distribution, '--exec', "$translatorDirectory/aeneas", '-backend', 'lean',
    $intermediateFile, '-dest', $linuxProofs, '-subdir', "$Feature/Generated", '-split-files',
    '-namespace', "Velocity.$Feature.Generated", '-no-progress-bar')
Write-Host "Generated proofs/$Feature/Generated. Complete external definitions, import the generated modules"
Write-Host 'from the confidence proof and Velocity.lean, then run lake build in proofs/.'
