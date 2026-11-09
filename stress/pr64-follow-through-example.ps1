# Risk: F9's saved issue association must not break the existing F6 entry point
# when every Cargo target is checked after merging the two features.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot 'target'
cargo check --manifest-path (Join-Path $repositoryRoot 'Cargo.toml') -p velocity-store --example follow_through --locked
exit $LASTEXITCODE
