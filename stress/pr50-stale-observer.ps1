# Risk: an F6 GitHub read can finish after F12 deletes its thread and a new
# launch reuses its numeric ID. A stale checkpoint must not change the replacement.
# Uses real store/planner entry points; creates only isolated state and git fixtures.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr50-stale-observer'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$storePath = (Join-Path $repositoryRoot 'crates\velocity-store').Replace('\', '/')
$threadsPath = (Join-Path $repositoryRoot 'crates\velocity-threads').Replace('\', '/')
$githubPath = (Join-Path $repositoryRoot 'crates\velocity-github').Replace('\', '/')
$manifest = @"
[package]
name = "pr50-stale-observer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-store = { path = "$storePath" }
velocity-threads = { path = "$threadsPath" }
velocity-github = { path = "$githubPath" }
serde_json = "1"
"@
$source = @'
use std::{fs, path::Path, process::{Command, exit}};
use velocity_github::{PullRequestSnapshot, follow_through::FollowThrough};
use velocity_store::{PersistedState, ProviderKind, StateStore, ThreadState, threads::NewThread};
use velocity_threads::plan_launch_with_preferences as plan_launch;

fn report(label: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {label}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn seed(directory: &Path) -> StateStore {
    fs::create_dir_all(directory).expect("isolated state");
    let repository = directory.join("repository with spaces");
    let initialized = Command::new("git").args(["init", "--initial-branch", "master"])
        .arg(&repository).output().expect("fixture git init");
    assert!(initialized.status.success(), "isolated git init");
    let committed = Command::new("git").current_dir(&repository).args([
        "-c", "user.name=PR Duty", "-c", "user.email=pr-duty@example.invalid",
        "commit", "--allow-empty", "-m", "isolated watcher fixture",
    ]).output().expect("fixture git commit");
    assert!(committed.status.success(), "isolated git commit");
    let mut saved = PersistedState::default();
    saved.projects.push(serde_json::from_value(serde_json::json!({
        "id": 1, "name": "fixture", "location_kind": "local",
        "repository_path": repository.to_string_lossy(), "default_branch": "master",
        "github_remote": null, "spec_path": "SPEC.md", "spec_locked": true,
        "active_thread_id": null, "merged_today": 0, "tasks": [], "threads": []
    })).expect("valid saved project"));
    fs::write(directory.join("state.json"), serde_json::to_vec(&saved).expect("saved fixture"))
        .expect("write isolated state");
    let mut store = StateStore::load_or_default(directory).expect("load real store");
    let mut settings = store.settings();
    settings.providers.default_provider = ProviderKind::Codex;
    settings.worktrees.location_root = Some(directory.join("worktrees").display().to_string());
    store.set_settings(settings).expect("save normal Windows location");
    store
}

fn planned_thread(store: &StateStore, prompt: &str) -> NewThread {
    let context = store.worktree_context();
    plan_launch(store.project(1).expect("project"), &context.preferences, &store.settings(), prompt)
        .expect("real launch planner").thread
}

fn closed_snapshot(branch: &str) -> PullRequestSnapshot {
    serde_json::from_value(serde_json::json!({
        "pull_request": {
            "id": "PR_fixture_42", "number": 42, "title": "Old thread PR", "body": "",
            "url": "https://example.invalid/fixture/project/pull/42", "state": "CLOSED",
            "isDraft": false, "author": null, "headRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "headRefName": branch, "baseRefName": "master", "additions": 120, "deletions": 3,
            "createdAt": "2026-11-09T00:00:00Z", "updatedAt": "2026-11-09T00:00:01Z",
            "closedAt": "2026-11-09T00:00:01Z", "mergedAt": null,
            "reviewDecision": null, "mergeStateStatus": "CLEAN",
            "headRepository": { "nameWithOwner": "fixture/project" }
        },
        "checks": [], "reviews": [], "comments": [], "review_threads": []
    })).expect("real PR snapshot")
}

fn run_case(directory: &Path, label: &str, same_branch: bool) -> usize {
    let mut store = seed(directory);
    let original = planned_thread(&store, "original");
    let original_branch = original.branch_name.clone();
    let original_path = original.worktree_path.clone();
    let original_identifier = store.add_thread(1, original).expect("original launch record");
    let snapshot = closed_snapshot(&original_branch);
    let mut observed = FollowThrough::default();
    observed.observe(&snapshot);
    let accepted = store.save_follow_through(1, original_identifier, &snapshot, observed.clone())
        .expect("valid original observation");
    let mut failures = report(&format!("{label}/original-update-control"), accepted,
        "the observation applies while the original thread exists");
    // Same ordering as a watcher paused in GitHub I/O across an expired F12 deletion.
    store.remove_thread(1, original_identifier).expect("F12 removes the original record");
    let prompt = if same_branch { "original" } else { "replacement" };
    let replacement = planned_thread(&store, prompt);
    let replacement_identifier = store.add_thread(1, replacement).expect("replacement launch");
    let before = store.project(1).expect("project").threads.iter()
        .find(|thread| thread.id == replacement_identifier).expect("replacement").clone();
    failures += report(&format!("{label}/replacement-control"),
        before.state == ThreadState::Working && before.follow_through.is_none(),
        format!("old ID={original_identifier}, new ID={replacement_identifier}"));
    let expected_identity = if same_branch {
        before.branch_name == original_branch && before.worktree_path.as_deref() == Some(&original_path)
    } else { before.branch_name != original_branch };
    failures += report(&format!("{label}/branch-path-control"), expected_identity,
        format!("original={original_branch}, replacement={}", before.branch_name));
    let wrote = store.save_follow_through(1, original_identifier, &snapshot, observed)
        .expect("late original checkpoint");
    failures += report(&format!("{label}/stale-observation-rejected"), !wrote,
        format!("old observation wrote={wrote}"));
    let after = store.project(1).expect("project").threads.iter()
        .find(|thread| thread.id == replacement_identifier).expect("replacement remains");
    failures += report(&format!("{label}/replacement-unchanged"), *after == before,
        format!("actual state={:?}, PR={:?}", after.state, after.pull_request_number));
    let reloaded = StateStore::load_or_default(directory).expect("reload saved result");
    let persisted = reloaded.project(1).expect("project").threads.iter()
        .find(|thread| thread.id == replacement_identifier).expect("persisted replacement");
    failures += report(&format!("{label}/saved-replacement-unchanged"),
        serde_json::to_value(persisted).expect("persisted row")
            == serde_json::to_value(&before).expect("original replacement row"),
        "a late watcher must not persist the deleted thread's PR into its replacement");
    failures
}

fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let failures = run_case(&root.join("different branch"), "different-branch", false)
        + run_case(&root.join("same branch and path"), "same-branch-path", true);
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot '.tools\pr50-target'
$fixtureRoot = [IO.Path]::GetFullPath((Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))))
if (-not $fixtureRoot.StartsWith($harnessRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture root escaped this review checkout' }
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE

