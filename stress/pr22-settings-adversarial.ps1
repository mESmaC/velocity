# Risks: settings lost on reload/save failure, stale project edits, and Windows inventory aliases.
# Calls the actual core, store and git crates. Every fixture stays under this review checkout.
# Adapted for #499: the harness is stress/harness/sources/pr22-settings-adversarial.rs, built with
# every other Rust harness into the stress crate's stress-harness binary instead of a Cargo
# project this script wrote. The -TargetDirectory parameter only placed that project's build
# and is gone. The harness source, assertions and fixture folder are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixtureRoot = Join-Path $repositoryRoot ('.tools\pr22-fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null
Write-Output "Fixture root: $fixtureRoot"
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr22-settings-adversarial $fixtureRoot
exit $LASTEXITCODE
