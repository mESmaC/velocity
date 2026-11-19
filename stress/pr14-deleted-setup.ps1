# Risk: a setup completion retained across F12 deletion must not rename or overwrite
# a replacement thread that reused its ID. Runs real git/planner/store/runtime code and
# the unchanged native completion/record_names bodies with a store-only handle adapter.
# All filesystem effects are confined to the isolated fixture; no agent is started.
# Updated for #230: the native completion decision moved from an inline `let wanted = lock(..)`
# block into `mark_worktree_ready`; the fixture now takes that function's unchanged body.
# Updated for #485: use the native folder_ready path and extract its helpers without relying
# on adjacent doc comments. Exercise the Git folder path and keep the persisted ID check.
# Adapted for #499: the harness is stress/harness/sources/pr14-deleted-setup.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project this
# script wrote. stress/harness/spliced.rs now takes the same five native items from
# src-tauri/src/threads.rs when the binary is built, with the same unique-match check: if one
# moved, this harness fails with "Native completion item changed". Assertions unchanged.
$ErrorActionPreference='Stop'
$repositoryRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot=Join-Path $repositoryRoot '.tools\pr14-deleted-setup'
New-Item -ItemType Directory -Path $harnessRoot -Force | Out-Null
$fixtureRoot=[IO.Path]::GetFullPath((Join-Path $harnessRoot ('fixture-'+[Guid]::NewGuid().ToString('N').Substring(0,8))))
if(-not $fixtureRoot.StartsWith($harnessRoot+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Fixture root escaped review checkout'}
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr14-deleted-setup $fixtureRoot
exit $LASTEXITCODE
