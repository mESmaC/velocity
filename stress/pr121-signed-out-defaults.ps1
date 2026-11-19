# Adapted for #499: the harness is stress/harness/sources/pr121-signed-out-defaults.rs, built with
# every other Rust harness into the stress crate's stress-harness binary instead of a Cargo
# project this script wrote. Its source and assertions are unchanged.
$ErrorActionPreference = 'Stop'
$fixture = Join-Path $env:TEMP ('velocity-pr121-signout-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr121-signed-out-defaults (Join-Path $fixture 'state')
exit $LASTEXITCODE
