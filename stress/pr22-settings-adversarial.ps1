param([string]$TargetDirectory = $env:CARGO_TARGET_DIR)

# Risks: settings lost on reload/save failure, stale project edits, and Windows inventory aliases.
# Calls the actual core, store and git crates. Every fixture stays under this review checkout.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixtureRoot = Join-Path $repositoryRoot ('.tools\pr22-fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$harnessRoot = Join-Path $fixtureRoot 'harness'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
if (-not $TargetDirectory) { $TargetDirectory = Join-Path $repositoryRoot '.tools\pr22-target' }
$env:CARGO_TARGET_DIR = $TargetDirectory
$corePath = (Join-Path $repositoryRoot 'crates\velocity-core').Replace('\', '/')
$storePath = (Join-Path $repositoryRoot 'crates\velocity-store').Replace('\', '/')
$gitPath = (Join-Path $repositoryRoot 'crates\velocity-git').Replace('\', '/')
$manifest = @"
[package]
name = "pr22-settings-adversarial"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-core = { path = "$corePath" }
velocity-store = { path = "$storePath" }
velocity-git = { path = "$gitPath" }
serde_json = "1.0"
"@
$source = @'
use std::{fs, path::{Path, PathBuf}, process::{Command, exit}, sync::atomic::AtomicBool};
use velocity_core::{layout_state::LayoutState, worktree_view::{WorktreeSettingsEdit as Edit, WorktreeOwner},
    worktree_settings::WorktreeDisplayStatus as Status};
use velocity_git::inventory::{InventoryError, WorktreeInventory, measure_worktree, read_drives};
use velocity_store::{StateStore, ProjectLocationKind, NewProjectRecord};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn register(store: &mut StateStore, root: &Path, name: &str) -> u32 {
    store.commit_new_project(NewProjectRecord {
        name: name.into(), location_kind: ProjectLocationKind::Local,
        repository_path: root.join(name).to_string_lossy().into_owned(),
        default_branch: "master".into(), github_remote: None,
    }, None, true).expect("register isolated project")
}

fn persistence(root: &Path) -> (StateStore, usize) {
    let directory = root.join("saved state with spaces");
    let mut store = StateStore::load_or_default(&directory).expect("fresh store");
    let first = register(&mut store, root, "first project");
    let mut global = root.join("long root with spaces");
    while global.to_string_lossy().chars().count() < 290 { global.push("long folder with spaces"); }
    let global = global.to_string_lossy().into_owned();
    assert!(global.chars().count() > 260);
    store.edit_worktree_settings(Edit::Root(global.clone()), Some(first)).expect("long absolute root");
    store.edit_worktree_settings(Edit::ProjectRoot(root.join("override one").to_string_lossy().into()), Some(first)).expect("first override");
    let second = register(&mut store, root, "second project");
    store.edit_worktree_settings(Edit::ProjectRoot(root.join("override two").to_string_lossy().into()), Some(second)).expect("second override");
    store.edit_worktree_settings(Edit::CopyFiles(r".env, config\local.json".into()), Some(second)).expect("copy paths");
    store.edit_worktree_settings(Edit::SetupCommand(r#"pnpm -C "ui folder" install"#.into()), Some(second)).expect("quoted setup");
    store.edit_worktree_settings(Edit::ShareBuildCache(false), Some(second)).expect("cache setting");
    store.edit_worktree_settings(Edit::DeleteLocalBranch(false), Some(second)).expect("branch setting");
    store.edit_worktree_settings(Edit::ReadyCount(7), Some(second)).expect("ready count");
    let reloaded = StateStore::load_or_default(&directory).expect("reload settings");
    let mut failures = report("save-load-all-preferences", store.settings() == reloaded.settings(),
        format!("long root length={}, overrides={}", global.chars().count(), reloaded.settings().worktrees.project_roots.len()));

    let before = serde_json::to_vec(store.state()).expect("snapshot");
    let disk = fs::read(directory.join("state.json")).expect("saved bytes");
    let stale = store.edit_worktree_settings(Edit::OverrideEnabled(false), Some(first));
    failures += report("stale-project-edit-rejected", stale.is_err()
        && before == serde_json::to_vec(store.state()).expect("snapshot")
        && disk == fs::read(directory.join("state.json")).expect("disk bytes"), format!("{stale:?}"));
    let blocked = directory.join("state.json.partial");
    fs::create_dir(&blocked).expect("save-failure injection");
    let failed = store.edit_worktree_settings(Edit::ReadyCount(9), Some(second));
    failures += report("failed-save-preserves-memory-and-disk", failed.is_err()
        && before == serde_json::to_vec(store.state()).expect("snapshot")
        && disk == fs::read(directory.join("state.json")).expect("disk bytes"), format!("{failed:?}"));
    fs::remove_dir(&blocked).expect("remove only empty injected blocker");
    let retried = store.edit_worktree_settings(Edit::ReadyCount(9), Some(second));
    let reloaded = StateStore::load_or_default(&directory).expect("reload after retry");
    failures += report("retry-after-save-repair", retried.is_ok()
        && store.settings() == reloaded.settings() && reloaded.settings().worktrees.warm_pool_count == 9,
        format!("{retried:?}"));
    let mut layout = LayoutState::default();
    store.switch_project_layout(&mut layout, first).expect("switch first");
    let first_override = store.worktree_context().preferences.project_override;
    store.switch_project_layout(&mut layout, second).expect("switch second");
    let second_override = store.worktree_context().preferences.project_override;
    failures += report("project-overrides-remain-separate", first_override != second_override
        && first_override.is_some() && second_override.is_some(), "project switch preserved both roots");
    (store, failures)
}

fn junction(link: &Path, destination: &Path) {
    let output = Command::new("cmd.exe").args(["/d", "/c", "mklink", "/J"])
        .arg(link).arg(destination).output().expect("junction command");
    assert!(output.status.success(), "junction fixture: {}", String::from_utf8_lossy(&output.stderr));
}

fn owner(identifier: u32, path: &Path, status: Status) -> WorktreeOwner {
    WorktreeOwner { identifier, title: format!("owner-{identifier}"),
        path: path.to_string_lossy().into_owned(), status, fixing: false }
}

fn inventory(root: &Path, store: StateStore) -> usize {
    let working = root.join("\u{00c5}ngstr\u{00f6}m working tree");
    let failed = root.join("failed tree");
    let closed = root.join("closed tree");
    for path in [&working, &failed, &closed] { fs::create_dir_all(path).expect("tree fixture"); }
    fs::write(working.join("payload"), [0_u8; 7]).expect("working data");
    fs::write(failed.join("payload"), [0_u8; 3]).expect("failed data");
    fs::write(closed.join("payload"), [0_u8; 2]).expect("closed data");
    let mut deep = working.clone();
    while deep.to_string_lossy().chars().count() < 290 { deep.push("long folder with spaces"); }
    fs::create_dir_all(&deep).expect("long path fixture");
    fs::write(deep.join("payload"), [0_u8; 3]).expect("long path data");
    let external = root.join("external cache");
    fs::create_dir_all(&external).expect("external cache fixture");
    fs::write(external.join("excluded"), [0_u8; 999]).expect("excluded data");
    junction(&working.join("linked cache"), &external);
    let stopped = AtomicBool::new(false);
    let measured = measure_worktree(&working, &stopped).expect("Windows measurement");
    let mut failures = report("long-path-spaces-junction-measurement", measured == 10,
        format!("bytes={measured}, nested path length={}", deep.to_string_lossy().chars().count()));
    let cancelled = measure_worktree(&working, &AtomicBool::new(true));
    failures += report("cancelled-measurement", matches!(cancelled, Err(InventoryError::Stopped)), "stopped scan returns typed error");
    let drives = read_drives().expect("native Windows drive inventory");
    failures += report("native-drive-capacities", !drives.is_empty()
        && drives.iter().all(|drive| drive.free_bytes <= drive.total_bytes), format!("{} drives", drives.len()));
    let mut context = store.worktree_context();
    context.owners = vec![owner(1, &working, Status::Working), owner(2, &failed, Status::Failed), owner(3, &closed, Status::Closed)];
    let mut inventory = WorktreeInventory::default();
    inventory.update_drives(Ok(drives));
    for path in [&working, &failed, &closed] {
        inventory.update_size(path.to_string_lossy().into_owned(), measure_worktree(path, &stopped));
    }
    let normal = inventory.snapshot(&context);
    failures += report("closed-failed-live-count-control", normal.live_count == "3" && normal.total_size == "15 B",
        format!("count={}, total={}", normal.live_count, normal.total_size));

    failures + alias_checks(root, &working, &mut inventory, &mut context)
}

fn alias_checks(root: &Path, working: &Path, inventory: &mut WorktreeInventory,
    context: &mut velocity_core::worktree_view::WorktreeContext) -> usize {
    let stopped = AtomicBool::new(false);
    let ascii_alias = root.join("\u{00c5}ngstr\u{00f6}m WORKING TREE");
    context.revision = context.revision.wrapping_add(1);
    context.owners.push(owner(4, &ascii_alias, Status::Working));
    inventory.update_size(ascii_alias.to_string_lossy().into_owned(), measure_worktree(&ascii_alias, &stopped));
    let ascii = inventory.snapshot(context);
    let mut failures = report("ASCII-case-alias-control", ascii_alias.is_dir()
        && fs::canonicalize(&ascii_alias).ok() == fs::canonicalize(working).ok()
        && ascii.live_count == "3" && ascii.total_size == "15 B",
        format!("count={}, total={}", ascii.live_count, ascii.total_size));
    context.owners.pop();
    let alias = root.join("\u{00e5}ngstr\u{00f6}m working tree");
    let same_directory = fs::canonicalize(&alias).ok() == fs::canonicalize(&working).ok();
    if !same_directory || !alias.is_dir() {
        println!("SKIP Unicode case-alias check: fixture filesystem does not expose this case alias");
        return failures;
    }
    println!("Native canonicalize confirms both Unicode spellings identify the same directory");
    context.revision = context.revision.wrapping_add(1);
    context.owners.push(owner(5, &alias, Status::Working));
    inventory.update_size(alias.to_string_lossy().into_owned(), measure_worktree(&alias, &stopped));
    let aliased = inventory.snapshot(context);
    failures += report("same-Windows-directory-counted-once", aliased.live_count == "3",
        format!("expected=3, actual={}", aliased.live_count));
    failures += report("same-Windows-directory-sized-once", aliased.total_size == "15 B",
        format!("expected=15 B, actual={}", aliased.total_size));
    failures
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("fixture path"));
    let (store, mut failures) = persistence(&root);
    failures += inventory(&root, store);
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
Write-Output "Fixture root: $fixtureRoot"
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE
