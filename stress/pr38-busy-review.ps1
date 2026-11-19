# Risk: SPEC.md Rule 1/F4 requires messages to a busy PR-review agent to queue.
# Exercises real state.json load, project projection and the Rust composer reducer.
# Does not launch an ACP process or test native UI/IPC transport.
# Adapted for #499: the harness is stress/harness/sources/pr38-busy-review.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project this
# script wrote into the fixture. Its source, assertions and fixture folder are unchanged.
param()
$ErrorActionPreference = 'Stop'
$repository = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixture = Join-Path $repository ('.tools\pr38-busy-review\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
Write-Output "Fixture: $fixture"
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr38-busy-review $fixture
exit $LASTEXITCODE
