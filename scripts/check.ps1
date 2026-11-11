<#
.SYNOPSIS
Runs every local check PR duty needs before approving (AGENTS.md), in one command.

.DESCRIPTION
Run it on the merge result: the PR branch with the current master merged in.
It stops at the first failing check and exits non-zero.

It runs only the checks the change can affect, judged from the files changed
relative to -Base. The build queue is the swarm's bottleneck, so a change that
cannot affect the Rust build never enters it:
- Rust (format, locked build, crates alone, bindings, Clippy): crates/,
  src-tauri/, the stress crate, Cargo files, the toolchain, and the files Rust
  embeds at compile time (proof fixtures, theme tokens, bindings).
- UI (type check, lint, build): ui/, or any Rust change, since Rust generates
  the UI's bindings.
- Proofs (lake build, axiom audit): proofs/.
A docs-only change (SPEC.md, AGENTS.md, README, stress scripts) runs nothing.
A path the script does not recognise, such as this script, the build queue or a
new folder, runs everything. So does -Full.

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1 -Full
#>
param(
    # The branch the change detection compares against.
    [string]$Base = 'origin/master',
    # Run every check, whatever changed.
    [switch]$Full
)
$ErrorActionPreference = 'Stop'
$checkoutPath = Split-Path -Parent $PSScriptRoot
# pnpm resolves Windows drive aliases and junctions before launching Cargo.
# Use the same physical path for every check, or Cargo gives the same package
# two identities and recompiles it when the binding export runs (#74).
$physicalCheckoutPath = node -e "process.stdout.write(require('node:fs').realpathSync.native(process.argv[1]))" -- $checkoutPath
if ($LASTEXITCODE -ne 0 -or -not $physicalCheckoutPath) {
    throw "Could not resolve the physical checkout path: $checkoutPath"
}
Set-Location -LiteralPath $physicalCheckoutPath

# Vite's Windows path probe runs `net use`. With Java's bin folders ahead of
# System32 on PATH, `net` resolves to Java's net.dll and the build never exits
# (#44). Putting System32 first, for this process only, fixes it.
$env:Path = (Join-Path $env:SystemRoot 'System32') + ';' + $env:Path

# Cargo steps run in the machine-wide build queue, one build at a time (build-queue.ps1).
. (Join-Path $PSScriptRoot 'build-queue.ps1')

function Invoke-Check([string]$Name, [scriptblock]$Command) {
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name (exit $LASTEXITCODE)" -ForegroundColor Red
        exit 1
    }
}

$mergeBase = git merge-base $Base HEAD
if ($LASTEXITCODE -ne 0 -or -not $mergeBase) {
    throw "Could not find the merge base with $Base. Run git fetch origin first."
}
# Committed changes since the merge base plus uncommitted ones. A rename lists its new path.
$changedPaths = @(git diff --name-only $mergeBase) +
    @(git status --porcelain --untracked-files=all | ForEach-Object { ($_.Substring(3) -split ' -> ')[-1].Trim('"') }) |
    Where-Object { $_ } | Sort-Object -Unique

function Test-Changed([string[]]$Patterns) {
    foreach ($path in $changedPaths) {
        foreach ($pattern in $Patterns) {
            if ($path -like $pattern) { return $true }
        }
    }
    return $false
}

$rustPatterns = @(
    'crates/*', 'src-tauri/*', 'stress/Cargo.toml', 'stress/src/*',
    'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/*', 'rustfmt.toml', 'clippy.toml',
    # Read by include_str!/include_bytes!, or generated from Rust.
    'proofs/fixtures/*', 'ui/src/lib/theme/tokens.css', 'ui/src/lib/bindings.ts'
)
# Paths no check reads: docs, the read-only reference UI, the icon source, CI (which
# never holds up a PR) and stress scripts, which the stress runner compiles itself.
$uncheckedPatterns = @('*.md', 'design/*', 'assets/*', '.github/*', '.gitignore', 'stress/*')
# A path on none of the lists runs everything, this script and the build queue
# included. A new folder or root file is then checked until someone decides that
# no check can depend on it; skipping it by default would let a broken build land.
$knownPatterns = $rustPatterns + $uncheckedPatterns + @('ui/*', 'proofs/*')
$unrecognisedPaths = @(@($changedPaths) | Where-Object {
        $path = $_
        $path -and -not ($knownPatterns | Where-Object { $path -like $_ })
    })
if ($unrecognisedPaths.Count -gt 0) {
    Write-Host "==> Unrecognised paths, running every check: $($unrecognisedPaths -join ', ')" -ForegroundColor Cyan
}
$runAll = $Full -or $unrecognisedPaths.Count -gt 0
$rustChanged = $runAll -or (Test-Changed $rustPatterns)
$uiChanged = $rustChanged -or (Test-Changed @('ui/*'))
$proofsChanged = $runAll -or (Test-Changed @('proofs/*'))

if ($rustChanged -or $uiChanged) {
    if (-not (Test-Path -LiteralPath 'ui/node_modules')) {
        Invoke-Check 'Install UI dependencies' { pnpm -C ui install --frozen-lockfile }
    }
}
if ($rustChanged) {
    Invoke-Check 'Rust format' { cargo fmt --all --check }
    Invoke-InBuildQueue "check.ps1 cargo steps" {
        Invoke-Check 'Rust build (locked)' { cargo build --workspace --locked }
        # The workspace build turns on every crate's features together, so a crate that
        # only compiles with a feature another member enables still passes it. Stress
        # scripts and other crates build each crate alone, so check alone each crate this
        # branch changes. Checking all of them costs minutes in a fresh worktree.
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
    }
} else {
    Write-Host '==> Rust: no Rust inputs changed, skipped (no build queue)' -ForegroundColor Cyan
}
if ($uiChanged) {
    Invoke-Check 'UI type check' { pnpm -C ui check }
    Invoke-Check 'UI lint' { pnpm -C ui lint }
    Invoke-Check 'UI build' { pnpm -C ui build }
} else {
    Write-Host '==> UI: ui/ and Rust unchanged, skipped' -ForegroundColor Cyan
}

if (-not $proofsChanged) {
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
