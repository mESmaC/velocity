# Risk: steering queued while worktree setup is running must remain visibly
# queued, and its following turn must not be published as idle (Rule 1, F4).
# Uses the actual client steering policy and the new thread runtime projection.
# Adapted for #499: the harness is stress/harness/sources/pr43-queued-startup.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote. Its source and assertions are unchanged.
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr43-queued-startup
exit $LASTEXITCODE
