# Risk: F6 cleanup must identify an owned worktree without conflating distinct
# Windows directory names. All destructive calls target this isolated fixture.
# Updated for #230: since #177 (b829fb6) cleanup keeps any worktree with uncommitted or
# unpushed work, so each fixture worktree now commits its sentinel and pushes its branch
# to a local bare remote, the state a merged PR's worktree is in. Assertions unchanged.
# Adapted for #499: the harness is stress/harness/sources/pr50-unicode-worktree-cleanup.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote. Its source, assertions and fixture folder are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr50-unicode-worktree-cleanup'
$fixtureRoot = [IO.Path]::GetFullPath((Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))))
if (-not $fixtureRoot.StartsWith($harnessRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture path escaped this review checkout' }
New-Item -ItemType Directory -Path $harnessRoot -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr50-unicode-worktree-cleanup $fixtureRoot
exit $LASTEXITCODE
