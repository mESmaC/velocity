//! GitHub access through the `gh` CLI, signed in with the user's own `gh auth login`.
//! Every call runs `gh` as a child process with its arguments passed directly, never
//! through a shell.

mod command;
mod repository;

pub use repository::{
    GitHubError, RepositoryName, Visibility, clone_repository, create_repository, repository_exists,
};
