<#
.SYNOPSIS
Runs the local project checks in one command.

.DESCRIPTION
Run it on the merge result: a Rosen build tree or the PR branch with master merged in.
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
new folder, runs everything. So does -Full. In a tree without .git, pass
-ChangedPathsFile with the merge result's changed paths (one root-relative path
per line, including deletions). Without that list, every check runs. An empty
list explicitly means no paths changed. PowerShell 7 (pwsh) is required.

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1 -Full

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1 -Plan

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1 -Priority landing

.EXAMPLE
pwsh -NoProfile -File scripts/check.ps1 -ChangedPathsFile changed-paths.txt -Priority landing
#>
param(
    # The branch the change detection compares against.
    [string]$Base = 'origin/master',
    # Explicit change detection for a Rosen merge result, without Git metadata.
    [string]$ChangedPathsFile,
    # Run every check, whatever changed.
    [switch]$Full,
    # Print the changed paths and the checks they would run, then stop.
    [switch]$Plan,
    # Place checks in the project build queue (SPEC.md F5). Runs pass
    # landing; an agent's check before marking a PR ready keeps the default.
    [ValidateSet('landing', 'ready', 'normal')]
    [string]$Priority = 'ready'
)
$ErrorActionPreference = 'Stop'
if ($ChangedPathsFile) {
    # Resolve against the caller's directory before moving to the checked tree.
    $ChangedPathsFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ChangedPathsFile)
}
$checkoutPath = Split-Path -Parent $PSScriptRoot
# pnpm resolves Windows drive aliases and junctions before launching Cargo.
# Use the same physical path for every check, or Cargo gives the same package
# two identities and recompiles it when the binding export runs (#74).
$physicalCheckoutPath = node -e "process.stdout.write(require('node:fs').realpathSync.native(process.argv[1]))" -- $checkoutPath
if ($LASTEXITCODE -ne 0 -or -not $physicalCheckoutPath) {
    throw "Could not resolve the physical checkout path: $checkoutPath"
}
Set-Location -LiteralPath $physicalCheckoutPath
$gitCheckout = Test-Path -LiteralPath (Join-Path $physicalCheckoutPath '.git')

# Vite's Windows path probe runs `net use`. With Java's bin folders ahead of
# System32 on PATH, `net` resolves to Java's net.dll and the build never exits
# (#44). Putting System32 first, for this process only, fixes it.
$env:Path = (Join-Path $env:SystemRoot 'System32') + ';' + $env:Path

# Cargo steps run in the machine-wide build queue, one build at a time (build-queue.ps1).
. (Join-Path $PSScriptRoot 'build-queue.ps1')

# The bindings export and lake build write these files with LF line endings.
# Under core.autocrlf, git status then lists them as modified though git diff
# sees no change (#224). Put back the ones whose content is unchanged, so a run
# leaves the worktree as it found it. A real change shows in git diff and stays.
function Restore-LineEndingRewrites {
    if (-not $gitCheckout) { return }
    $rewritten = @(git -c core.quotePath=false status --porcelain --untracked-files=no -- ui/src/lib/bindings.ts proofs/fixtures |
            ForEach-Object { $_.Substring(3) })
    foreach ($path in $rewritten) {
        git diff --quiet -- $path
        if ($LASTEXITCODE -eq 0) { git checkout --quiet -- $path }
    }
}

function Exit-Check([int]$Code) {
    Restore-LineEndingRewrites
    exit $Code
}

function Invoke-Check([string]$Name, [scriptblock]$Command) {
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name (exit $LASTEXITCODE)" -ForegroundColor Red
        Exit-Check 1
    }
}

