# Risk: SPEC.md Rule 1/F4 requires messages to a busy PR-review agent to queue.
# Exercises real state.json load, project projection and the Rust composer reducer.
# Does not launch an ACP process or test native UI/IPC transport.
param()
$ErrorActionPreference = 'Stop'
$repository = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixture = Join-Path $repository ('.tools\pr38-busy-review\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $fixture 'src') -Force | Out-Null
$manifest = @'
[package]
name = "velocity-pr38-busy-review"
version = "0.1.0"
edition = "2024"
[workspace]
[dependencies]
velocity-core = { path = "REPO_ROOT/crates/velocity-core", features = ["bindings"] }
velocity-store = { path = "REPO_ROOT/crates/velocity-store" }
serde_json = "1.0"
'@
$source = @'
use std::{fs, path::Path};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent};
use velocity_store::{AgentActivity, PersistedState, ReviewStage, StateStore, ThreadState};

fn run_case(root: &Path, label: &str, state: ThreadState, activity: AgentActivity,
            stage: Option<ReviewStage>, expected_queue: bool) -> bool {
    let folder = root.join(label);
    fs::create_dir_all(&folder).expect("fresh case directory");
    let mut saved = PersistedState::default();
    let project = serde_json::json!({
        "id": 1, "name": "fixture", "location_kind": "local",
        "repository_path": "C:/fixture", "default_branch": "master",
        "github_remote": null, "spec_path": "SPEC.md", "spec_locked": true,
        "active_thread_id": 7, "merged_today": 0, "tasks": [],
        "threads": [{
            "id": 7, "title": "Address PR comments", "state": state,
            "activity": activity, "branch_name": "vel/address-comments",
            "worktree_path": "X:/Worktrees/Address comments/with spaces",
            "pull_request_number": 41, "review_stage": stage, "task_ids": [],
            "additions": 182, "deletions": 12, "elapsed_seconds": 389,
            "provider": "claude", "model_name": "fixture model", "effort": "high",
            "preferred_speed": "regular", "effective_speed": "regular",
            "model_supports_fast": true
        }]
    });
    saved.projects.push(serde_json::from_value(project).expect("valid persisted project"));
    fs::write(folder.join("state.json"), serde_json::to_vec(&saved).expect("serialize state"))
        .expect("write state fixture");
    let store = StateStore::load_or_default(&folder).expect("load real state store");
    let mut layout = LayoutState::default();
    store.restore_project_layout(&mut layout);
    let busy = layout.snapshot().active_thread.expect("saved thread must have detail").agent_busy;
    layout.apply(LayoutAction::ComposerText("Follow up after the current step".into()));
    let queued = match layout.apply(LayoutAction::SubmitComposer) {
        Some(LayoutIntent::SteerThread { queued, .. }) => queued,
        other => panic!("expected SteerThread, got {other:?}"),
    };
    let passed = busy == expected_queue && queued == expected_queue;
    println!("{} {label}: agent_busy={busy}, queued={queued}, expected={expected_queue}",
        if passed { "PASS" } else { "FAIL" });
    passed
}

fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("fixture root"));
    let cases = [
        ("working-thinking", ThreadState::Working, AgentActivity::Thinking, None, true),
        ("review-fixing-thinking", ThreadState::Review, AgentActivity::Thinking, Some(ReviewStage::Fixing), true),
        ("review-fixing-executing", ThreadState::Review, AgentActivity::Executing, Some(ReviewStage::Fixing), true),
        ("review-ci-idle", ThreadState::Review, AgentActivity::Idle, Some(ReviewStage::ContinuousIntegration), false),
    ];
    let mut failures = 0;
    for (label, state, activity, stage, expected) in cases {
        if !run_case(&root, label, state, activity, stage, expected) { failures += 1; }
    }
    println!("failures: {failures}; fixture retained at {}", root.display());
    if failures != 0 { std::process::exit(1); }
}
'@
$encoding = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $fixture 'Cargo.toml'), $manifest.Replace('REPO_ROOT', $repository.Replace('\', '/')), $encoding)
[System.IO.File]::WriteAllText((Join-Path $fixture 'src/main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repository 'Cargo.lock') -Destination (Join-Path $fixture 'Cargo.lock')
Write-Output "Harness: $fixture"
cargo run --offline --manifest-path (Join-Path $fixture 'Cargo.toml') -- $fixture
exit $LASTEXITCODE
