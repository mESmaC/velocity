# Exercises the real F2 Tauri create_project command with a failed state write.
# Uses Tauri's mock event runtime and real command/store/layout code and filesystem
# paths. Only Tauri handle/window runtime type annotations change in generated
# copies; function bodies stay unchanged. This does not test native UI/IPC transport.
# Run from any directory: powershell -File stress/pr7-create-save.ps1
param()
$ErrorActionPreference = 'Stop'
$repository = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixture = Join-Path $repository ('.tools\pr7-create-save\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
$root = $repository.Replace('\', '/')
$manifest = @'
[package]
name = "velocity-pr7-create-save-review"
version = "0.1.0"
edition = "2024"
[workspace]
[dependencies]
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
specta = { version = "=2.0.0-rc.26", features = ["derive", "function"] }
tauri = { version = "=2.12.5", default-features = false, features = ["test", "specta"] }
tauri-specta = { version = "=2.0.0-rc.26", features = ["derive", "typescript"] }
tauri-plugin-dialog = "=2.8.3"
velocity-core = { path = "REPO_ROOT/crates/velocity-core", features = ["bindings"] }
velocity-store = { path = "REPO_ROOT/crates/velocity-store" }
velocity-git = { path = "REPO_ROOT/crates/velocity-git" }
velocity-github = { path = "REPO_ROOT/crates/velocity-github" }
'@
$source = @'
#![allow(dead_code)]
#[path = "../state.rs"] mod state;
#[path = "../layout.rs"] mod layout;
#[path = "../new_project.rs"] mod new_project;
OPTIONAL_PROJECT_MODULE
use std::{fs, path::PathBuf, sync::Mutex};
use tauri::Manager;

fn report(label: &str, passed: bool, failures: &mut u32) {
    println!("{} {label}", if passed { "PASS" } else { "FAIL" });
    if !passed { *failures += 1; }
}

fn main() {
    let fixture = PathBuf::from(std::env::args().nth(1).expect("fixture path"));
    let data = fixture.join("app data");
    let store = velocity_store::StateStore::load_or_default(&data).expect("initial state");
    let before = store.state().clone();
    let disk_before = fs::read(data.join("state.json")).expect("saved baseline");
    let blocker = data.join("state.json.partial");
    fs::create_dir(&blocker).expect("inject failed file creation");
    let events = tauri_specta::Builder::<tauri::test::MockRuntime>::new()
        .events(tauri_specta::collect_events![layout::LayoutStateChanged, layout::LayoutIntentRequested])
        OPTIONAL_PROJECT_EVENT;
    let application = tauri::test::mock_builder()
        .manage(Mutex::new(store))
        .manage(layout::MainLayout::default())
        .setup(move |app| { events.mount_events(app); Ok(()) })
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("Tauri mock event runtime");
    let settings = before.settings.providers.clone();
    let request = new_project::NewProjectRequest {
        location: new_project::ProjectLocationRequest::LocalFolder {
            path: fixture.join("new project").display().to_string(),
            create_if_needed: true,
        },
        model_name: settings.default_model,
        effort: settings.default_effort,
        speed: settings.default_speed,
        brief: "PR duty failed-save fixture".into(),
    };
    let first = tauri::async_runtime::block_on(new_project::create_project(
        application.handle().clone(), request.clone()));
    println!("First Create: {first:?}");
    let unchanged = {
        let managed = application.state::<state::ManagedStateStore>();
        let guard = managed.lock().expect("store lock");
        println!("Projects after failed Create: {}", guard.state().projects.len());
        guard.state() == &before
    };
    let mut failures = 0;
    report("injected save failure is reported", first.is_err(), &mut failures);
    report("failed Create preserves the live project state", unchanged, &mut failures);
    report("failed Create preserves the saved snapshot",
        fs::read(data.join("state.json")).expect("read snapshot") == disk_before, &mut failures);
    fs::remove_dir(&blocker).expect("remove the empty injected blocker");
    let retry = tauri::async_runtime::block_on(new_project::create_project(
        application.handle().clone(), request));
    println!("Retry after repairing state path: {retry:?}");
    report("Create can be retried after the save path is repaired", retry.is_ok(), &mut failures);
    println!("Fixture retained at {}", fixture.display());
    if failures != 0 { std::process::exit(1); }
}
'@
$projectModule = ''
$projectEvent = ''
if (Test-Path -LiteralPath (Join-Path $repository 'src-tauri/src/projects.rs')) {
    $projectModule = '#[path = "../projects.rs"] mod projects;'
    $projectEvent = '.events(tauri_specta::collect_events![projects::ProjectSelected])'
}
$source = $source.Replace('OPTIONAL_PROJECT_MODULE', $projectModule).Replace('OPTIONAL_PROJECT_EVENT', $projectEvent).Replace('REPO_ROOT', $root)
$manifest = $manifest.Replace('REPO_ROOT', $root)
$encoding = New-Object System.Text.UTF8Encoding($false)
New-Item -ItemType Directory -Path (Join-Path $fixture 'src') -Force | Out-Null
[System.IO.File]::WriteAllText((Join-Path $fixture 'Cargo.toml'), $manifest, $encoding)
[System.IO.File]::WriteAllText((Join-Path $fixture 'src/main.rs'), $source, $encoding)
foreach ($module in @('state', 'layout', 'new_project', 'projects')) {
    $modulePath = Join-Path $repository "src-tauri/src/$module.rs"
    if (!(Test-Path -LiteralPath $modulePath)) { continue }
    $moduleText = [System.IO.File]::ReadAllText($modulePath)
    $import = [regex]::Match($moduleText, 'use tauri::\{([^}]+)\};')
    if (!$import.Success) { throw "Tauri import changed in $module; review the runtime adapter" }
    $names = @($import.Groups[1].Value.Split(',') | ForEach-Object { $_.Trim() })
    $remaining = @($names | Where-Object { $_ -notin @('AppHandle', 'Window') })
    $adapted = 'use tauri::{' + ($remaining -join ', ') + '};'
    foreach ($name in @('AppHandle', 'Window')) {
        if ($name -in $names) { $adapted += " type $name = tauri::$name<tauri::test::MockRuntime>;" }
    }
    $moduleText = $moduleText.Replace($import.Value, $adapted)
    [System.IO.File]::WriteAllText((Join-Path $fixture "$module.rs"), $moduleText, $encoding)
}
Copy-Item -LiteralPath (Join-Path $repository 'Cargo.lock') -Destination (Join-Path $fixture 'Cargo.lock')
Write-Output "Harness: $fixture"
cargo run --offline --manifest-path (Join-Path $fixture 'Cargo.toml') -- $fixture
exit $LASTEXITCODE
