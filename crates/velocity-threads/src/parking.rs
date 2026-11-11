//! Parking (SPEC.md §2.2). An agent waiting on another agent's PR (`airc_wait`) has its
//! ACP session ended, so it holds no provider slot while it waits; its worktree and claims
//! stay. When the wait ends, Velocity resumes it in the same worktree.

use velocity_airc::waits::{WaitEnded, WaitState};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{AgentActivity, ThreadState};

/// The transcript a new session is sent is cut to its newest characters, so a long thread
/// does not fill the agent's context before it reads what to do next.
const TRANSCRIPT_CONTEXT_CHARACTERS: usize = 40_000;

/// What Velocity does for one thread on a parking pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkingAction {
    /// End the running session and mark the thread `parked`.
    Park,
    /// End the wait if it is still pending, then resume the session.
    Wake,
    Nothing,
}

/// One thread as a parking pass sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParkingInput {
    pub state: ThreadState,
    pub activity: AgentActivity,
    /// The agent's session is past setup and has not ended.
    pub session_running: bool,
    pub wait: WaitState,
}

pub fn parking_action(input: ParkingInput) -> ParkingAction {
    // Only a thread whose agent still has work parks or resumes: a merged, closed, failed
    // or finished thread keeps whatever it has.
    if !matches!(input.state, ThreadState::Working | ThreadState::Review) {
        return ParkingAction::Nothing;
    }
    match (input.activity, input.wait) {
        (AgentActivity::Parked, WaitState::Waiting) => ParkingAction::Nothing,
        (AgentActivity::Parked, _) if !input.session_running => ParkingAction::Wake,
        (_, WaitState::Waiting) if input.session_running => ParkingAction::Park,
        _ => ParkingAction::Nothing,
    }
}

/// The first message of a resumed session: why it stopped and how the wait ended.
pub fn resume_prompt(ended: Option<&WaitEnded>) -> String {
    let opening = "Velocity parked your session while you waited on another agent's PR, and resumed it because the wait ended";
    match ended {
        Some(ended) if ended.update_failed => format!(
            "{opening}: {}. Resolve that in your worktree before any other edit, then continue your work.",
            ended.message
        ),
        Some(ended) => format!("{opening}: {}. Continue your work.", ended.message),
        None => format!("{opening}. Re-read airc_claims and continue your work."),
    }
}

/// The earlier session's transcript as text, for an agent that cannot `session/load` it.
pub fn transcript_context(blocks: &[TranscriptBlock]) -> String {
    let mut lines = Vec::new();
    for block in blocks {
        match block {
            TranscriptBlock::UserMessage {
                text,
                queued: false,
            } => {
                lines.push(format!("[message to you]\n{text}"));
            }
            TranscriptBlock::AgentMessage { paragraphs } => {
                lines.push(format!("[you]\n{}", paragraphs.join("\n\n")));
            }
            TranscriptBlock::ToolGroup { steps } => lines.extend(
                steps
                    .iter()
                    .map(|step| format!("[tool] {} {}", step.verb, step.argument)),
            ),
            TranscriptBlock::ErrorCard { card } => {
                lines.push(format!("[error] {}: {}", card.headline, card.detail));
            }
            TranscriptBlock::Notice { text } => lines.push(format!("[from Velocity]\n{text}")),
            _ => {}
        }
    }
    let joined = lines.join("\n");
    let transcript = newest_characters(&joined, TRANSCRIPT_CONTEXT_CHARACTERS);
    format!(
        "This continues an earlier session of yours in this worktree. Its transcript, newest last:\n\n{transcript}\n\n---\n"
    )
}

fn newest_characters(text: &str, limit: usize) -> &str {
    let count = text.chars().count();
    match text.char_indices().nth(count.saturating_sub(limit)) {
        Some((start, _)) if count > limit => &text[start..],
        _ => text,
    }
}
