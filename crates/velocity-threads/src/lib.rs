//! Starting and running build threads (SPEC.md F4, F5, §3.2): a thread's name and worktree,
//! its agent session, and the live transcript the thread view shows.

#![forbid(unsafe_code)]

pub mod launch;
pub mod naming;
pub mod runtime;
pub mod titles;
pub mod transcript_view;

pub use launch::{
    LaunchError, LaunchPlan, WorktreeNaming, agent_launch, plan_launch, title_worktree_name,
};
pub use runtime::{
    RunningSession, SessionInputs, SteerError, ThreadKey, ThreadProgress, ThreadRuntimes,
    spawn_session,
};
pub use titles::{TitlePlan, plan_title, resolve_title};
pub use transcript_view::LiveTranscript;
