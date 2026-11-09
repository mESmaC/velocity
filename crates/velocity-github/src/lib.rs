//! GitHub access through the `gh` CLI, signed in with the user's own `gh auth login`.
//! Every call runs `gh` as a child process with its arguments passed directly, never
//! through a shell.

mod command;
mod pull_request_command;
mod pull_request_mutations;
mod pull_request_panel;
mod pull_request_queries;
mod pull_request_reviews;
mod pull_request_types;
mod pull_requests;
mod repository;

pub use pull_request_command::PullRequestError;
pub use pull_request_panel::{
    CheckStatus, PullRequestPanelRow, PullRequestPanelSnapshot, PullRequestThread, ReviewStatus,
};
pub use pull_request_types::*;
pub use pull_requests::PullRequestClient;
pub use repository::{
    GitHubError, RepositoryName, Visibility, clone_repository, create_repository, repository_exists,
};
