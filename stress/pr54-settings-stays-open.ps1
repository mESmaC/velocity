# Risk: with a thread open, Settings must stay open while saved state refreshes the main
# window (an agent's status change, a deletion, a project switch all call refresh_layout).
# A refresh that forces the selected thread's view back would make Settings unusable
# while any agent runs (§5 navigation, F13). Uses the real store and layout reducer;
# no agent, worktree or provider is started.
# Adapted for #499: the harness is stress/harness/sources/pr54-settings-stays-open.rs, built with
# every other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote into the fixture. Its source, assertions and fixture folder are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixtureRoot = Join-Path $repositoryRoot ('.tools\pr54-settings-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr54-settings-stays-open $fixtureRoot
exit $LASTEXITCODE
