//! Parking (SPEC.md §2.2). An agent waiting on another agent's PR (`airc_wait`) or on
//! Rosen path claims (`rosen` MCP `wait`) has its ACP session ended, so it holds no provider
//! slot while it waits; its workspace and claims stay. When the wait ends, Velocity resumes
//! it in the same workspace.

use velocity_airc::waits::{WaitEnded, WaitState};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::rosen::{RosenOverlapWait, RosenPathWait};
use velocity_store::{AgentActivity, ThreadItem, ThreadState};

use crate::ThreadKey;

/// Whether `state` is allowed to resume a build session (#585).
/// Merged, Done, Spec and Draft are final or non-build states and can never resume.
pub const fn can_resume_build_session(state: ThreadState) -> bool {
    velocity_store::rosen::can_receive_follow_up(state)
}

/// Opening mode passed to a session resume, included in non-resumable diagnostics (#585).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeOpeningKind {
    Prompt,
    RestoredRuns,
    Handoff,
    AfterLimit,
    Message,
}

impl ResumeOpeningKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Prompt => "Prompt",
            Self::RestoredRuns => "RestoredRuns",
            Self::Handoff => "Handoff",
            Self::AfterLimit => "AfterLimit",
            Self::Message => "Message",
        }
    }
}

/// Error returned when starting or resuming a build session fails (#585).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResumeError {
    #[error(
        "Thread {}/{} cannot resume a build session ({}, state: {:?})",
        key.0,
        key.1,
        opening.label(),
        state
    )]
    NonResumable {
        key: ThreadKey,
        state: ThreadState,
        opening: ResumeOpeningKind,
    },
    #[error("{0}")]
    Other(String),
}

impl ResumeError {
    pub const fn is_non_resumable(&self) -> bool {
        matches!(self, Self::NonResumable { .. })
    }
}

/// Checks whether `key` in `state` may resume a build session for `opening` (#585).
pub fn check_resumable(
    key: ThreadKey,
    state: ThreadState,
    opening: ResumeOpeningKind,
) -> Result<(), ResumeError> {
    if can_resume_build_session(state) {
        Ok(())
    } else {
        Err(ResumeError::NonResumable {
            key,
            state,
            opening,
        })
    }
}

/// Drops pending deliveries on a thread that reached a final non-resumable state (#585).
pub fn drop_pending_deliveries(thread: &mut ThreadItem) -> bool {
    velocity_store::rosen::drop_pending_deliveries(thread)
}

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
        (_, false) if !input.session_running && input.tracked_wait => RosenParkingAction::Wake,
        (_, true) if input.session_running && !input.turn_in_progress => RosenParkingAction::Park,
        (_, false) if input.tracked_wait && input.session_running && !input.turn_in_progress => {
            RosenParkingAction::ContinueLive
        }
        _ => RosenParkingAction::Nothing,
    }
}

/// Deterministic deadlock-breaker when two threads mutually wait on each other's overlapping
/// Rosen changes (#565): returns `true` only when every remaining open blocker is itself
/// waiting on `change` and `change` has the strictly smallest change ID among them.
pub fn should_break_mutual_overlap_wait(
    change: &str,
    open_blockers: &[String],
    peer_blockers: &[(&str, &[String])],
) -> bool {
    !open_blockers.is_empty()
        && open_blockers.iter().all(|blocker| {
            change < blocker.as_str()
                && peer_blockers.iter().any(|(peer_change, peer_targets)| {
                    *peer_change == blocker.as_str()
                        && peer_targets.iter().any(|target| target == change)
                })
        })
}

/// The first message sent when a Rosen overlap wait ends (#565).
pub fn rosen_overlap_resume_prompt(wait: &RosenOverlapWait) -> String {
    let paths = wait.paths();
    let scope = if paths.is_empty() {
        String::from("overlapping Rosen changes")
    } else {
        format!("overlapping Rosen changes on {}", paths.join(", "))
    };
    let detail = if wait.wake_messages.is_empty() {
        String::from("the blocking changes finished")
    } else {
        wait.wake_messages.join("; ")
    };
    format!(
        "Velocity paused your wait on {scope}, and resumed you because {detail}. Any pending \
         rebase has been applied. Run `preflight` from `velocity-run` and name your change with \
         `rosen commit -m` once it passes."
    )
}

