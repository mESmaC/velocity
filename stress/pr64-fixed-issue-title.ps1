# Risk: merging M3 title generation into F9 must not request a second agent or
# replace the issue's fixed title. The composer control prevents disabling all
# title requests from making this boundary check pass vacuously.
# Adapted for #499: the harness is stress/harness/sources/pr64-fixed-issue-title.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote. Its source, assertions and fixture folder are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr64-fixed-issue-title'
$fixtureRoot = Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $harnessRoot -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr64-fixed-issue-title $fixtureRoot
exit $LASTEXITCODE
