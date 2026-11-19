# Risk: completed threads retain their branch identities. A new job must not
# reuse one when its old branch and worktree have already been deleted (F5).
# Adapted for #499: the harness is stress/harness/sources/pr43-history-name-exhaustion.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote. Its source and assertions are unchanged.
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr43-history-name-exhaustion
exit $LASTEXITCODE
