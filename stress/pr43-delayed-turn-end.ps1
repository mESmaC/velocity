# Risk: queued steering and activity must agree with the ACP client even when
# native event delivery lags behind two UI sends (Rule 1, F4).
# Calls the real runtime and steering policy; no provider process is started.
# Adapted for #499: the harness is stress/harness/sources/pr43-delayed-turn-end.rs, built with every
# other Rust harness into the stress crate's stress-harness binary instead of a Cargo project
# this script wrote. Its source and assertions are unchanged.
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr43-delayed-turn-end
exit $LASTEXITCODE