$changedPaths = @()
if ($Full) {
    # Full checks never need version-control metadata, even in a Git checkout.
} elseif ($ChangedPathsFile) {
    $changedPaths = @(Get-Content -LiteralPath $ChangedPathsFile -Encoding utf8 |
            Where-Object { $_ } | ForEach-Object { $_.Replace('\', '/') } | Sort-Object -Unique)
} elseif ($gitCheckout) {
    $mergeBase = git merge-base $Base HEAD
    if ($LASTEXITCODE -ne 0 -or -not $mergeBase) {
        throw "Could not find the merge base with $Base. Run git fetch origin first, or pass -Full or -ChangedPathsFile."
    }
    # Compare content after line-ending conversion (#224), including untracked
    # files. A rename lists its new path.
    $trackedPaths = @(git -c core.quotePath=false diff --name-only $mergeBase)
    if ($LASTEXITCODE -ne 0) { throw 'Could not list changed tracked paths.' }
    $untrackedPaths = @(git -c core.quotePath=false ls-files --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) { throw 'Could not list untracked paths.' }
    $changedPaths = @($trackedPaths + $untrackedPaths | Where-Object { $_ } | Sort-Object -Unique)
} else {
    # A duty folder is a merge tree, not a Rosen workspace or Git checkout.
    # Never infer an empty change from missing metadata, or borrow another tree.
    Write-Host '==> No Git metadata or changed-path list; running every check.' -ForegroundColor Cyan
    $Full = $true
}

function Test-Changed([string[]]$Patterns) {
    foreach ($path in $changedPaths) {
        foreach ($pattern in $Patterns) {
            if ($path -like $pattern) { return $true }
        }
    }
    return $false
}

$rustPatterns = @(
    'crates/*', 'src-tauri/*', 'stress/Cargo.toml', 'stress/src/*', 'stress/harness/build.rs',
    'stress/harness/spliced.rs', 'stress/harness/main.rs',
    'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/*', 'rustfmt.toml', 'clippy.toml',
    # Read by include_str!/include_bytes!, or generated from Rust.
    'proofs/fixtures/*', 'ui/src/lib/theme/tokens.css', 'ui/src/lib/bindings.ts'
)
# Paths no check reads: docs, the root license and notice files, the read-only reference
# UI, the icon source, CI (which never holds up a PR) and stress scripts, which the stress
# runner compiles itself.
$uncheckedPatterns = @('*.md', 'LICENSE*', 'NOTICE*', 'design/*', 'assets/*', '.github/*', '.gitignore', 'stress/*')
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
if ($Plan) {
    Write-Host "Changed paths: $(if ($changedPaths) { $changedPaths -join ', ' } else { 'none' })"
    Write-Host "Plan: rust=$rustChanged ui=$uiChanged proofs=$proofsChanged"
    exit 0
}

if ($rustChanged -or $uiChanged) {
    if (-not (Test-Path -LiteralPath 'ui/node_modules')) {
        Invoke-Check 'Install UI dependencies' { pnpm -C ui install --frozen-lockfile }
    }
}
if ($rustChanged) {
    Invoke-Check 'Rust format' { cargo fmt --all --check }
    Invoke-InBuildQueue "check.ps1 cargo steps ($Priority)" -Priority $Priority -Command {
        Invoke-Check 'Rust build (locked)' { cargo build --workspace --locked }
        # The workspace build turns on every crate's features together, so a crate that
        # only compiles with a feature another member enables still passes it. Stress
        # scripts and other crates build each crate alone, so check alone each crate this
        # branch changes. Checking all of them costs minutes in a fresh worktree.
        $changedCrates = (cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json).packages |
            Where-Object { $_.manifest_path -like '*crates*' } |
            Where-Object {
                if ($runAll) { return $true }
                $crateFolder = 'crates/' + (Split-Path -Leaf (Split-Path -Parent $_.manifest_path)) + '/'
                $changedPaths | Where-Object { $_.StartsWith($crateFolder) }
            }
        foreach ($crate in $changedCrates) {
            Invoke-Check "Builds alone: $($crate.name)" { cargo check -p $crate.name --locked }
        }
        # The exporter compares the supplied file to regenerated content, ignoring
        # CRLF, so stale bindings fail in both Git and Rosen trees.
        Invoke-Check 'IPC bindings match Rust' { cargo run --locked -p velocity-app --bin export-bindings -- --check }
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
    Exit-Check 0
}
if (-not (Get-Command lake -ErrorAction SilentlyContinue)) {
    Write-Host 'FAILED: proofs/ changed but lake is not installed. Run proofs/scripts/Setup-Toolchain.ps1.' -ForegroundColor Red
    Exit-Check 1
}
Write-Host '==> Proofs: lake build and axiom audit' -ForegroundColor Cyan
Push-Location proofs
lake build 2>&1 | Tee-Object -Variable lakeLines | Out-Host
$lakeExit = $LASTEXITCODE
Pop-Location
if ($lakeExit -ne 0) {
    Write-Host "FAILED: lake build (exit $lakeExit)" -ForegroundColor Red
    Exit-Check 1
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
    Exit-Check 1
}
Write-Host 'All checks passed.' -ForegroundColor Green
Exit-Check 0
