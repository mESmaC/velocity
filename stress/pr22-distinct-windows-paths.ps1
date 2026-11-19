# Risk: Unicode folding must not merge two distinct Windows directories into
# one inventory row, size-cache entry or footer count. No agent/worktree is deleted.
# Updated after #370: WorktreeContext gained uses_rosen and other_version_control_threads;
# the harness sets them to a git project with no other threads, which leaves the risk as it was.
# Adapted for #499: the harness is stress/harness/sources/pr22-distinct-windows-paths.rs, built
# with every other Rust harness into the stress crate's stress-harness binary instead of a Cargo
# project this script wrote. Its source, assertions and fixture folder are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixtureRoot = Join-Path $repositoryRoot ('.tools\pr22-distinct-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr22-distinct-windows-paths $fixtureRoot
exit $LASTEXITCODE
