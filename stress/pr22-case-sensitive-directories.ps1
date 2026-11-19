# Updated after #370: WorktreeContext gained uses_rosen and other_version_control_threads;
# the harness sets them to a git project with no other threads, which leaves the risk as it was.
# Risk: Windows per-directory case sensitivity (fsutil setCaseSensitiveInfo, set by WSL on
# folders it creates) keeps `Api` and `api` as two directories. Two projects whose names
# differ only in ASCII case then own two distinct worktrees under one root (F5 path rule
# <root>\<project>\<branch>). ASCII folding must not merge them into one inventory row,
# one size-cache entry or one footer count. No agent/worktree is deleted.
# Adapted for #499: the harness is stress/harness/sources/pr22-case-sensitive-directories.rs, built
# with every other Rust harness into the stress crate's stress-harness binary instead of a Cargo
# project this script wrote under .tools. Its source and assertions are unchanged.
$ErrorActionPreference = 'Stop'
# Setting the attribute needs write-attributes rights the repo folder may not grant, so the
# worktree root lives in the user's temp folder.
$worktreeRoot = Join-Path ([IO.Path]::GetTempPath()) ('pr22-case-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $worktreeRoot -Force | Out-Null
fsutil file setCaseSensitiveInfo $worktreeRoot enable | Out-Host
# fsutil exits 0 even when it is denied, so confirm the attribute instead.
if (-not ((fsutil file queryCaseSensitiveInfo $worktreeRoot) -match 'enabled')) {
    Write-Output 'SKIP: per-directory case sensitivity unavailable'; exit 2
}
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr22-case-sensitive-directories $worktreeRoot
exit $LASTEXITCODE
