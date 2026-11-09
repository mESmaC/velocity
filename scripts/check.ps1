<#
.SYNOPSIS
Runs every local check PR duty needs before approving (AGENTS.md), in one command.

.DESCRIPTION
Run it on the merge result: the PR branch with the current master merged in.
It stops at the first failing check and exits non-zero. The proof check runs
only when the checkout changes proofs/ relative to -Base, so most PRs never
need Lean installed.

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1
#>
param(
    # The branch the proofs/ change detection compares against.
    [string]$Base = 'origin/master'
)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)

# Vite's Windows path probe runs `net use`. With Java's bin folders ahead of
# System32 on PATH, `net` resolves to Java's net.dll and the build never exits
# (#44). Putting System32 first, for this process only, fixes it.
$env:Path = (Join-Path $env:SystemRoot 'System32') + ';' + $env:Path

function Invoke-Check([string]$Name, [scriptblock]$Command) {
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name (exit $LASTEXITCODE)" -ForegroundColor Red
        exit 1
    }
}

if (-not (Test-Path -LiteralPath 'ui/node_modules')) {
    Invoke-Check 'Install UI dependencies' { pnpm -C ui install --frozen-lockfile }
}
Invoke-Check 'Rust format' { cargo fmt --all --check }
Invoke-Check 'Rust build (locked)' { cargo build --workspace --locked }
# The workspace build turns on every crate's features together, so a crate that
# only compiles with a feature another member enables still passes it. Stress
# scripts and other crates build each crate alone, so check alone each crate this
# branch changes. Checking all of them costs minutes in a fresh worktree.
$mergeBase = git merge-base $Base HEAD
$changedPaths = @(git diff --name-only $mergeBase) + @(git status --porcelain | ForEach-Object { $_.Substring(3) })
$changedCrates = (cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json).packages |
    Where-Object { $_.manifest_path -like '*crates*' } |
    Where-Object {
        $crateFolder = 'crates/' + (Split-Path -Leaf (Split-Path -Parent $_.manifest_path)) + '/'
        $changedPaths | Where-Object { $_.StartsWith($crateFolder) }
    }
foreach ($crate in $changedCrates) {
    Invoke-Check "Builds alone: $($crate.name)" { cargo check -p $crate.name --locked }
}
Invoke-Check 'Regenerate IPC bindings' { pnpm -C ui bindings }
Invoke-Check 'Bindings are committed' { git diff --exit-code -- ui/src/lib/bindings.ts }
Invoke-Check 'Clippy' { cargo clippy --workspace --all-targets --locked -- -D warnings }
Invoke-Check 'UI type check' { pnpm -C ui check }
Invoke-Check 'UI lint' { pnpm -C ui lint }
Invoke-Check 'UI build' { pnpm -C ui build }

$proofChanges = @(git diff --name-only $mergeBase -- proofs) + @(git status --porcelain -- proofs)
if (-not ($proofChanges -join '').Trim()) {
    Write-Host '==> Proofs: proofs/ unchanged, skipped' -ForegroundColor Cyan
    Write-Host 'All checks passed.' -ForegroundColor Green
    exit 0
}
if (-not (Get-Command lake -ErrorAction SilentlyContinue)) {
    Write-Host 'FAILED: proofs/ changed but lake is not installed. Run proofs/scripts/Setup-Toolchain.ps1.' -ForegroundColor Red
    exit 1
}
Write-Host '==> Proofs: lake build and axiom audit' -ForegroundColor Cyan
Push-Location proofs
lake build 2>&1 | Tee-Object -Variable lakeLines | Out-Host
$lakeExit = $LASTEXITCODE
Pop-Location
if ($lakeExit -ne 0) {
    Write-Host "FAILED: lake build (exit $lakeExit)" -ForegroundColor Red
    exit 1
}
# `#print axioms` lines read "'Name' depends on axioms: [a, b]". Only these three are allowed.
$allowedAxioms = @('propext', 'Classical.choice', 'Quot.sound')
$badAxioms = foreach ($line in $lakeLines) {
    if ("$line" -match 'depends on axioms: \[(.*)\]') {
        $Matches[1].Split(',').Trim() | Where-Object { $_ -and $_ -notin $allowedAxioms }
    }
}
if ($badAxioms) {
    Write-Host "FAILED: disallowed axioms: $(($badAxioms | Sort-Object -Unique) -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host 'All checks passed.' -ForegroundColor Green
