//! Resume after a restart (SPEC.md §3.2, #279). Closing Velocity ends every agent process,
//! so a turn that was running is cut off. When Velocity starts again, each build thread
//! whose turn was cut off continues by itself in its worktree: `session/load` of its earlier
//! session where the agent supports it, otherwise a new session sent the transcript.
//!
//! Parked and at-limit threads are not listed here: their own wake-ups (§2.2) resume them.

use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{AgentActivity, PersistedState, ThreadItem, ThreadState};

use crate::ThreadKey;

/// The first message of a session resumed because Velocity closed during its turn.
pub const CONTINUE_PROMPT: &str = "Velocity was closed while you were working and has started again. Your worktree, claims and transcript are kept, but any command that was running then was stopped: run it again if you still need its result. Continue where you left off.";

/// The threads whose turn the last exit cut off, in saved order. Read the saved state
/// before `StateStore::reset_disconnected_threads`, which sets every one of them to idle.
pub fn interrupted_turns(state: &PersistedState) -> Vec<ThreadKey> {
    state
        .projects
        .iter()
        .flat_map(|project| {
            project
                .threads
                .iter()
                .filter(|thread| turn_interrupted(thread))
                .map(move |thread| (project.id, thread.id))
        })
        .collect()
}

/// A thread whose agent was mid-turn, or still had your queued messages to read, when the
/// app exited. Only a thread whose session had started counts: one cut off during worktree
/// setup or goal-swarm work choice has no session or worktree to continue in.
pub fn turn_interrupted(thread: &ThreadItem) -> bool {
    let active_state = matches!(thread.state, ThreadState::Working | ThreadState::Review);
    let mid_turn = matches!(
        thread.activity,
        AgentActivity::Thinking | AgentActivity::Executing | AgentActivity::Stalled
    );
    let queued = thread
        .transcript
        .iter()
        .any(|block| matches!(block, TranscriptBlock::UserMessage { queued: true, .. }));
    active_state
        && thread.interview.is_none()
        && thread.agent_session.is_some()
        && thread.worktree_path.is_some()
        && (thread.startup_recovery_pending
            || mid_turn
            || (queued && thread.activity == AgentActivity::Idle))
}

// Risk (#384): another exit after the startup reset must not forget an interrupted
// turn with no queued row. Its slot stays reserved until recovery resumes it.
#[cfg(test)]
#[test]
fn startup_recovery_survives_reset_and_another_restart() -> Result<(), Box<dyn std::error::Error>> {
    let (directory, mut store) = interrupted_store()?;
    let interrupted = interrupted_turns(store.state());
    assert_eq!(interrupted, vec![(1, 1)]);
    store.reserve_startup_recovery(&interrupted)?;
    store.reset_disconnected_threads()?;
    drop(store);
    let mut loaded = velocity_store::StateStore::load_or_default(&directory)?;
    assert_eq!(interrupted_turns(loaded.state()), interrupted);
    let thread = &loaded.project(1).ok_or("missing project")?.threads[0];
    assert_eq!(thread.activity, AgentActivity::Idle);
    assert!(!velocity_store::continuous_dispatch::can_finish_empty_dispatch(thread));
    let mut runtimes = crate::ThreadRuntimes::default();
    runtimes.restore(loaded.state());
    let lease = runtimes.action_lock((1, 1));
    let _guard = lease.try_lock()?;
    let mut inputs = runtimes.resume_parked((1, 1), "checkout", Some(CONTINUE_PROMPT), false)?;
    loaded.update_thread(1, 1, |thread| {
        thread.activity = AgentActivity::Thinking;
        thread.startup_recovery_pending = false;
    })?;
    assert!(inputs.try_recv().is_ok());
    assert!(!runtimes.is_at_step_boundary((1, 1)));
    assert!(
        !velocity_store::continuous_dispatch::can_finish_empty_dispatch(
            &loaded.project(1).ok_or("missing project")?.threads[0]
        )
    );
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

#[cfg(test)]
fn interrupted_store()
-> Result<(std::path::PathBuf, velocity_store::StateStore), Box<dyn std::error::Error>> {
    let directory =
        std::env::temp_dir().join(format!("velocity-384-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let mut state = PersistedState::default();
    let mut project = velocity_store::ProjectItem::new(1, "restart".into(), "checkout".into());
    let mut thread = ThreadItem::new(1, "interrupted".into());
    thread.agent_session = Some("earlier-session".into());
    thread.worktree_path = Some("checkout".into());
    thread.activity = AgentActivity::Thinking;
    thread.supervision.turns = 1;
    project.threads.push(thread);
    state.projects.push(project);
    std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;
    let store = velocity_store::StateStore::load_or_default(&directory)?;
    Ok((directory, store))
}
