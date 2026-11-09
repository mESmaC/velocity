<#
.SYNOPSIS
One-time setup that makes merges with master cheap on a development machine.

.DESCRIPTION
1. Registers the `velocity-take-master` git merge driver named in .gitattributes.
   Generated files (bindings.ts, Cargo.lock, pnpm-lock.yaml) then never stop a
   `git merge origin/master` with a conflict: git keeps master's copy and you
   regenerate (AGENTS.md, Shared files). Without this driver git falls back to
   its normal text merge, so nothing breaks on a machine that skipped setup.
   All worktrees share the main repository's config, so this runs once.
2. Turns on sccache, a compiler cache shared by every worktree. Each worktree
   keeps its own target/ (no lock contention between agents), but a crate
   compiled once is never compiled again, so a fresh worktree or a master merge
   rebuilds in a fraction of the time.

.EXAMPLE
pwsh -NoProfile -File scripts/Setup-DevMachine.ps1
#>
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)

# %A is the current branch's copy and the result; %B is the branch being merged in.
git config merge.velocity-take-master.name 'Keep the incoming copy of a generated file; regenerate afterwards'
git config merge.velocity-take-master.driver 'cp -f %B %A'
Write-Host 'Registered the velocity-take-master merge driver.'

if (-not (Get-Command sccache -ErrorAction SilentlyContinue)) {
    if (Get-Command scoop -ErrorAction SilentlyContinue) {
        scoop install sccache
    } else {
        cargo install sccache --locked
    }
}
$sccache = (Get-Command sccache).Source
[Environment]::SetEnvironmentVariable('RUSTC_WRAPPER', $sccache, 'User')
if (-not [Environment]::GetEnvironmentVariable('SCCACHE_CACHE_SIZE', 'User')) {
    [Environment]::SetEnvironmentVariable('SCCACHE_CACHE_SIZE', '40G', 'User')
}
Write-Host "RUSTC_WRAPPER=$sccache for your user. Open a new terminal (and restart agents) to pick it up."
