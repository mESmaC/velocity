use std::{fs, path::{Path, PathBuf}, process::Command};
use velocity_store::{PersistedState, ProviderKind, StateStore};
use velocity_github::GitHubIssue;
use velocity_threads::{plan_issue_dispatch, plan_launch_with_preferences as plan_launch};

fn report(label: &str, passed: bool) -> usize {
    println!("{} {label}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn seed(root: &Path) -> StateStore {
    fs::create_dir_all(root).expect("isolated root");
    let repository = root.join("repository with spaces");
    let status = Command::new("git").args(["init", "--initial-branch", "master"])
        .arg(&repository).status().expect("isolated git");
    assert!(status.success(), "fixture git init");
    let mut saved = PersistedState::default();
    saved.settings.providers.default_provider = ProviderKind::Codex;
    saved.settings.providers.title_provider = ProviderKind::Codex;
    saved.projects.push(serde_json::from_value(serde_json::json!({
        "id":1, "name":"fixture", "location_kind":"local",
        "repository_path":repository.to_string_lossy(), "default_branch":"master",
        "github_remote":null, "spec_path":"SPEC.md", "spec_locked":true,
        "active_thread_id":null, "merged_today":0, "tasks":[], "threads":[]
    })).expect("legacy project"));
    fs::write(root.join("state.json"), serde_json::to_vec(&saved).expect("fixture JSON"))
        .expect("fixture state");
    StateStore::load_or_default(root).expect("real store")
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("fixture directory"));
    let store = seed(&root);
    let issue: GitHubIssue = serde_json::from_value(serde_json::json!({
        "id":"fixture", "number":41, "title":"Keep the issue title", "body":"fixture",
        "url":"https://github.com/mESmaC/velocity/issues/41", "state":"OPEN",
        "author":null, "createdAt":"2026-11-09T00:00:00Z",
        "updatedAt":"2026-11-09T00:00:00Z", "closedAt":null, "labels":[], "scout":null
    })).expect("issue fixture");
    let plan = plan_issue_dispatch(&store, 1, "mESmaC/velocity", &issue, false).expect("dispatch plan");
    let project = store.project(1).expect("project");
    let context = store.worktree_context();
    let composer = plan_launch(project, &context.preferences, &store.settings(), "Ordinary composer work")
        .expect("composer control");
    let mut failures = report("composer-keeps-title-model", composer.title.request.is_some());
    failures += report("dispatched-title-kept", plan.launch.thread.title == issue.title);
    failures += report("dispatched-branch-kept", plan.launch.thread.branch_name == "vel/issue-41");
    failures += report("dispatched-work-never-requests-title-model", plan.launch.title.request.is_none());
    println!("failures: {failures}");
    std::process::exit(i32::from(failures != 0));
}
