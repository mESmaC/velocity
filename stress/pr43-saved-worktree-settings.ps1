# Risk: after #22, starting a thread must honor saved project roots, copied
# files and setup commands (F5/F13). Plans only; no worktree or agent is started.
# Adapted for #499: the harness is stress/harness/sources/pr43-saved-worktree-settings.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote. Its source, assertions and fixture folder are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr43-saved-worktree-settings'
$fixtureRoot = Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $harnessRoot -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr43-saved-worktree-settings $fixtureRoot
exit $LASTEXITCODE
