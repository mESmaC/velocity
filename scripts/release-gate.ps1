<#
.SYNOPSIS
The release gate (SPEC.md F7 "The release gate", #381): what a commit must pass
before it is released.

.DESCRIPTION
On a Rosen project no approval gates a land, so the release is where everything
is held to the full bar. Run it on the release commit. It stops at the first
failure and exits non-zero:
1. A green Velocity integration build on the exact release commit.
2. No open issue labelled release-blocker (gh, on the checkout's repository).
3. cargo run -p stress -- --scripts: every stress script. --all would add the
   workspace tests, which the green integration build in step 1 already ran,
   and the UI build, which the release build itself runs (#498).
4. lake build in proofs/, with the axiom audit: only propext,
   Classical.choice and Quot.sound.

The release workflow checks integration before setting up its tools and runs
the full gate before building the release.

.EXAMPLE
pwsh -NoProfile -File scripts/release-gate.ps1
#>
param(
    # The repository whose release-blocker issues count, as owner/name. Empty
    # reads it from the checkout's origin through gh.
    [string]$Repository = '',
    [switch]$IntegrationOnly
)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
# Vite's Windows path probe runs `net use`; with Java's bin folders ahead of
# System32 on PATH it never exits (#44), as in check.ps1.
$env:Path = (Join-Path $env:SystemRoot 'System32') + ';' + $env:Path

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

function Confirm-Integration {
    $commit = git rev-parse --verify HEAD
    if ($LASTEXITCODE -ne 0 -or $commit -cnotmatch '^[0-9a-f]{40}$') {
        Stop-Gate 'could not resolve the release commit'
    }
    $repo = $Repository
    if (-not $repo) {
        $repo = gh repo view --json nameWithOwner --jq .nameWithOwner
        if ($LASTEXITCODE -ne 0) {
            Stop-Gate 'could not resolve the release repository'
        }
    }
    $states = @(gh api --paginate "repos/$repo/commits/$commit/statuses" --jq '.[] | select(.context == "velocity/integration") | .state')
    if ($LASTEXITCODE -ne 0) {
        Stop-Gate "could not read integration status on $commit"
    }
    if ($states.Count -eq 0 -or $states[0] -cne 'success') {
        Stop-Gate "$commit has no green Velocity integration build"
    }
    Write-Host "==> Integration passed on $commit" -ForegroundColor Cyan
}

Confirm-Integration
if ($IntegrationOnly) {
    exit 0
}

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

Invoke-Step 'Stress: cargo run -p stress -- --scripts' { cargo run -p stress -- --scripts }

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
Write-Host 'Release gate passed.' -ForegroundColor Green
exit 0
