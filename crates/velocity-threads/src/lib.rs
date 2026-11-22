//! Starting and running build threads (SPEC.md F4, F5, §3.2): a thread's name and worktree,
//! its agent session, the live transcript the thread view shows, and deleting it (F12).

#![forbid(unsafe_code)]

pub mod agent_activity;
pub mod agent_memory;
pub mod agent_tasks;
pub mod build_repairs;
pub mod ci_repairs;
pub mod continuous_dispatch;
pub mod deletion;
pub mod dependencies;
pub mod deploy_repairs;
pub mod dispatch;
pub mod follow_through;
pub mod goal_selection;
pub mod goal_swarm;
pub mod improvement_scout;
pub mod issue_activity;
pub mod landing_runner;
pub mod launch;
pub mod local_release;
pub mod machine_worker;
pub mod naming;
pub mod next_work;
pub mod overlap_paths;
pub mod parked_review;
pub mod parking;
mod process;
pub mod restart;
mod review_swarm;
mod review_swarm_server;
mod review_swarm_session;
pub mod runtime;
pub mod shared_mcp;
pub mod subagents;
pub mod swarm;
pub mod sweep;
pub mod sweep_runner;
pub mod task_brief;
mod task_spec;
pub mod titles;
pub mod transcript_saves;
pub mod transcript_view;
pub mod work_pool;
pub mod workspace_retry;

pub use deletion::{Deletion, PendingDeletions};
pub use dispatch::{DispatchError, IssueDispatchPlan, plan_issue_dispatch};
pub use follow_through::DeliveryStatus;
pub use improvement_scout::{
    IMPROVEMENT_SCOUT_SWITCH, ImprovementFleetSnapshot, ImprovementScoutServer, ImprovementScouts,
    TargetRepositoryConfig,
};
pub use launch::{
    LaunchError, LaunchPlan, QuestionPlan, agent_launch, agent_launch_for_account, plan_launch,
    plan_launch_with_preferences, plan_question_launch, project_workspace_root,
};
pub use review_swarm_server::{REVIEW_SWARM_SWITCH, ReviewSwarmServer};
pub use runtime::{
    RunningSession, SessionInputs, SteerError, StopOutcome, ThreadKey, ThreadProgress,
    ThreadRuntimes, spawn_session,
};
pub use shared_mcp::shared_mcp_servers;
pub use sweep::SCOUT_SERVER_SWITCH;
pub use sweep_runner::{ScoutChange, ScoutSweeps, SweepHost};
pub use titles::{TitlePlan, plan_title, resolve_title, thread_run_settings};
pub use transcript_view::LiveTranscript;
