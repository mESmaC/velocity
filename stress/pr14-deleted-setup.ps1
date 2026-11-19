# Risk: a setup completion retained across F12 deletion must not rename or overwrite
# a replacement thread that reused its ID. Runs real git/planner/store/runtime code and
# the unchanged native completion/record_names bodies with a store-only handle adapter.
# All filesystem effects are confined to the isolated fixture; no agent is started.
# Updated for #230: the native completion decision moved from an inline `let wanted = lock(..)`
# block into `mark_worktree_ready`; the fixture now takes that function's unchanged body.
# Updated for #485: use the native folder_ready path and extract its helpers without relying
# on adjacent doc comments. Exercise the Git folder path and keep the persisted ID check.
$ErrorActionPreference='Stop'
$repositoryRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot=Join-Path $repositoryRoot '.tools\pr14-deleted-setup'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$native=Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'src-tauri\src\threads.rs')
function Get-NativeItem([string]$Signature) {
    $items = [regex]::Matches($native, '(?ms)^' + [regex]::Escape($Signature) + '\b.*?^}')
    if ($items.Count -ne 1) { throw "Native completion item changed: $Signature" }
    return $items[0].Value
}
$nativeItems = @(
    Get-NativeItem 'pub(crate) fn lock'
    Get-NativeItem 'struct ThreadFolder'
    Get-NativeItem 'fn folder_ready'
    Get-NativeItem 'fn mark_worktree_ready'
    Get-NativeItem 'fn record_names'
) -join [Environment]::NewLine
$storePath=(Join-Path $repositoryRoot 'crates\velocity-store').Replace('\','/')
$threadsPath=(Join-Path $repositoryRoot 'crates\velocity-threads').Replace('\','/')
$gitPath=(Join-Path $repositoryRoot 'crates\velocity-git').Replace('\','/')
$manifest=@"
[package]
name = "pr14-deleted-setup"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-store = { path = "$storePath" }
velocity-threads = { path = "$threadsPath" }
velocity-git = { path = "$gitPath" }
serde_json = "1"
"@
$source=@'
use std::{fs, path::{Path, PathBuf}, process::{Command, exit}, sync::{Mutex, MutexGuard}};
use velocity_store::{PersistedState, ProviderKind, StateStore};
use velocity_threads::{LaunchPlan, SessionInputs, StopOutcome, ThreadKey, ThreadRuntimes, plan_launch_with_preferences as plan_launch, title_worktree_name};

type ManagedStateStore = Mutex<StateStore>;
type ManagedThreads = Mutex<ThreadRuntimes>;

// Only the native handle's typed state lookup is replaced. Managed state is real.
struct AppHandle { store: ManagedStateStore, runtimes: ManagedThreads }
trait StateAccess<Value> { fn managed(&self) -> &Value; }
impl StateAccess<ManagedStateStore> for AppHandle {
    fn managed(&self) -> &ManagedStateStore { &self.store }
}
impl StateAccess<ManagedThreads> for AppHandle {
    fn managed(&self) -> &ManagedThreads { &self.runtimes }
}
struct StateReference<'state, Value>(&'state Value);
impl<'state, Value> StateReference<'state, Value> {
    fn inner(&self) -> &'state Value { self.0 }
}
impl AppHandle {
    fn state<Value>(&self) -> StateReference<'_, Value> where Self: StateAccess<Value> {
        StateReference(<Self as StateAccess<Value>>::managed(self))
    }
}
include!("native_functions.rs");

fn complete_setup(application: &AppHandle, key: ThreadKey, path: PathBuf, branch: &str) -> bool {
    let folder = ThreadFolder { path, branch_name: Some(branch.to_owned()), failure: None, rosen_change: None };
    let path = folder.path.display().to_string();
    if !folder_ready(application, key, &folder, &path) {
        return false;
    }
    record_names(application, key, &path, folder.branch_name.as_deref()).expect("native record_names");
    true
}

fn report(label: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {label}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn seed(root: &Path) -> StateStore {
    fs::create_dir_all(root).expect("fixture root");
    let repository = root.join("repository with spaces");
    let output = Command::new("git").args(["init", "--initial-branch", "master"])
        .arg(&repository).output().expect("git init");
    assert!(output.status.success(), "fixture git init");
    let output = Command::new("git").current_dir(&repository).args([
        "-c", "user.name=PR Duty", "-c", "user.email=pr-duty@example.invalid",
        "commit", "--allow-empty", "-m", "isolated delayed setup",
    ]).output().expect("git commit");
    assert!(output.status.success(), "fixture git commit");
    let mut saved = PersistedState::default();
    saved.projects.push(serde_json::from_value(serde_json::json!({
        "id": 1, "name": "fixture", "location_kind": "local",
        "repository_path": repository.to_string_lossy(), "default_branch": "master",
        "github_remote": null, "spec_path": "SPEC.md", "spec_locked": true,
        "active_thread_id": null, "merged_today": 0, "tasks": [], "threads": []
    })).expect("fixture project"));
    fs::write(root.join("state.json"), serde_json::to_vec(&saved).expect("fixture JSON")).expect("state file");
    let mut store = StateStore::load_or_default(root).expect("real store");
    let mut settings = store.settings();
    settings.providers.default_provider = ProviderKind::Codex;
    settings.worktrees.location_root = Some(root.join("worktrees").display().to_string());
    settings.worktrees.copy_files.clear();
    settings.worktrees.setup_command.clear();
    settings.worktrees.share_rust_build_cache = false;
    store.set_settings(settings).expect("fixture settings");
    store
}

fn start(application: &AppHandle, prompt: &str) -> (ThreadKey, LaunchPlan, SessionInputs) {
    let mut store = application.store.lock().expect("store");
    let context = store.worktree_context();
    let plan = plan_launch(store.project(1).expect("project"), &context.preferences, &store.settings(), prompt)
        .expect("real planner");
    let identifier = store.add_thread(1, plan.thread.clone()).expect("launch record");
    drop(store);
    let key = (1, identifier);
    let inputs = application.runtimes.lock().expect("runtimes").start(key, &plan.thread.worktree_path, prompt);
    (key, plan, inputs)
}

fn guard_path(root: &Path, path: &Path) {
    assert!(path.is_absolute() && path.starts_with(root), "planned fixture path escaped");
    let parent = path.parent().expect("fixture parent");
    fs::create_dir_all(parent).expect("fixture parent directory");
    assert!(fs::canonicalize(parent).expect("parent identity").starts_with(fs::canonicalize(root).expect("root identity")));
}

fn branch(path: &Path) -> String {
    let output = Command::new("git").current_dir(path).args(["branch", "--show-current"])
        .output().expect("fixture branch");
    assert!(output.status.success(), "read actual branch");
    String::from_utf8(output.stdout).expect("branch text").trim().into()
}

fn main() {
    assert!(cfg!(windows), "Windows fixture");
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("fixture root"));
    let application = AppHandle { store: Mutex::new(seed(&root)), runtimes: Mutex::default() };
    let (old_key, old_plan, _old_inputs) = start(&application, "original launch message");
    let name = title_worktree_name(&old_plan.naming, &old_plan.worktree, "Original generated title").expect("titled name");
    guard_path(&root, &old_plan.worktree.worktree_path);
    guard_path(&root, &name.worktree_path);
    let old_completed = velocity_git::execute_titled_worktree_setup(&old_plan.worktree, || Some(name));
    assert!(old_completed.setup.is_ok(), "old titled checkout and setup");
    let old_branch = old_completed.branch_name.as_deref().expect("old branch");
    let mut failures = report("old-titled-worktree-control",
        branch(&old_completed.worktree_path) == old_branch, old_completed.worktree_path.display());
    // The blocking setup finished, but its native continuation has not been polled.
    let stopped = application.runtimes.lock().expect("runtimes").stop(old_key);
    failures += report("delete-during-setup-control", matches!(stopped, StopOutcome::SettingUp),
        "the expired F12 deletion leaves setup to clean its own worktree");
    application.store.lock().expect("store").remove_thread(old_key.0, old_key.1).expect("F12 removes record");
    let (new_key, new_plan, _new_inputs) = start(&application, "replacement launch message");
    guard_path(&root, &new_plan.worktree.worktree_path);
    let new_completed = velocity_git::execute_titled_worktree_setup(&new_plan.worktree, || None);
    assert!(new_completed.setup.is_ok(), "new fallback checkout and setup");
    let new_branch = new_completed.branch_name.as_deref().expect("new branch");
    let new_path = new_completed.worktree_path.display().to_string();
    assert!(complete_setup(&application, new_key, new_completed.worktree_path.clone(), new_branch), "new valid completion");
    fs::write(new_completed.worktree_path.join("replacement-sentinel"), "replacement payload").expect("sentinel");
    let before = application.store.lock().expect("store").project(1).expect("project").threads[0].clone();
    failures += report("replacement-running-control",
        application.runtimes.lock().expect("runtimes").is_live(new_key),
        format!("old ID={}, new ID={}", old_key.1, new_key.1));
    let accepted = complete_setup(&application, old_key, old_completed.worktree_path.clone(), old_branch);
    failures += report("deleted-completion-rejected", !accepted, format!("old completion accepted={accepted}"));
    let after = application.store.lock().expect("store").project(1).expect("project").threads[0].clone();
    failures += report("replacement-names-unchanged", before == after,
        format!("expected={new_branch} at {new_path}; actual={} at {:?}", after.branch_name, after.worktree_path));
    let reloaded = StateStore::load_or_default(&root).expect("reload");
    failures += report("saved-replacement-unchanged",
        serde_json::to_value(&reloaded.project(1).expect("project").threads[0]).expect("saved row")
            == serde_json::to_value(&before).expect("replacement row"),
        "old setup must not persist its branch/path into the replacement");
    failures += report("replacement-git-control", branch(&new_completed.worktree_path) == new_branch
        && fs::read_to_string(new_completed.worktree_path.join("replacement-sentinel")).expect("sentinel") == "replacement payload",
        "the actual new worktree and its payload remain correct");
    if accepted { application.runtimes.lock().expect("runtimes").session_ended(old_key); }
    failures += report("replacement-session-remains-live",
        application.runtimes.lock().expect("runtimes").is_live(new_key),
        "the deleted thread's agent end must not end the new thread's session");
    failures += report("deleted-thread-id-not-reused", old_key != new_key,
        format!("old ID={}, new ID={}", old_key.1, new_key.1));
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$encoding=New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'),$manifest,$encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'),$source,$encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\native_functions.rs'),$nativeItems,$encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $repositoryRoot 'target' }
$fixtureRoot=[IO.Path]::GetFullPath((Join-Path $harnessRoot ('fixture-'+[Guid]::NewGuid().ToString('N').Substring(0,8))))
if(-not $fixtureRoot.StartsWith($harnessRoot+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Fixture root escaped review checkout'}
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE

