# Runs one Rust harness of the stress crate's `stress-harness` binary (#499).
# Every Rust harness is a module of that one binary in the root workspace, so the suite
# compiles shared dependencies once, with one feature resolution and one target folder,
# instead of each script writing and building its own Cargo project. Harness sources live
# in stress/harness/sources; stress/harness/build.rs turns each into a module.
# The stress runner builds the binary once for the whole suite and passes its path in
# VELOCITY_STRESS_HARNESS_EXE. A script run on its own builds it here, through the build queue.
# VELOCITY_STRESS_HARNESS names the harness, so the harness reads its arguments unchanged.

function Get-StressHarnessExecutable {
    if ($env:VELOCITY_STRESS_HARNESS_EXE) { return $env:VELOCITY_STRESS_HARNESS_EXE }
    $repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
    if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $repositoryRoot 'target' }
    & (Join-Path $repositoryRoot 'scripts/build-queue.ps1') cargo build --locked `
        --manifest-path (Join-Path $repositoryRoot 'Cargo.toml') -p stress --features harnesses --bin stress-harness |
        Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Could not build the stress harnesses' }
    $targetDirectory = [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR, $repositoryRoot)
    $executable = Join-Path $targetDirectory (Join-Path 'debug' ('stress-harness' + $(if ($IsWindows) { '.exe' } else { '' })))
    if (-not (Test-Path -LiteralPath $executable)) { throw "The stress harness build left no binary at $executable" }
    return $executable
}

# Runs the harness named after its source file (stress/harness/sources/<Name>.rs) with the
# given arguments. The caller reads the harness's exit code from $LASTEXITCODE.
function Invoke-StressHarness {
    param(
        [Parameter(Mandatory)][string]$Name,
        [string[]]$Arguments = @()
    )
    $executable = Get-StressHarnessExecutable
    $previous = $env:VELOCITY_STRESS_HARNESS
    $env:VELOCITY_STRESS_HARNESS = $Name
    try {
        & $executable @Arguments
    } finally {
        $env:VELOCITY_STRESS_HARNESS = $previous
    }
}

Export-ModuleMember -Function Invoke-StressHarness
