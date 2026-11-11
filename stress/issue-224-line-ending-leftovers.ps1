# Risk (#224): scripts/check.ps1 rewrites ui/src/lib/bindings.ts and
# proofs/fixtures/*.tsv with LF line endings. git status lists those files, so
# change detection took them for Rust and proof changes and ran every step on
# each later run. A line-ending-only rewrite must not count, a real change to
# the same files must, and an unrecognised path must still run everything.
# Runs check.ps1 -Plan (detection only, no builds) in a throwaway shared clone.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixture = Join-Path ([IO.Path]::GetTempPath()) "velocity-issue-224-$PID"
$headCommit = git -C $repositoryRoot rev-parse HEAD

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
    git -c core.autocrlf=true clone --quiet --shared --no-checkout $repositoryRoot $fixture
    Invoke-Git config core.autocrlf true
    Invoke-Git checkout --quiet --detach $headCommit
    # Test this checkout's own scripts, whatever is committed.
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'scripts\check.ps1') -Destination (Join-Path $fixture 'scripts\check.ps1') -Force
    Invoke-Git -c user.name=stress -c user.email=stress@example.invalid commit --quiet --allow-empty -am 'scripts under test'
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
    if (Test-Path -LiteralPath $fixture) { Remove-Item -LiteralPath $fixture -Recurse -Force }
}
