# Legacy entry point for the F2 failed-save and retry reproducer.
# The implementation moved to velocity-new-project; use its existing harness
# rather than copying Tauri modules and chasing unrelated runtime dependencies.
# This exercises the real executor/store/git/filesystem, not native UI or IPC.
param()
$ErrorActionPreference = 'Stop'
& (Join-Path $PSScriptRoot 'pr7-create-save-crate.ps1')
exit $LASTEXITCODE
