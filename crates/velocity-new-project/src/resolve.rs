//! Turns the dialog's request into a path, name and repository, and phrases the
//! plan's refusals for the user.

use std::path::{Path, PathBuf};
use velocity_core::new_project::{NewProjectLocation, Refusal};
use velocity_github::{RepositoryName, Visibility};

use crate::error::NewProjectError;
use crate::request::{ProjectLocationRequest, RepositoryVisibility};

/// The request with its path, name and repository worked out.
pub(crate) struct ResolvedRequest {
    pub(crate) location: NewProjectLocation,
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) repository: Option<RepositoryName>,
    pub(crate) visibility: Visibility,
}

impl ResolvedRequest {
    pub(crate) fn path_text(&self) -> String {
        self.path.display().to_string()
    }
}

pub(crate) fn resolve(
    location: &ProjectLocationRequest,
) -> Result<ResolvedRequest, NewProjectError> {
    match location {
        ProjectLocationRequest::LocalFolder {
            path,
            create_if_needed,
        } => {
            let path = absolute_folder(path)?;
            Ok(ResolvedRequest {
                location: NewProjectLocation::LocalFolder {
                    create_if_needed: *create_if_needed,
                },
                name: folder_name(&path)?,
                path,
                repository: None,
                visibility: Visibility::Private,
            })
        }
        ProjectLocationRequest::GitHubNew {
            repository,
            visibility,
            clone_to,
        } => {
            let visibility = match visibility {
                RepositoryVisibility::Private => Visibility::Private,
                RepositoryVisibility::Public => Visibility::Public,
            };
            resolve_github(
                NewProjectLocation::GitHubNew,
                repository,
                clone_to,
                visibility,
            )
        }
        ProjectLocationRequest::GitHubExisting {
            repository,
            clone_to,
        } => resolve_github(
            NewProjectLocation::GitHubExisting,
            repository,
            clone_to,
            Visibility::Private,
        ),
    }
}

fn resolve_github(
    location: NewProjectLocation,
    repository: &str,
    clone_to: &str,
    visibility: Visibility,
) -> Result<ResolvedRequest, NewProjectError> {
    let repository = RepositoryName::parse(repository)?;
    // The clone gets its own folder named after the repository, as `git clone` does.
    let path = absolute_folder(clone_to)?.join(repository.name());
    Ok(ResolvedRequest {
        location,
        path,
        name: repository.name().to_owned(),
        repository: Some(repository),
        visibility,
    })
}

fn absolute_folder(text: &str) -> Result<PathBuf, NewProjectError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(NewProjectError::NoFolder);
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(NewProjectError::RelativePath(trimmed.to_owned()));
    }
    Ok(path)
}

fn folder_name(path: &Path) -> Result<String, NewProjectError> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| NewProjectError::DriveRoot(path.display().to_string()))
}

pub(crate) fn refusal_message(refusal: Refusal, resolved: &ResolvedRequest) -> String {
    let path = resolved.path.display();
    let repository = resolved
        .repository
        .as_ref()
        .map(RepositoryName::full_name)
        .unwrap_or_default();
    match refusal {
        Refusal::NotARepository => format!(
            "{path} is not a git repository. Turn on \"Create the folder and run git init if needed\"."
        ),
        Refusal::AlreadyAProject => format!("{path} is already a project. Switch to it instead."),
        Refusal::RemoteAlreadyExists => {
            format!("github.com/{repository} already exists. Choose Existing repo to clone it.")
        }
        Refusal::RemoteMissing => {
            format!("github.com/{repository} was not found, or your gh account cannot see it.")
        }
        Refusal::TargetOccupied => {
            format!("{path} already exists and is not empty. Choose another clone folder.")
        }
    }
}
