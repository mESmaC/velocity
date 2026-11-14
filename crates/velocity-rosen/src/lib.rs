//! Rosen, Velocity's first-class version control (SPEC.md F5).
//!
//! Velocity starts and owns one Rosen server per project, imports the project's git
//! repository into it with GitHub as the mirror, and gives each agent a Rosen change and
//! workspace in place of a git worktree. Git worktrees stay as the fallback when Rosen is
//! unavailable or a project chooses git.

mod agent;
mod process;
mod server;
mod workspace;

pub use agent::{agent_environment, agent_name, workspace_note};
pub use rosen_client::Error as ClientError;
pub use server::{BINARY_FOLDER_ENV, ImportSource, ProjectServer, Servers, binary_folder};
pub use workspace::{AgentWorkspace, Watches, abandon_change, create_workspace};

use std::path::PathBuf;

/// Why Rosen could not do what Velocity asked.
#[derive(Debug, thiserror::Error)]
pub enum RosenError {
    #[error(
        "rosen-server was not found next to Velocity, in the folder {BINARY_FOLDER_ENV} names, or on PATH"
    )]
    MissingBinary,
    #[error("could not start rosen-server: {0}")]
    Start(String),
    #[error("{0} is not a valid Rosen agent name")]
    AgentName(String),
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
