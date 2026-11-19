//! GitHub access through the `gh` CLI, signed in with the user's own `gh auth login`.
//! Every call runs `gh` as a child process with its arguments passed directly, never
//! through a shell. Velocity uses GitHub only for issues (F9, scouts, post-land failure
//! reports) and for listing and cloning repositories (F2); changes land through Rosen.

mod command;
mod dependency_graph;
mod gh_command;
mod issue_panel;
mod issue_types;
mod issues;
pub mod mcp_stdio;
pub mod post_land_failures;
mod release;
mod repository;
mod repository_client;
mod repository_list;
mod request_counter;
mod rosen_issue;
mod scout_directory;
mod scout_evidence;
mod scout_issues;
mod scout_reference;
mod scout_search;
mod scout_server;
pub mod velocity_reports;

pub use dependency_graph::DependencyGraph;
pub use gh_command::GitHubCommandError;
pub use issue_panel::{
    IssueDiscussion, IssuePanelCache, IssuePanelRow, IssuePanelSnapshot, SpecQuestion,
};
pub use issue_types::*;
pub use issues::IssueClient;
pub use repository::{
    GitHubError, RepositoryName, Visibility, clone_repository, create_repository, repository_exists,
};
pub use repository_list::{GitHubRepository, checkout_repository, list_your_repositories};
pub use request_counter::{RequestTotal, request_totals};
pub use rosen_issue::RosenIssueEffect;
pub use scout_evidence::Reproduction;
pub use scout_issues::{ScoutFinding, ScoutIssueError, ScoutIssueOutcome};
pub use scout_reference::{ReferenceComparison, ReferenceComparisonResult};
pub use scout_server::{ScoutReport, ScoutServer, filed_message, read_scout_reports};
