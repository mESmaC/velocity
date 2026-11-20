# Risk: the advertised review-mode change can sit named forever in Velocity.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$runner = Get-Content -Raw 'crates/velocity-threads/src/landing_runner.rs'
$notice = Get-Content -Raw 'ui/src/lib/RosenStatusPane.svelte'
$policy = Get-Content -Raw 'crates/velocity-rosen/src/post_land.rs'
$core = Get-Content -Raw 'crates/velocity-core/src/landing.rs'
if ($notice -notmatch 'Switch to <code>review = "after-land"</code>') {
    Write-Output 'PASS: The status pane no longer advertises this switch procedure.'
    exit 0
}

$baseLookup = 'after_land_at\(server, None\)\.await'
$baseGate = 'velocity_rosen::post_land::after_land\(&server\)\.await,\s*Ok\(true\)'
$inlineGate = "if matches!\(\s*$baseGate\s*\)\s*\{"
$fixtureGate = ($runner -match "let after_land = matches!\(\s*$baseGate") -and
    ($runner -match 'landing_request\(after_land, named\(thread\), thread\.rosen_change\.as_deref\(\)\)') -and
    ($core -match 'if after_land && named\s*\{\s*change\s*\}\s*else\s*\{\s*None\s*\}')
$attempts = [regex]::Matches($runner, 'runner\.land\(&host, &server, &project, thread, change\)').Count
if (($policy -match $baseLookup) -and ($attempts -eq 1) -and
    (($runner -match $inlineGate) -or $fixtureGate)) {
    Write-Output 'FAIL: The named-change runner gates every land request on the current base being after-land.'
    Write-Output 'With before-land on base, even a named rosen.toml-only switch never reaches Rosen.'
    exit 1
}
Write-Output 'PASS: The documented switch is no longer blocked by this runner gate.'
