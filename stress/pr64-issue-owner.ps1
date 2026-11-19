# Risk: F9 ownership must survive reload and refuse duplicate work while retaining
# F6's non-reused identifiers and birth metadata. Uses the real saved-state store;
# no agents, GitHub writes or worktree deletion are run.
# Adapted for #499: the harness is stress/harness/sources/pr64-issue-owner.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote. Its source, assertions and fixture folder are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr64-issue-owner'
$fixtureRoot = Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $harnessRoot -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr64-issue-owner $fixtureRoot
exit $LASTEXITCODE
