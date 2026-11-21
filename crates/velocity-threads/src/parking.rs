//! Parking (SPEC.md §2.2). An agent waiting on another agent's PR (`airc_wait`) or on
//! Rosen path claims (`rosen` MCP `wait`) has its ACP session ended, so it holds no provider
//! slot while it waits; its workspace and claims stay. When the wait ends, Velocity resumes
//! it in the same workspace.

use velocity_airc::waits::{WaitEnded, WaitState};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::rosen::RosenPathWait;
use velocity_store::{AgentActivity, ThreadState};

/// The transcript a new session is sent is cut to its newest characters, so a long thread
/// does not fill the agent's context before it reads what to do next.
const TRANSCRIPT_CONTEXT_BYTES: usize = 7_700;

pub const HANDOFF_OPENING: &str =
    "Continue the work item from the saved hand-off and the current workspace state.";
pub const HANDOFF_BOUNDARY: &str = "Velocity started a fresh session for this work item.";

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

/// What Velocity does for a Rosen thread waiting on Rosen path claims (#531).
/// Unlike blocking `airc_wait`, the Rosen MCP `wait` tool returns at once so the agent
/// finishes its turn cleanly before Velocity ends the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosenParkingAction {
    /// Turn ended while Rosen path waits remain held: end the session and mark `parked`.
    Park,
    /// Session is parked and all waited paths are free: apply rebase and resume.
    Wake,
    /// Session stayed live and its turn ended after waited paths already freed: continue
    /// directly without losing the wake or restarting the session unnecessarily.
    ContinueLive,
    Nothing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RosenParkingInput {
    pub state: ThreadState,
    pub activity: AgentActivity,
    pub session_running: bool,
    pub turn_in_progress: bool,
    pub has_active_waits: bool,
    pub tracked_wait: bool,
}

pub fn rosen_parking_action(input: RosenParkingInput) -> RosenParkingAction {
    if !matches!(input.state, ThreadState::Working | ThreadState::Review) {
        return RosenParkingAction::Nothing;
    }
    match (input.activity, input.has_active_waits) {
        (AgentActivity::Parked, true) => RosenParkingAction::Nothing,
        (AgentActivity::Parked, false) if !input.session_running && input.tracked_wait => {
            RosenParkingAction::Wake
        }
        (_, true) if input.session_running && !input.turn_in_progress => RosenParkingAction::Park,
        (_, false) if input.tracked_wait && input.session_running && !input.turn_in_progress => {
            RosenParkingAction::ContinueLive
        }
        _ => RosenParkingAction::Nothing,
    }
}

/// The first message sent when a Rosen path wait ends (#531).
pub fn rosen_resume_prompt(wait: &RosenPathWait) -> String {
    let paths = if wait.paths.is_empty() {
        String::from("your waited paths")
    } else {
        wait.paths.join(", ")
    };
    let detail = if wait.wake_messages.is_empty() {
        format!("{paths} are now free")
    } else {
        wait.wake_messages.join("; ")
    };
    format!(
        "Velocity parked your session while you waited on Rosen paths ({paths}), and resumed it because the wait ended ({detail}). Claim the code paths you need with `rosen claim --exclusive <paths>` (or the `rosen` MCP `claim` tool with `exclusive: true`) and continue your work."
    )
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
    let start = joined.ceil_char_boundary(joined.len().saturating_sub(TRANSCRIPT_CONTEXT_BYTES));
    let notice = if start > 0 { " (recent excerpt)" } else { "" };
    let transcript = &joined[start..];
    format!(
        "This continues an earlier session of yours in this worktree. Its transcript{notice}, newest last:\n\n{transcript}\n\n---\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#531): Rosen parking must wait for turn end before stopping a session, wake once
    // all waited paths are free, and continue directly if paths freed before turn end.
    #[test]
    fn rosen_parking_decisions_preserve_turns_and_wakes() {
        let mid_turn = RosenParkingInput {
            state: ThreadState::Working,
            activity: AgentActivity::Thinking,
            session_running: true,
            turn_in_progress: true,
            has_active_waits: true,
            tracked_wait: true,
        };
        assert_eq!(rosen_parking_action(mid_turn), RosenParkingAction::Nothing);

        let turn_ended = RosenParkingInput {
            activity: AgentActivity::Idle,
            turn_in_progress: false,
            ..mid_turn
        };
        assert_eq!(rosen_parking_action(turn_ended), RosenParkingAction::Park);

        let still_parked = RosenParkingInput {
            activity: AgentActivity::Parked,
            session_running: false,
            turn_in_progress: false,
            has_active_waits: true,
            tracked_wait: true,
            state: ThreadState::Working,
        };
        assert_eq!(
            rosen_parking_action(still_parked),
            RosenParkingAction::Nothing
        );

        let wake = RosenParkingInput {
            has_active_waits: false,
            ..still_parked
        };
        assert_eq!(rosen_parking_action(wake), RosenParkingAction::Wake);

        let freed_before_turn_end = RosenParkingInput {
            state: ThreadState::Working,
            activity: AgentActivity::Idle,
            session_running: true,
            turn_in_progress: false,
            has_active_waits: false,
            tracked_wait: true,
        };
        assert_eq!(
            rosen_parking_action(freed_before_turn_end),
            RosenParkingAction::ContinueLive
        );
    }
}
