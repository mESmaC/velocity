//! Starting and running build threads (SPEC.md F4, F5, §3.2): a thread's name and worktree,
//! its agent session, the live transcript the thread view shows, and deleting it (F12).

#![forbid(unsafe_code)]

pub mod deletion;
pub mod dispatch;
pub mod launch;
pub mod naming;
pub mod runtime;
pub mod transcript_view;

pub use deletion::{DeletionError, DeletionPlan, PendingDeletions, plan_deletion};
pub use dispatch::{DispatchError, IssueDispatchPlan, plan_issue_dispatch};
pub use launch::{LaunchError, LaunchPlan, agent_launch, plan_launch};
pub use runtime::{
    RunningSession, SessionInputs, SteerError, StopOutcome, ThreadKey, ThreadProgress,
    ThreadRuntimes, spawn_session,
};
pub use transcript_view::LiveTranscript;
