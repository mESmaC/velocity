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
    #[error("Choose a folder, not a drive: {0}")]
    DriveRoot(String),
    /// A refusal from the plan. Nothing has been created.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    GitHub(#[from] GitHubError),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    AgentsTemplate(#[from] velocity_git::agents_template::AgentsTemplateError),
    #[error(transparent)]
    AgentsFile(#[from] velocity_git::agents_file::AgentsFileError),
    /// The remote now exists, so a plain retry would be refused as RemoteAlreadyExists.
    #[error(
        "github.com/{repository} was created, but cloning it failed: {source}. Choose Existing repo to clone it again."
    )]
    CloneAfterCreate {
        repository: String,
        source: GitHubError,
    },
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Provider(#[from] velocity_store::providers::ProviderUnavailable),
    #[error("Could not save the new project: {0}")]
    Save(#[from] StoreError),
    #[error("{0} lock is poisoned")]
    Poisoned(&'static str),
    #[error("{0}")]
    StepOrder(&'static str),
    #[error("{0}")]
    Publish(String),
}
