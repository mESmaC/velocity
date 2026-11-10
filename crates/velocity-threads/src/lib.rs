//! Starting and running build threads (SPEC.md F4, F5, §3.2): a thread's name and worktree,
//! its agent session, the live transcript the thread view shows, and deleting it (F12).

#![forbid(unsafe_code)]

pub mod deletion;
pub mod dependencies;
pub mod dispatch;
pub mod follow_through;
pub mod goal_swarm;
pub mod launch;
pub mod naming;
pub mod review;
pub mod review_runner;
pub mod runtime;
pub mod swarm;
pub mod sweep;
pub mod sweep_runner;
pub mod titles;
pub mod transcript_view;
pub mod work_pool;

pub use deletion::{DeletionError, DeletionPlan, PendingDeletions, plan_deletion};
pub use dispatch::{DispatchError, IssueDispatchPlan, plan_issue_dispatch};
pub use follow_through::DeliveryStatus;
pub use launch::{
    LaunchError, LaunchPlan, WorktreeNaming, agent_launch, plan_launch, title_worktree_name,
};
pub use review::{PR_DUTY_SERVER_SWITCH, ReviewPlan, plan_review};
pub use review_runner::{PrDutyReviews, ReviewChange, ReviewHost};
pub use runtime::{
    RunningSession, SessionInputs, SteerError, StopOutcome, ThreadKey, ThreadProgress,
    ThreadRuntimes, spawn_session,
};
pub use sweep::{SCOUT_SERVER_SWITCH, SweepPlan, plan_sweep};
pub use sweep_runner::{ScoutChange, ScoutSweeps, SweepHost};
pub use titles::{TitlePlan, plan_title, resolve_title, thread_run_settings};
pub use transcript_view::LiveTranscript;
