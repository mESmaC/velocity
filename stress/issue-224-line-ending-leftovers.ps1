# Risk (#224): scripts/check.ps1 rewrites ui/src/lib/bindings.ts and
# proofs/fixtures/*.tsv with LF line endings. git status lists those files, so
# change detection took them for Rust and proof changes and ran every step on
# each later run. A line-ending-only rewrite must not count, a real change to
# the same files must, and an unrecognised path must still run everything.
# Updated for Rosen merge trees (#385): seed a throwaway Git fixture from the
# reviewed scripts, bindings and fixtures; the outer tree needs no Git metadata.
# Runs the same check.ps1 -Plan cases (detection only, no builds).
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Import-Module (Join-Path $PSScriptRoot 'git-fixture.psm1') -Force
$paths = @('.gitattributes', 'scripts/check.ps1', 'scripts/build-queue.ps1', 'ui/src/lib/bindings.ts') +
    @(Get-ChildItem -LiteralPath (Join-Path $repositoryRoot 'proofs/fixtures') -Filter '*.tsv' |
        ForEach-Object { "proofs/fixtures/$($_.Name)" })
$fixture = New-StressGitFixture $repositoryRoot $paths

function Invoke-Git { git -C $fixture @args; if ($LASTEXITCODE -ne 0) { throw "git $args failed" } }

# Runs the plan against HEAD and checks which steps it chose.
function Assert-Plan([string]$Case, [string]$Expected) {
    $output = pwsh -NoProfile -File (Join-Path $fixture 'scripts\check.ps1') -Base HEAD -Plan 2>&1 | Out-String
    $planLine = ($output -split "`r?`n" | Where-Object { $_ -like 'Plan:*' }) -join ''
    if ($planLine -ne "Plan: $Expected") { throw "$Case`: expected 'Plan: $Expected', got:`n$output" }
    Write-Host "PASS $Case`: $planLine"
}

# Rewrites a file with LF line endings and the same content, as the bindings export and lake build do.
function Write-WithLf([string]$RelativePath) {
    $path = Join-Path $fixture $RelativePath
    $text = [IO.File]::ReadAllText($path).Replace("`r`n", "`n")
    [IO.File]::WriteAllText($path, $text, [Text.UTF8Encoding]::new($false))
}

try {
    Assert-Plan 'clean worktree' 'rust=False ui=False proofs=False'

    Write-WithLf 'ui/src/lib/bindings.ts'
    Get-ChildItem -LiteralPath (Join-Path $fixture 'proofs\fixtures') -Filter '*.tsv' | ForEach-Object { Write-WithLf "proofs/fixtures/$($_.Name)" }
    if (-not (git -C $fixture status --porcelain)) { Write-Host '  note: git status shows no rewrite on this machine' }
    Assert-Plan 'line-ending-only rewrites' 'rust=False ui=False proofs=False'

    Add-Content -LiteralPath (Join-Path $fixture 'ui\src\lib\bindings.ts') -Value '// changed'
    Assert-Plan 'real change to bindings' 'rust=True ui=True proofs=False'
    Invoke-Git checkout --quiet -- ui/src/lib/bindings.ts

    Add-Content -LiteralPath (Join-Path $fixture 'proofs\fixtures\agent-activity.tsv') -Value 'changed'
    Assert-Plan 'real change to a fixture' 'rust=True ui=True proofs=True'
    Invoke-Git checkout --quiet -- proofs/fixtures

    New-Item -ItemType Directory -Path (Join-Path $fixture 'proofs\Scratch') | Out-Null
    Set-Content -LiteralPath (Join-Path $fixture 'proofs\Scratch\New.lean') -Value '-- new'
    Assert-Plan 'untracked proof file' 'rust=False ui=False proofs=True'

    New-Item -ItemType Directory -Path (Join-Path $fixture 'new folder') | Out-Null
    Set-Content -LiteralPath (Join-Path $fixture 'new folder\tool.txt') -Value 'new'
    Assert-Plan 'untracked unrecognised path' 'rust=True ui=True proofs=True'
    Write-Host 'All check.ps1 change detection cases passed.'
} finally {
    Remove-StressGitFixture $fixture
}
