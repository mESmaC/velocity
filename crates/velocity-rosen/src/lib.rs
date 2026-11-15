//! Rosen, Velocity's first-class version control (SPEC.md F5).
//!
//! Velocity starts and owns one Rosen server per project, imports the project's git
//! repository into it with GitHub as the mirror, and gives each agent a Rosen change and
//! workspace in place of a git worktree. Git worktrees stay as the fallback when Rosen is
//! unavailable or a project chooses git.

mod agent;
#[cfg(feature = "bundled")]
mod bundled;
pub mod checker;
mod conflicts;
pub mod dispatch;
pub mod duty;
mod events;
mod issue;
pub mod machine_dispatch;
pub mod machines;
mod mirror;
pub mod policy;
pub mod post_land;
mod process;
pub mod review_swarm;
mod server;
mod stack;
mod workspace;

pub use agent::{
    McpCommand, agent_environment, agent_name, mcp_command, path_with_rosen, run_queue_note,
    workspace_note,
};
pub use conflicts::{conflict_message, has_open_conflicts, regeneration_message};
pub use events::{
    ChangeEvent, EventStream, change_event, conflict_paths, regenerate_paths, revert_output,
};
pub use events::{IssueEvent, issue_event};
pub use issue::{issue_build_coverage, record_issue};
pub use mirror::{MirrorObservation, mirror_status};
pub use rosen_client::Error as ClientError;
pub use rosen_client::protocol::Event as ProjectEvent;
pub use rosen_client::protocol::EventKind as ProjectEventKind;
pub use server::{BINARY_FOLDER_ENV, ImportSource, ProjectServer, Servers, binary_folder};
pub use stack::stack_workspace;
pub use workspace::{
    AgentWorkspace, Watches, abandon_change, create_workspace, rebase_pending, reopen_workspace,
};

use std::path::PathBuf;

/// Why Rosen could not do what Velocity asked.
#[derive(Debug, thiserror::Error)]
pub enum RosenError {
    #[error("could not read Rosen mirror metadata: {0}")]
    MirrorMetadata(#[from] rusqlite::Error),
    #[error("invalid Rosen mirror event: {0}")]
    MirrorData(String),
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
    #[error("{0} is not a commit or tree id")]
    InvalidId(String),
    #[error(transparent)]
    Client(#[from] rosen_client::Error),
    #[error("the watcher on {0} ended as it started")]
    WatchEnded(PathBuf),
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}
