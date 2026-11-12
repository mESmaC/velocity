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
        && (mid_turn || (queued && thread.activity == AgentActivity::Idle))
}
