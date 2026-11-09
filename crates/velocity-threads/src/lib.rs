//! Starting and running build threads (SPEC.md F4, F5, §3.2): a thread's name and worktree,
//! its agent session, and the live transcript the thread view shows.

#![forbid(unsafe_code)]

pub mod follow_through;
pub mod launch;
pub mod naming;
pub mod runtime;
pub mod transcript_view;

pub use follow_through::DeliveryStatus;
pub use launch::{LaunchError, LaunchPlan, agent_launch, plan_launch};
pub use runtime::{
    RunningSession, SessionInputs, SteerError, StopOutcome, ThreadKey, ThreadProgress,
    ThreadRuntimes, spawn_session,
};
pub use transcript_view::LiveTranscript;
