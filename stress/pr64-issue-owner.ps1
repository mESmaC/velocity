# Risk: F9 ownership must survive reload and refuse duplicate work while retaining
# F6's non-reused identifiers and birth metadata. Uses the real saved-state store;
# no agents, GitHub writes or worktree deletion are run.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr64-issue-owner'
$fixtureRoot = Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$storePath = (Join-Path $repositoryRoot 'crates\velocity-store').Replace('\', '/')
$manifest = @"
[package]
name = "pr64-issue-owner"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-store = { path = "$storePath" }
serde_json = "1"
[profile.dev]
debug = "line-tables-only"
[profile.dev.package."*"]
debug = false
"@
$source = @'
use std::{fs, path::{Path, PathBuf}};
use velocity_store::{AgentActivity, PersistedState, StateStore, ThreadState};
use velocity_store::issues::{ThreadIssue, issue_thread, issue_thread_label};
use velocity_store::threads::{NewThread, ThreadError};

fn report(label: &str, passed: bool) -> usize {
    println!("{} {label}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn seed(root: &Path) -> StateStore {
    fs::create_dir_all(root).expect("isolated fixture");
    let mut state = PersistedState::default();
    state.projects.push(serde_json::from_value(serde_json::json!({
        "id": 1, "name": "owner fixture", "location_kind": "local",
        "repository_path": root.join("repository with spaces").to_string_lossy(),
        "default_branch": "master", "github_remote": null,
        "spec_path": "SPEC.md", "spec_locked": true, "active_thread_id": null,
        "merged_today": 0, "tasks": [], "threads": []
    })).expect("legacy project without the new optional thread fields"));
    fs::write(root.join("state.json"), serde_json::to_vec(&state).expect("fixture JSON"))
        .expect("fixture state");
    StateStore::load_or_default(root).expect("real store")
}

fn record(store: &StateStore, root: &Path) -> NewThread {
    NewThread::new("Fix issue".into(), "vel/issue-41".into(),
        root.join("worktree with spaces").display().to_string(), &store.settings())
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("fixture directory"));
    let mut store = seed(&root);
    let issue = ThreadIssue::new("mESmaC/velocity".into(), 41);
    let first = store.add_issue_thread(1, record(&store, &root), issue.clone()).expect("first owner");
    let revision = store.state().revision;
    let duplicate = store.add_issue_thread(1, record(&store, &root), issue.clone());
    let mut failures = report("duplicate-refused-without-saving", matches!(duplicate,
        Err(ThreadError::IssueAssigned { thread, .. }) if thread == first)
        && store.state().revision == revision);
    // A completed ACP turn leaves a Working thread with Idle activity. Its saved
    // issue projection must not tell the user that the stopped agent is working.
    store.update_thread(1, first, |thread| thread.activity = AgentActivity::Idle).expect("ended turn");
    let idle_store = StateStore::load_or_default(&root).expect("reload ended turn");
    let idle_owner = issue_thread(idle_store.project(1).expect("project"), &issue).expect("idle owner");
    failures += report("saved-idle-owner-not-reported-working", idle_owner.activity == AgentActivity::Idle
        && !issue_thread_label(idle_owner).contains("working"));
    store.update_thread(1, first, |thread| thread.state = ThreadState::Error).expect("failed owner");
    let mut store = StateStore::load_or_default(&root).expect("reload saved ownership");
    let owner = issue_thread(store.project(1).expect("project"), &issue).expect("persisted owner");
    failures += report("ownership-and-failed-state-survive-reload", owner.id == first
        && owner.state == ThreadState::Error);
    failures += report("birth-metadata-retained", store.state().thread_started_at.get(&1)
        .is_some_and(|entries| entries.contains_key(&first)));
    store.remove_thread(1, first).expect("expired deletion");
    let mut store = StateStore::load_or_default(&root).expect("reload deletion and high-water mark");
    failures += report("deletion-releases-issue", issue_thread(store.project(1).expect("project"), &issue).is_none());
    let next = store.add_issue_thread(1, record(&store, &root), issue.clone()).expect("new owner");
    failures += report("deleted-identifier-never-reused", next > first);
    let store = StateStore::load_or_default(&root).expect("reload replacement");
    let replacement = issue_thread(store.project(1).expect("project"), &issue).expect("new saved owner");
    failures += report("replacement-and-birth-metadata-persist", replacement.id == next
        && store.state().thread_started_at.get(&1).is_some_and(|entries| entries.contains_key(&next)));
    println!("failures: {failures}");
    std::process::exit(i32::from(failures != 0));
}
'@
$encoding = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot 'target'
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE
