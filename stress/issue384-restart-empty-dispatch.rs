// Risk: continuous dispatch retires an interrupted, empty Rosen thread before startup
// recovery replays its saved user message. Drive the real startup/store/runtime methods
// in that interleaving; no provider or GitHub request is needed to reproduce retirement.
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{AgentActivity, PersistedState, ProjectItem, StateStore};
use velocity_store::{issues::ThreadIssue, threads::NewThread};
use velocity_threads::{ThreadRuntimes, restart::interrupted_turns};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("dispatch-restart-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let mut saved = PersistedState::default();
    let mut project = ProjectItem::new(1, "restart".into(), directory.display().to_string());
    project.spec_locked = true;
    project.continuous_dispatch.slots = 1;
    saved.projects.push(project);
    std::fs::write(directory.join("state.json"), serde_json::to_vec(&saved)?)?;
    let mut store = StateStore::load_or_default(&directory)?;
    let thread = NewThread::new("Issue".into(), "vel/issue-42".into(),
        directory.join("empty-workspace").display().to_string(), &store.settings());
    let identifier = store.record_continuous_dispatch(1, thread,
        ThreadIssue::new("owner/repo".into(), 42), "version-1".into())?;
    store.update_thread(1, identifier, |thread| {
        thread.agent_session = Some("interrupted-session".into());
        thread.rosen_change = Some("empty-unnamed-change".into());
        thread.supervision.turns = 1;
        thread.activity = AgentActivity::Thinking;
        thread.transcript = std::sync::Arc::new(vec![TranscriptBlock::UserMessage {
            text: "Do the next part".into(), queued: true,
        }]);
    })?;
    let interrupted = interrupted_turns(store.state());
    assert_eq!(interrupted, vec![(1, identifier)]);
    store.reset_disconnected_threads()?;
    let mut runtimes = ThreadRuntimes::default();
    runtimes.restore(store.state());
    let thread = &store.project(1).ok_or("missing project")?.threads[0];
    // These are the admission checks in finish_empty_threads/finish_empty_thread.
    let admitted = thread.interview.is_none()
        && thread.state == velocity_store::ThreadState::Working
        && thread.activity == AgentActivity::Idle
        && thread.supervision.turns > 0
        && thread.rosen_change.is_some()
        && runtimes.is_at_step_boundary((1, identifier));
    println!("Interrupted turn awaiting restart: {interrupted:?}");
    println!("Empty-thread cleanup admits interrupted turn: {admitted}");
    if admitted {
        // The workspace has no edits and no named snapshot. Cleanup runs first.
        let _ = runtimes.stop((1, identifier));
        store.finish_empty_dispatch(1, identifier)?;
    }
    let thread = &store.project(1).ok_or("missing project")?.threads[0];
    println!("Thread state before restart::resume: {:?}", thread.state);
    println!("Saved unread message: {:?}", thread.transcript);
    let can_resume = runtimes.resume_parked((1, identifier),
        thread.worktree_path.as_deref().ok_or("missing worktree")?,
        Some(velocity_threads::restart::CONTINUE_PROMPT), false).is_ok();
    println!("Restart can resume the interrupted turn: {can_resume}");
    std::fs::remove_dir_all(&directory)?;
    assert!(can_resume, "continuous dispatch retired unread work during startup recovery");
    Ok(())
}
