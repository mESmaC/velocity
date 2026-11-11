# Risk: F5 launch planning must use persisted native worktree preferences,
# including default/override roots, setup fields and collision checks.
# This probes the revised planner API; the original reproducer stays unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr43-effective-worktree-settings'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$threadsPath = (Join-Path $repositoryRoot 'crates\velocity-threads').Replace('\', '/')
$storePath = (Join-Path $repositoryRoot 'crates\velocity-store').Replace('\', '/')
$manifest = @"
[package]
name = "pr43-effective-worktree-settings"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-threads = { path = "$threadsPath" }
velocity-store = { path = "$storePath" }
"@
$source = @'
use std::{fs, path::{Path, PathBuf}, process::{Command, exit}};
use velocity_store::{NewProjectRecord, ProjectLocationKind, StateStore};
use velocity_threads::{LaunchPlan, plan_launch_with_preferences as plan_launch};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn plan(store: &StateStore) -> LaunchPlan {
    let context = store.worktree_context();
    plan_launch(store.active_project().expect("active project"), &context.preferences,
        &store.settings(), "check settings").expect("native planner path")
}

fn saved_preferences(root: &Path, state_path: &Path, mut store: StateStore) -> usize {
    let identifier = store.active_project().expect("active project").id;
    let project_root = root.join("saved project override");
    let mut settings = store.settings();
    settings.worktrees.location_root = Some(root.join("saved global root").to_string_lossy().into_owned());
    settings.worktrees.project_roots.insert(identifier, project_root.to_string_lossy().into_owned());
    settings.worktrees.copy_files = "saved.config, config with spaces.json".into();
    settings.worktrees.setup_command = "echo saved setup".into();
    settings.worktrees.share_rust_build_cache = false;
    store.set_settings(settings).expect("save settings");
    let mut store = StateStore::load_or_default(state_path).expect("reload settings");
    let settings = store.settings();
    let planned = plan(&store);
    let mut failures = report("saved-project-override", planned.worktree.worktree_path.parent() == Some(project_root.as_path()),
        format!("path={}", planned.worktree.worktree_path.display()));
    failures += report("saved-copy-files", planned.worktree.untracked_files == settings.worktrees.copy_files,
        &planned.worktree.untracked_files);
    failures += report("saved-setup-command", planned.worktree.setup_command == settings.worktrees.setup_command,
        &planned.worktree.setup_command);
    failures += report("disabled-shared-cache", !planned.worktree.settings.share_build_cache
        && !planned.agent.env.iter().any(|(key, _)| key == "CARGO_TARGET_DIR"), "cache toggle reaches the agent");
    fs::create_dir_all(project_root.join("check-settings")).expect("isolated collision directory");
    let collision = plan(&store);
    failures += report("collision-in-effective-project-root", collision.thread.branch_name == "vel/check-settings-2"
        && collision.worktree.worktree_path == project_root.join("check-settings-2"), &collision.thread.branch_name);
    let mut cleared = store.settings();
    cleared.worktrees.copy_files.clear();
    cleared.worktrees.setup_command.clear();
    store.set_settings(cleared).expect("save cleared fields");
    let cleared = plan(&store);
    failures += report("cleared-fields-skip-setup-steps", !cleared.worktree.settings.copy_untracked_files
        && !cleared.worktree.settings.run_setup_command, "empty lists do not fall back to defaults");
    failures
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let repository = root.join("repository with spaces");
    fs::create_dir_all(&repository).expect("fixture repository");
    assert!(Command::new("git").args(["init", "--initial-branch", "master"])
        .arg(&repository).output().expect("git init").status.success());
    let state_path = root.join("saved state");
    let mut store = StateStore::load_or_default(&state_path).expect("fresh store");
    store.commit_new_project(NewProjectRecord {
        name: "display project".into(), location_kind: ProjectLocationKind::Local,
        repository_path: repository.to_string_lossy().into_owned(), default_branch: "master".into(),
        github_remote: None,
    }, None, true).expect("register project");
    let default_root = state_path.join("worktrees").join("display project");
    let planned = plan(&store);
    let mut failures = report("default-root-matches-settings-preview", planned.worktree.worktree_path.parent() == Some(default_root.as_path()),
        format!("path={}", planned.worktree.worktree_path.display()));
    failures += report("shared-cache-uses-effective-root", planned.worktree.shared_cargo_target_dir == default_root.join(".cargo-target"),
        planned.worktree.shared_cargo_target_dir.display());
    failures += saved_preferences(&root, &state_path, store);
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot '.tools\pr43-target'
$fixtureRoot = Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE
