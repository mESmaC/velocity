use std::io;
use std::path::PathBuf;

use thiserror::Error;
use velocity_git::GitError;
use velocity_github::GitHubError;
use velocity_store::StoreError;
use velocity_store::projects::ProjectError;

/// Why Create stopped. Every message is shown to the user in the dialog.
#[derive(Debug, Error)]
pub enum NewProjectError {
    #[error("Choose a folder for the project")]
    NoFolder,
    #[error("Use a full path, starting with a drive letter: {0}")]
    RelativePath(String),
    #[error("Cannot use a drive or share root as a project folder: {0}")]
    DriveRoot(String),
    #[error("Cannot use {path} as a project folder because {reason}. Choose another folder.", path = .path.display())]
    ProtectedFolder { path: PathBuf, reason: &'static str },
    #[error("Could not check the project folder {path}: {source}", path = .path.display())]
    InspectFolder { path: PathBuf, source: io::Error },
    /// A refusal from the plan. Nothing has been created.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    GitHub(#[from] GitHubError),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Rosen(#[from] velocity_rosen::project::ProjectError),
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Provider(#[from] velocity_store::providers::ProviderUnavailable),
    #[error("Could not save the new project: {0}")]
    Save(#[from] StoreError),
    #[error("{0} lock is poisoned")]
    Poisoned(&'static str),
    #[error("{0}")]
    Publish(String),
}
