# Risk: abandoning the original Rosen change before the deferred state writer saves
# its replacement strands the task on an abandoned change after a crash. Reuses the
# change's real three-member smoke scenario and stops at that exact handoff boundary.
# Adapted for #499: this script used to patch crates/velocity-threads/examples/rosen_goal_swarm.rs
# and build the copy as its own Cargo project. stress/harness/spliced.rs now applies the same
# three edits when the stress crate's stress-harness binary is built: return before the event
# stream, save and defer saves before the stacked handoff, and assert the recovered change
# before the abandonment. If either needle moved, this harness fails with the same "moved;
# update this evidence without weakening it" message. Assertions unchanged.
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr4-goal-stack-crash
exit $LASTEXITCODE
