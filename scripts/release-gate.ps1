<#
.SYNOPSIS
The release gate (SPEC.md F7 "The release gate", #381): what a commit must pass
before it is released.

.DESCRIPTION
On a Rosen project no approval gates a land, so the release is where everything
is held to the full bar. Run it on the release commit. It stops at the first
failure and exits non-zero:
1. No open issue labelled release-blocker (gh, on the checkout's repository).
2. The post-land full tests: the [checks.post_land] commands of rosen.toml.
3. cargo run -p stress -- --all.
4. lake build in proofs/, with the axiom audit: only propext,
   Classical.choice and Quot.sound.

The release workflow runs it before it builds anything, so a tag that fails it
publishes nothing.

.EXAMPLE
pwsh -NoProfile -File scripts/release-gate.ps1
#>
param(
    # The repository whose release-blocker issues count, as owner/name. Empty
    # reads it from the checkout's origin through gh.
    [string]$Repository = ''
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

# The commands under [checks.post_land] in rosen.toml: the one array of
# one-line strings Rosen reads there (Rosen SPEC §2 "Project configuration").
function Get-PostLandCommands {
    $text = Get-Content -Raw -LiteralPath 'rosen.toml'
    $section = [regex]::Match($text, '(?ms)^\[checks\.post_land\]\s*$(.*?)(?=^\[|\z)')
    if (-not $section.Success) {
        return @()
    }
    $array = [regex]::Match($section.Groups[1].Value, '(?ms)^commands\s*=\s*\[(.*?)\]')
    if (-not $array.Success) {
        return @()
    }
    [regex]::Matches($array.Groups[1].Value, '"((?:[^"\\]|\\.)*)"') |
        ForEach-Object { $_.Groups[1].Value -replace '\\"', '"' -replace '\\\\', '\' }
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

$postLand = @(Get-PostLandCommands)
if ($postLand.Count -eq 0 -and (Test-Path -LiteralPath 'rosen.toml')) {
    Write-Host '==> Post-land tests: rosen.toml names none' -ForegroundColor Cyan
}
foreach ($command in $postLand) {
    # Each is one line that runs the same from cmd, PowerShell and sh (rosen.toml).
    Invoke-Step "Post-land: $command" { Invoke-Expression $command }
}

Invoke-Step 'Stress: cargo run -p stress -- --all' { cargo run -p stress -- --all }

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
