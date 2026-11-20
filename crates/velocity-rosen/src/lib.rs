//! Rosen, Velocity's first-class version control (SPEC.md F5).
//!
//! Velocity starts and owns one Rosen server per project, imports the project's git
//! repository into it with GitHub as the mirror, and gives each agent a Rosen change and
//! workspace in place of a git worktree. Git worktrees are used only when a project
//! chooses git; Rosen failures retain a retryable launch.

mod agent;
pub mod agent_run;
pub mod bindings_check;
pub mod build_repairs;
pub mod bundled;
pub mod change_details;
pub mod checker;
mod checkout;
pub mod collapse_link;
mod conflicts;
pub mod connection;
pub mod dispatch;
mod events;
mod git_base;
pub mod integration;
mod issue;
pub mod landing;
pub mod machine_dispatch;
pub mod machines;
pub mod memory;
mod migration;
mod mirror;
mod mirror_catch_up;
pub mod policy;
pub mod post_land;
mod process;
pub mod project;
pub mod queued_turn;
pub mod release;
mod retry;
pub mod review_swarm;
mod server;
mod server_update;
mod stack;
pub mod status;
pub mod thread_change;
pub mod workflow_policy;
mod workspace;
pub mod workspace_settings;
mod workspace_wait;

pub use agent::{
    McpCommand, agent_environment, agent_name, mcp_command, path_with_rosen, run_queue_note,
    workspace_note,
};
pub use conflicts::{conflict_message, has_open_conflicts, regeneration_message};
pub use events::{
    ChangeEvent, EventHead, EventReplay, EventStream, change_event, conflict_paths,
    regenerate_paths, revert_output,
};
pub use events::{IssueEvent, issue_event};
pub use git_base::synchronize_git_base;
pub use issue::{issue_build_coverage, record_work_sources};
pub use migration::{Migration, migration_event_cursor};
pub use mirror::{MirrorLag, MirrorObservation, mirror_lag, mirror_remote, mirror_status};
pub use mirror_catch_up::{CatchUp, FastForward, adopt_mirror_tip, catch_up_from_mirror};
pub use retry::retry_workspace;
pub use rosen_client::Error as ClientError;
pub use rosen_client::protocol::Event as ProjectEvent;
pub use rosen_client::protocol::EventKind as ProjectEventKind;
pub use rosen_core::claim::PathPattern;
pub use server::{BINARY_FOLDER_ENV, ImportSource, ProjectServer, Servers, binary_folder};
pub use server_update::{RosenBuild, ServerUpdate};
pub use stack::stack_workspace;
pub use workspace::{
    AgentWorkspace, Watches, abandon_change, create_change, create_workspace,
    create_workspace_retaining_change, open_workspace_within, rebase_pending, reopen_workspace,
};
pub use workspace_wait::{CreationWait, creation_wait};

use std::path::PathBuf;

/// Why Rosen could not do what Velocity asked.
#[derive(Debug, thiserror::Error)]
pub enum RosenError {
    #[error("Could not read Rosen status: {0}")]
    StatusData(String),
    #[error("Migration to Rosen: {0}")]
    Migration(String),
    #[error("could not read Rosen thread change: {0}")]
    ThreadChange(String),
    #[error("could not read Rosen build repair evidence: {0}")]
    BuildRepairData(String),
    #[error("could not synchronize git with Rosen's base: {0}")]
    GitBase(String),
    #[error("could not read Rosen mirror metadata: {0}")]
    MirrorMetadata(#[from] rusqlite::Error),
    #[error("invalid Rosen mirror event: {0}")]
    MirrorData(String),
    #[error("Rosen refused: {0}")]
    MirrorRefused(String),
    #[error(
        "rosen-server was not found next to Velocity, in the folder {BINARY_FOLDER_ENV} names, or on PATH"
    )]
    MissingBinary,
    #[error("could not start rosen-server: {0}")]
    Start(String),
    #[error("Rosen's servers are stopped for an update or exit")]
    Stopped,
    #[error("{0}")]
    Machine(String),
    #[error("{0} is not a valid Rosen agent name")]
    AgentName(String),
    #[error("{0}")]
    Release(String),
    #[error("{0} is not a commit or tree id")]
    InvalidId(String),
    #[error(transparent)]
    Client(#[from] rosen_client::Error),
    #[error("created Rosen change {change} but could not prepare its workspace: {source}")]
    WorkspaceNotCreated {
        change: String,
        source: Box<rosen_client::Error>,
    },
    #[error(
        "Rosen made no workspace at {root} in {seconds} seconds, on the shared connection \
         or on one of its own"
    )]
    WorkspaceWait { root: PathBuf, seconds: u64 },
    #[error("the watcher on {0} ended as it started")]
    WatchEnded(PathBuf),
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}
