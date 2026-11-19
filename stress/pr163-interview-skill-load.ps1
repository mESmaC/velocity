$ErrorActionPreference = 'Stop'
# F4: interview tool steps lose skill identity when ACP supplies a path or raw input.
# Adapted for #499: the harness is stress/harness/sources/pr163-interview-skill-load.rs, built with
# every other Rust harness into the stress crate's stress-harness binary instead of a Cargo
# project this script wrote. Its source and assertions are unchanged.
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr163-interview-skill-load
exit $LASTEXITCODE
