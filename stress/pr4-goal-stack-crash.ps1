# Risk: abandoning the original Rosen change before the deferred state writer saves
# its replacement strands the task on an abandoned change after a crash. Reuses the
# change's real three-member smoke scenario and stops at that exact handoff boundary.
$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$harnessRoot = Join-Path $repositoryRoot '.tools/pr4-goal-stack-crash'
New-Item -ItemType Directory -Path $harnessRoot -Force | Out-Null
$source = Get-Content -Raw -Encoding UTF8 (Join-Path $repositoryRoot 'crates/velocity-threads/examples/rosen_goal_swarm.rs')
$source = $source.Replace('    let mut events = EventStream::open(server, 0).await?;', '    return Ok(());' + "`n" + '    #[allow(unreachable_code)]' + "`n" + '    let mut events = EventStream::open(server, 0).await?;')
$needle = "    store`n        .lock()`n        .map_err(|e| e.to_string())?`n        .update_thread(1, members[1].thread, |saved| {"
$source = $source.Replace("`r`n", "`n")
if (-not $source.Contains($needle)) { throw 'Smoke stack handoff moved; update this evidence without weakening it' }
$source = $source.Replace($needle, '    velocity_store::save_pending(store)?;' + "`n" + '    store.lock().map_err(|e| e.to_string())?.defer_saves();' + "`n" + $needle)
$needle = '    members[1].workspace = workspace;'
if (-not $source.Contains($needle)) { throw 'Smoke abandonment moved; update this evidence without weakening it' }
$assertion = @'
    let directory = original.root.parent().ok_or("missing project folder")?
        .parent().ok_or("missing workspace root")?
        .parent().ok_or("missing fixture root")?.join("state");
    let recovered = StateStore::load_or_default(directory)?;
    let saved = recovered.project(1).ok_or("missing recovered project")?
        .threads.iter().find(|thread| thread.id == members[1].thread)
        .ok_or("missing recovered thread")?;
    println!("after abandon: original_exists={}, recovered_change={:?}, child={}",
        original.root.exists(), saved.rosen_change, workspace.change);
    if saved.rosen_change.as_deref() != Some(workspace.change.as_str()) {
        return Err("FAIL: crash restores abandoned source instead of the live stacked child".into());
    }
'@
$source = $source.Replace($needle, $assertion + "`n" + $needle)
$dependencies = @('velocity-airc', 'velocity-core', 'velocity-git', 'velocity-rosen', 'velocity-store', 'velocity-threads') | ForEach-Object {
    $_ + ' = { path = "' + (Join-Path $repositoryRoot ('crates/' + $_)).Replace('\', '/') + '" }'
}
$manifest = @'
[package]
name = "pr4-goal-stack-crash"
version = "0.0.0"
edition = "2024"
[workspace]
[[bin]]
name = "pr4-goal-stack-crash"
path = "main.rs"
[dependencies]
tokio = { version = "1.54", features = ["rt", "sync", "time"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
'@ + "`n" + ($dependencies -join "`n") + "`n[profile.dev]`ndebug = `"line-tables-only`"`n[profile.dev.package.`"*`"]`ndebug = false`n"
[IO.File]::WriteAllText((Join-Path $harnessRoot 'main.rs'), $source)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot 'target'
cargo run --offline --manifest-path (Join-Path $harnessRoot 'Cargo.toml')
exit $LASTEXITCODE
