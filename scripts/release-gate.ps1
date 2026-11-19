<#
.SYNOPSIS
The release gate (SPEC.md F7 "The release gate", #381): what a commit must pass
before it is released.

.DESCRIPTION
On a Rosen project no approval gates a land, so the release is where everything
is held to the full bar. Run it on the release commit. It stops at the first
failure and exits non-zero. It has three stages, which -Stage runs one at a time
and a local run takes in order:
1. Checks:
   - A green Velocity integration build on the exact release commit, or with
     -Green, on Rosen's last green commit (R12), which the release commit must
     be an ancestor of. Rosen advances green as a prefix of the base, so the gate
     reads that instead of re-deriving it from a status on every commit (#431).
   - No open issue labelled release-blocker (gh, on the checkout's repository).
   Under GitHub Actions it records `exact-green`: whether the release commit
   itself carries the green status. A beta release with an exact green commit
   skips the other two stages (#434).
2. Stress: cargo run -p stress -- --scripts, every stress script. --all would
   add the workspace tests, which the green integration build in step 1 already
   ran, and the UI build, which the release build itself runs (#498).
3. Proofs: lake build in proofs/, with the axiom audit: only propext,
   Classical.choice and Quot.sound.

The release workflow runs Checks first, then Stress and Proofs as parallel jobs
beside the release build, and publishes only when every stage it ran passed.

.EXAMPLE
pwsh -NoProfile -File scripts/release-gate.ps1

.EXAMPLE
pwsh -NoProfile -File scripts/release-gate.ps1 -Green <Rosen's last green commit>

.EXAMPLE
pwsh -NoProfile -File scripts/release-gate.ps1 -Stage Proofs
#>
param(
    # The repository whose release-blocker issues count, as owner/name. Empty
    # reads it from the checkout's origin through gh.
    [string]$Repository = '',
    # Rosen's last green commit covering the release commit, as Velocity's Cut
    # release passes it. Empty requires the status on the release commit itself.
    [string]$Green = '',
    # The stage to run; All runs every stage in order.
    [ValidateSet('All', 'Checks', 'Stress', 'Proofs')]
    [string]$Stage = 'All'
)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
# Vite's Windows path probe runs `net use`; with Java's bin folders ahead of
# System32 on PATH it never exits (#44), as in check.ps1. The Checks and Proofs
# jobs run on Linux, which has no SystemRoot.
if ($env:SystemRoot) {
    $env:Path = (Join-Path $env:SystemRoot 'System32') + [IO.Path]::PathSeparator + $env:Path
}

function Stop-Gate([string]$Reason) {
    Write-Host "RELEASE BLOCKED: $Reason" -ForegroundColor Red
    exit 1
}

function Invoke-Step([string]$Name, [scriptblock]$Command) {
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Stop-Gate "$Name (exit $LASTEXITCODE)"
    }
}

function Resolve-Repository {
    if ($Repository) {
        return $Repository
    }
    $resolved = gh repo view --json nameWithOwner --jq .nameWithOwner
    if ($LASTEXITCODE -ne 0) {
        Stop-Gate 'could not resolve the release repository'
    }
    return $resolved
}

# Whether $Commit's newest velocity/integration status is success. GitHub lists a
# context's newest status first.
function Test-IntegrationGreen([string]$Repo, [string]$Commit) {
    $states = @(gh api --paginate "repos/$Repo/commits/$Commit/statuses" --jq '.[] | select(.context == "velocity/integration") | .state')
    if ($LASTEXITCODE -ne 0) {
        Stop-Gate "could not read integration status on $Commit"
    }
    return ($states.Count -gt 0 -and $states[0] -ceq 'success')
}

# Stops the gate unless integration passed on the release commit or on the green
# commit covering it. Returns whether the release commit itself is green, which
# lets a beta release skip the stress and proof stages (#434).
function Confirm-Integration {
    $commit = git rev-parse --verify HEAD
    if ($LASTEXITCODE -ne 0 -or $commit -cnotmatch '^[0-9a-f]{40}$') {
        Stop-Gate 'could not resolve the release commit'
    }
    $repo = Resolve-Repository
    $checked = $commit
    if ($Green) {
        if ($Green -cnotmatch '^[0-9a-f]{40}$') {
            Stop-Gate "$Green is not a full commit ID"
        }
        # Exit 0: an ancestor (or the same commit). 1: not an ancestor. Other: unknown commit.
        git merge-base --is-ancestor $commit $Green
        if ($LASTEXITCODE -ne 0) {
            Stop-Gate "$commit is not covered by Rosen's green commit $Green"
        }
        $checked = $Green
    }
    if (-not (Test-IntegrationGreen $repo $checked)) {
        Stop-Gate "$checked has no green Velocity integration build"
    }
    if ($checked -ceq $commit) {
        Write-Host "==> Integration passed on $commit" -ForegroundColor Cyan
        return $true
    }
    Write-Host "==> Integration passed on Rosen's green commit $checked, which covers $commit" -ForegroundColor Cyan
    # Covered is enough for the full gate; the beta shortcut needs the commit's own status.
    return (Test-IntegrationGreen $repo $commit)
}

function Confirm-NoReleaseBlocker {
    Write-Host '==> No open release-blocker issue' -ForegroundColor Cyan
    $repositoryArguments = if ($Repository) { @('--repo', $Repository) } else { @() }
    $blockers = gh issue list @repositoryArguments --state open --label release-blocker --json number,title --limit 100 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) {
        Stop-Gate 'could not list release-blocker issues (gh)'
    }
    if ($blockers) {
        $list = ($blockers | ForEach-Object { "#$($_.number) $($_.title)" }) -join '; '
        Stop-Gate "open release-blocker issues: $list"
    }
}

function Confirm-Proofs {
    if (-not (Get-Command lake -ErrorAction SilentlyContinue)) {
        Stop-Gate 'lake is not installed. Run proofs/scripts/Setup-Toolchain.ps1.'
    }
    Write-Host '==> Proofs: lake build and axiom audit' -ForegroundColor Cyan
    Push-Location proofs
    lake build 2>&1 | Tee-Object -Variable lakeLines | Out-Host
    $lakeExit = $LASTEXITCODE
    Pop-Location
    if ($lakeExit -ne 0) {
        Stop-Gate "lake build (exit $lakeExit)"
    }
    # `#print axioms` lines read "'Name' depends on axioms: [a, b]" (AGENTS.md Proofs).
    $allowedAxioms = @('propext', 'Classical.choice', 'Quot.sound')
    $badAxioms = foreach ($line in $lakeLines) {
        if ("$line" -match 'depends on axioms: \[(.*)\]') {
            $Matches[1].Split(',').Trim() | Where-Object { $_ -and $_ -notin $allowedAxioms }
        }
    }
    if ($badAxioms) {
        Stop-Gate "disallowed axioms: $(($badAxioms | Sort-Object -Unique) -join ', ')"
    }
}

if ($Stage -in 'All', 'Checks') {
    $exactGreen = Confirm-Integration
    Confirm-NoReleaseBlocker
    if ($env:GITHUB_OUTPUT) {
        "exact-green=$($exactGreen.ToString().ToLower())" >> $env:GITHUB_OUTPUT
    }
}
if ($Stage -in 'All', 'Stress') {
    Invoke-Step 'Stress: cargo run -p stress -- --scripts' { cargo run -p stress -- --scripts }
}
if ($Stage -in 'All', 'Proofs') {
    Confirm-Proofs
}
Write-Host "Release gate passed ($Stage)." -ForegroundColor Green
exit 0