/// Combined wake/continue prompt when Rosen path and/or overlap waits end (#531, #565).
pub fn rosen_wait_resume_prompt(
    path_wait: Option<&RosenPathWait>,
    overlap_wait: Option<&RosenOverlapWait>,
) -> String {
    match (path_wait, overlap_wait) {
        (Some(path), Some(overlap)) => format!(
            "{}\n\n{}",
            rosen_resume_prompt(path),
            rosen_overlap_resume_prompt(overlap)
        ),
        (Some(path), None) => rosen_resume_prompt(path),
        (None, Some(overlap)) => rosen_overlap_resume_prompt(overlap),
        (None, None) => String::from(
            "Your Rosen wait ended and any pending rebase has been applied. Run `preflight` \
             from `velocity-run` and continue your work.",
        ),
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

        let freed_after_restart = RosenParkingInput {
            session_running: false,
            ..freed_before_turn_end
        };
        assert_eq!(
            rosen_parking_action(freed_after_restart),
            RosenParkingAction::Wake
        );
    }

    // Risk (#565): mutual overlap waits must break deterministically on one side only, and
    // overlap wake prompts must instruct the resumed agent to re-run preflight and commit.
    #[test]
    fn mutual_overlap_wait_breaks_on_lower_change_id() {
        let a_waits = vec!["bbbbbbbb".to_owned()];
        let b_waits = vec!["aaaaaaaa".to_owned()];
        let peers = [
            ("aaaaaaaa", a_waits.as_slice()),
            ("bbbbbbbb", b_waits.as_slice()),
        ];
        assert!(should_break_mutual_overlap_wait(
            "aaaaaaaa", &a_waits, &peers
        ));
        assert!(!should_break_mutual_overlap_wait(
            "bbbbbbbb", &b_waits, &peers
        ));

        let mut wait = RosenOverlapWait::from_overlaps(
            &[velocity_core::preflight::PreflightOverlap {
                change: "bbbbbbbb12345678".into(),
                owner: "issue-130-178".into(),
                paths: vec!["design/screens/README.md".into()],
            }],
            10,
        );
        wait.record_resolution("bbbbbbbb12345678", "landed");
        let prompt = rosen_wait_resume_prompt(None, Some(&wait));
        assert!(prompt.contains("issue-130-178"), "{prompt}");
        assert!(prompt.contains("landed"), "{prompt}");
        assert!(prompt.contains("preflight"), "{prompt}");
    }

    // Risk (#585): a queued run result or startup recovery pending for a thread that merged
    // in the meantime must fail with the thread key and opening kind, mark history delivered
    // after end, and stop retrying after one attempt.
    #[test]
    fn queued_run_result_for_merged_thread_is_dropped_after_one_attempt() {
        use velocity_core::queued_runs::{QueuedRunClass, QueuedRunPhase, QueuedRunRecord};

        let key = (1, 207);
        let mut thread = ThreadItem::new(207, "Issue #436".into());
        thread.agent_session = Some("session-207".into());
        thread.worktree_path = Some(r"Z:\velocity-work\velocity\issue-436".into());
        thread.state = ThreadState::Working;
        thread.activity = AgentActivity::Idle;
        thread.startup_recovery_pending = true;
        thread.queued_runs.push(QueuedRunRecord::new(
            "velocity-run-1".into(),
            "cargo".into(),
            vec!["check".into()],
            None,
            QueuedRunClass::Build,
        ));
        let mut finished = QueuedRunRecord::new(
            "velocity-run-0".into(),
            "cargo".into(),
            vec!["check".into()],
            None,
            QueuedRunClass::Build,
        );
        finished.phase = QueuedRunPhase::Completed;
        finished.exit_code = Some(0);
        finished.delivered = false;
        thread.queued_run_history.push(finished);
        assert!(crate::restart::turn_interrupted(&thread));

        // Thread lands and transitions to Merged before the resume runs.
        thread.state = ThreadState::Merged;
        let error = check_resumable(key, thread.state, ResumeOpeningKind::RestoredRuns)
            .err()
            .unwrap_or(ResumeError::Other(String::new()));
        assert!(error.is_non_resumable());
        let message = error.to_string();
        assert!(message.contains("1/207"), "{message}");
        assert!(message.contains("RestoredRuns"), "{message}");
        assert!(message.contains("Merged"), "{message}");

        assert!(drop_pending_deliveries(&mut thread));
        assert!(thread.queued_runs.is_empty());
        assert_eq!(thread.queued_run_history.len(), 1);
        assert!(thread.queued_run_history[0].delivered);
        assert!(!thread.startup_recovery_pending);
        assert!(!crate::restart::turn_interrupted(&thread));
        assert!(!drop_pending_deliveries(&mut thread));
    }

    // Risk (#585): supervision nudges, after-limit retries, handoffs, and Rosen follow-ups
    // must treat Merged, Done, Spec and Draft as final, name the thread and opening kind on
    // the first refusal, drop pending deliveries, and never schedule a second attempt.
    #[test]
    fn supervision_nudge_and_pending_resumes_for_final_thread_stop_after_one_attempt() {
        use crate::agent_activity::{Action, IDLE_WORKING_TIMEOUT_SECS, decide_with_turn};
        use velocity_store::Settings;
        use velocity_store::handoff::Note;
        use velocity_store::rosen::{RosenFollowUp, RosenResolutionStatus};

        let settings = Settings::default().agents;
        let key = (1, 207);
        let due = 1000 + IDLE_WORKING_TIMEOUT_SECS;

        for (final_state, opening, expected_label) in [
            (ThreadState::Merged, ResumeOpeningKind::Message, "Message"),
            (ThreadState::Done, ResumeOpeningKind::Handoff, "Handoff"),
            (
                ThreadState::Spec,
                ResumeOpeningKind::AfterLimit,
                "AfterLimit",
            ),
            (ThreadState::Draft, ResumeOpeningKind::Prompt, "Prompt"),
        ] {
            let mut thread = ThreadItem::new(207, "Issue #436".into());
            thread.state = ThreadState::Working;
            thread.activity = AgentActivity::Idle;
            thread.worktree_path = Some(r"Z:\velocity-work\velocity\issue-436".into());
            thread.supervision.started = 1000;
            thread.supervision.last_update = 1000;
            assert_eq!(
                decide_with_turn(&thread, &settings, due, false, false, false),
                Action::IdleNudge
            );

            thread.state = final_state;
            thread.activity = AgentActivity::AtLimit;
            thread.supervision.retry_after = Some(900);
            thread.dependency_wait = Some("waiting on path".into());
            thread.rosen_path_wait = Some(RosenPathWait::new(vec!["src/lib.rs".into()], vec![]));
            thread.context_handoff = Some(Note {
                change: "a4d9b88".into(),
                text: "handoff".into(),
                pending: true,
            });
            thread.rosen_follow_ups = vec![
                RosenFollowUp::Deliver {
                    message: "fix review".into(),
                    mark: 0,
                },
                RosenFollowUp::FinishLanded,
            ];
            thread.rosen_follow_up_error = Some("prior error".into());
            thread.rosen_resolution_status =
                Some(RosenResolutionStatus::new("queued".into(), false));

            let error = check_resumable(key, thread.state, opening)
                .err()
                .unwrap_or(ResumeError::Other(String::new()));
            assert!(error.is_non_resumable());
            let text = error.to_string();
            assert!(text.contains("1/207"), "{text}");
            assert!(text.contains(expected_label), "{text}");

            assert!(drop_pending_deliveries(&mut thread));
            assert_eq!(thread.activity, AgentActivity::Idle);
            assert_eq!(thread.supervision.retry_after, None);
            assert_eq!(thread.dependency_wait, None);
            assert_eq!(thread.rosen_path_wait, None);
            assert!(
                thread
                    .context_handoff
                    .as_ref()
                    .is_some_and(|note| !note.pending)
            );
            assert_eq!(thread.rosen_follow_ups, vec![RosenFollowUp::FinishLanded]);
            assert_eq!(thread.rosen_follow_up_error, None);
            assert_eq!(thread.rosen_resolution_status, None);
            assert_eq!(
                decide_with_turn(&thread, &settings, due, false, false, false),
                Action::Nothing
            );
            assert_eq!(
                decide_with_turn(&thread, &settings, due, true, false, false),
                Action::Nothing
            );
            assert!(!drop_pending_deliveries(&mut thread));
        }
    }
}
