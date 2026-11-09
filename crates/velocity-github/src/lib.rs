//! GitHub access through the `gh` CLI, signed in with the user's own `gh auth login`.
//! Every call runs `gh` as a child process with its arguments passed directly, never
//! through a shell.

mod command;
mod issue_panel;
mod issue_types;
mod issues;
mod pull_request_command;
mod pull_request_mutations;
mod pull_request_panel;
mod pull_request_queries;
mod pull_request_reviews;
mod pull_request_types;
mod pull_requests;
mod repository;
mod scout_directory;
mod scout_evidence;
mod scout_issues;

pub use issue_panel::{IssuePanelRow, IssuePanelSnapshot, SpecQuestion};
pub use issue_types::*;
pub use issues::IssueClient;
pub use pull_request_command::PullRequestError;
pub use pull_request_panel::{
    CheckStatus, PullRequestPanelRow, PullRequestPanelSnapshot, PullRequestThread, ReviewStatus,
};
pub use pull_request_types::*;
pub use pull_requests::PullRequestClient;
pub use repository::{
    GitHubError, RepositoryName, Visibility, clone_repository, create_repository, repository_exists,
};
pub use scout_evidence::Reproduction;
pub use scout_issues::{ScoutFinding, ScoutIssueError, ScoutIssueOutcome};
