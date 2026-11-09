# Risk: with a thread open, Settings must stay open while saved state refreshes the main
# window (an agent's status change, a deletion, a project switch all call refresh_layout).
# A refresh that forces the selected thread's view back would make Settings unusable
# while any agent runs (§5 navigation, F13). Uses the real store and layout reducer;
# no agent, worktree or provider is started.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixtureRoot = Join-Path $repositoryRoot ('.tools\pr54-settings-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$harnessRoot = Join-Path $fixtureRoot 'harness'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$storePath = (Join-Path $repositoryRoot 'crates\velocity-store').Replace('\', '/')
$corePath = (Join-Path $repositoryRoot 'crates\velocity-core').Replace('\', '/')
$manifest = @"
[package]
name = "pr54-settings-stays-open"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-store = { path = "$storePath" }
velocity-core = { path = "$corePath", features = ["bindings"] }
"@
$source = @'
use std::path::PathBuf;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent};
use velocity_core::navigation::{NavigationIntent, NavigationView};
use velocity_store::threads::NewThread;
use velocity_store::{EffortLevel, NewProjectRecord, ProjectLocationKind, ProviderKind, SpeedMode, StateStore};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let checkout = root.join("project");
    std::fs::create_dir_all(&checkout).expect("project folder");
    let mut store = StateStore::load_or_default(root.join("state")).expect("fresh store");
    let project = store.commit_new_project(NewProjectRecord {
        name: "project".into(), location_kind: ProjectLocationKind::Local,
        repository_path: checkout.to_string_lossy().into_owned(), default_branch: "master".into(),
        github_remote: None }, None, true).expect("project");
    let thread = store.add_thread(project, NewThread {
        title: "build thread".into(), branch_name: "vel/build-thread".into(),
        worktree_path: root.join("worktree").to_string_lossy().into_owned(),
        provider: ProviderKind::Claude, model_name: "agent default".into(), model_supports_fast: false,
        effort: EffortLevel::High, preferred_speed: SpeedMode::Regular }).expect("thread");
    let mut layout = LayoutState::default();
    store.refresh_layout(&mut layout);
    layout.select_thread(usize::try_from(thread).expect("identifier"));
    let mut failures = report("thread-open", layout.snapshot().view == NavigationView::Thread,
        format!("view={:?}", layout.snapshot().view));
    layout.apply(LayoutAction::Request(LayoutIntent::Navigation(NavigationIntent::Settings)));
    failures += report("settings-open", layout.snapshot().view == NavigationView::Settings,
        format!("view={:?}", layout.snapshot().view));
    // What every saved-state change does next: an agent status update, for example.
    store.refresh_layout(&mut layout);
    failures += report("settings-survives-refresh", layout.snapshot().view == NavigationView::Settings,
        format!("expected=Settings, actual={:?}", layout.snapshot().view));
    println!("failures: {failures}");
    std::process::exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot '.tools\pr54-target'
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE
