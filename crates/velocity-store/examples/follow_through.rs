//! Run the F6 persistence and window projection against a real PR without a window.
//! Arguments: state-directory, repository, PR number, optional `closed` projection.
use std::path::Path;
use velocity_core::layout_state::LayoutState;
use velocity_github::{
    PullRequestClient, PullRequestSnapshot, PullRequestState, follow_through::FollowThrough,
};
use velocity_store::{
    AgentActivity, EffortLevel, PersistedState, ProjectItem, ProjectLocationKind, ProviderKind,
    SpeedMode, StateStore, ThreadItem, ThreadState,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    let directory = Path::new(
        arguments
            .get(1)
            .ok_or("expected isolated state directory")?,
    );
    let repository = arguments.get(2).ok_or("expected repository")?;
    let number = arguments.get(3).ok_or("expected PR number")?.parse()?;
    let mut snapshot = PullRequestClient::discover(Path::new(repository))?.snapshot(number)?;
    if arguments.get(4).is_some_and(|mode| mode == "closed") {
        snapshot.pull_request.state = PullRequestState::Closed;
    }
    std::fs::create_dir_all(directory)?;
    let state_file = directory.join("state.json");
    if state_file.exists() {
        return Err("Use a fresh isolated state directory".into());
    }
    let state = seed(repository, &snapshot);
    std::fs::write(state_file, serde_json::to_vec_pretty(&state)?)?;
    let mut store = StateStore::load_or_default(directory)?;
    let mut watch = FollowThrough::default();
    watch.observe(&snapshot);
    store.save_follow_through(1, 1, &snapshot, watch.clone())?;
    let loaded = StateStore::load_or_default(directory)?;
    let thread = &loaded.state().projects[0].threads[0];
    // A lost cursor/outbox on save/load can drop review work or replay it forever.
    if thread.follow_through.as_ref() != Some(&watch) {
        return Err("Watcher state did not survive save/load".into());
    }
    if thread.worktree_path.as_deref() != Some(repository) {
        return Err("The retained worktree path changed".into());
    }
    let mut layout = LayoutState::default();
    loaded.restore_project_layout(&mut layout);
    println!("{}", serde_json::to_string_pretty(&layout.snapshot())?);
    Ok(())
}

fn seed(repository: &str, snapshot: &PullRequestSnapshot) -> PersistedState {
    let mut state = PersistedState::default();
    state.projects.push(ProjectItem {
        id: 1,
        name: "Velocity follow-through".into(),
        location_kind: ProjectLocationKind::GitHub,
        repository_path: repository.into(),
        default_branch: "master".into(),
        github_remote: None,
        spec_path: "SPEC.md".into(),
        spec_locked: true,
        active_thread_id: Some(1),
        merged_today: 0,
        tasks: Vec::new(),
        threads: vec![seed_thread(repository, snapshot)],
        pending_interview: None,
    });
    state
}

fn seed_thread(repository: &str, snapshot: &PullRequestSnapshot) -> ThreadItem {
    ThreadItem {
        id: 1,
        title: snapshot.pull_request.title.clone(),
        state: ThreadState::Working,
        activity: AgentActivity::Idle,
        branch_name: snapshot.pull_request.head_ref_name.clone(),
        worktree_path: Some(repository.into()),
        worktree_path_id: None,
        pull_request_number: None,
        follow_through: None,
        review_stage: None,
        task_ids: Vec::new(),
        additions: 0,
        deletions: 0,
        elapsed_seconds: 0,
        provider: ProviderKind::Codex,
        model_name: "agent default".into(),
        effort: EffortLevel::Low,
        preferred_speed: SpeedMode::Regular,
        effective_speed: SpeedMode::Regular,
        model_supports_fast: false,
    }
}
