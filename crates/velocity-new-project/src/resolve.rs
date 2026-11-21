//! Resolves folder paths and mirror choices for F2.

use std::path::{Path, PathBuf};
use velocity_core::new_project::{NewProjectLocation, Refusal};
use velocity_github::RepositoryName;

use crate::error::NewProjectError;
use crate::project_root::validate_project_root;
use crate::request::{
    OpenProjectRequest, ProjectLocationRequest, ProjectMirror, projects_directory,
};

pub(crate) struct ResolvedRequest {
    pub(crate) location: NewProjectLocation,
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) repository: Option<RepositoryName>,
    pub(crate) collapse_repository: Option<String>,
    pub(crate) github_repository: Option<RepositoryName>,
    pub(crate) mirror: Option<String>,
    pub(crate) explicit_destination: bool,
}

impl ResolvedRequest {
    pub(crate) fn path_text(&self) -> String {
        self.path.display().to_string()
    }
}

pub(crate) fn resolve(
    location: &ProjectLocationRequest,
) -> Result<ResolvedRequest, NewProjectError> {
    let (path, mirror, github_repository) = match location {
        ProjectLocationRequest::LocalFolder { path, .. } => (path, None, None),
        ProjectLocationRequest::RosenFolder { path, mirror } => {
            let github_repository = match mirror {
                ProjectMirror::Collapse {
                    github_repository: Some(text),
                    ..
                } => parse_github_repository(text)?,
                _ => None,
            };
            (path, mirror_url(mirror)?, github_repository)
        }
    };
    let mut resolved = folder(path, NewProjectLocation::NewFolder, mirror)?;
    resolved.github_repository = github_repository;
    Ok(resolved)
}

/// Parses an optional GitHub `owner/repository` (or GitHub HTTPS URL) for issues and CI (#574).
pub fn parse_github_repository(input: &str) -> Result<Option<RepositoryName>, NewProjectError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let name = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("github.com/"))
        .unwrap_or(trimmed);
    // Keep credentials and arbitrary URLs out of the project configuration.
    if name.contains([':', '@', '?', '#', '\\']) {
        return Err(NewProjectError::Refused(
            "Enter a GitHub owner/repository or https://github.com/ repository URL.".into(),
        ));
    }
    Ok(Some(RepositoryName::parse(name)?))
}

fn mirror_url(mirror: &ProjectMirror) -> Result<Option<String>, NewProjectError> {
    let (input, host) = match mirror {
        ProjectMirror::None => return Ok(None),
        ProjectMirror::GitHub { repository } => (repository, "https://github.com/"),
        ProjectMirror::Collapse { repository, .. } => (repository, "https://api.collapse.sh/"),
    };
    let trimmed = input.trim();
    let name = trimmed.strip_prefix(host).unwrap_or(trimmed);
    // Keep credentials and arbitrary URLs out of the project configuration.
    if name.contains([':', '@', '?', '#', '\\']) {
        return Err(NewProjectError::Refused(format!(
            "Enter an owner/repository or a {host} repository URL."
        )));
    }
    let repository = RepositoryName::parse(name)?;
    Ok(Some(format!("{host}{}.git", repository.full_name())))
}

/// Validates the Collapse repository name and destination folder before calling Collapse (#575).
pub fn validate_open_collapse(
    repository: &str,
    clone_to: &str,
) -> Result<(String, String), NewProjectError> {
    let (owner, name) = parse_collapse_repository(repository)?;
    let _ = resolve_clone_destination(&name, clone_to)?;
    Ok((owner, name))
}

fn parse_collapse_repository(repository: &str) -> Result<(String, String), NewProjectError> {
    velocity_core::collapse::parse_repository(repository).ok_or_else(|| {
        NewProjectError::Refused(format!(
            "{repository:?} is not a Collapse repository name; use owner/name"
        ))
    })
}

fn resolve_clone_destination(
    name: &str,
    clone_to: &str,
) -> Result<(PathBuf, bool), NewProjectError> {
    let explicit_destination = !clone_to.trim().is_empty();
    let parent = if explicit_destination {
        absolute_folder(clone_to)?
    } else {
        projects_directory(&std::env::home_dir().ok_or(NewProjectError::NoFolder)?)
    };
    let path = parent.join(name);
    validate_project_root(&path)?;
    Ok((path, explicit_destination))
}

pub(crate) fn resolve_open(
    request: &OpenProjectRequest,
) -> Result<ResolvedRequest, NewProjectError> {
    match request {
        OpenProjectRequest::LocalFolder { path } => {
            folder(path, NewProjectLocation::OpenFolder, None)
        }
        OpenProjectRequest::GitHub {
            repository,
            clone_to,
        } => {
            let repository = RepositoryName::parse(repository)?;
            let (path, explicit_destination) =
                resolve_clone_destination(repository.name(), clone_to)?;
            Ok(ResolvedRequest {
                location: NewProjectLocation::OpenGitHub,
                path,
                name: repository.name().to_owned(),
                repository: Some(repository),
                collapse_repository: None,
                github_repository: None,
                mirror: None,
                explicit_destination,
            })
        }
        OpenProjectRequest::Collapse {
            repository,
            clone_to,
        } => {
            let (owner, name) = parse_collapse_repository(repository)?;
            let full_name = format!("{owner}/{name}");
            let mirror = velocity_core::collapse::mirror_remote(&full_name).ok_or_else(|| {
                NewProjectError::Refused(format!(
                    "{repository:?} is not a Collapse repository name; use owner/name"
                ))
            })?;
            let (path, explicit_destination) = resolve_clone_destination(&name, clone_to)?;
            Ok(ResolvedRequest {
                location: NewProjectLocation::OpenCollapse,
                path,
                name,
                repository: None,
                collapse_repository: Some(full_name),
                github_repository: None,
                mirror: Some(mirror),
                explicit_destination,
            })
        }
    }
}

fn folder(
    text: &str,
    location: NewProjectLocation,
    mirror: Option<String>,
) -> Result<ResolvedRequest, NewProjectError> {
    // Local setup always takes the requested path. The dialog's suggestion is a placeholder.
    let path = absolute_folder(text)?;
    validate_project_root(&path)?;
    let name = folder_name(&path)?;
    Ok(ResolvedRequest {
        location,
        path,
        name,
        repository: None,
        collapse_repository: None,
        github_repository: None,
        mirror,
        explicit_destination: true,
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
    let (host, repository) = match (&resolved.repository, &resolved.collapse_repository) {
        (_, Some(collapse)) => ("collapse.sh", collapse.clone()),
        (Some(github), None) => ("github.com", github.full_name()),
        (None, None) => ("github.com", String::new()),
    };
    match refusal {
        Refusal::NotARepository => format!(
            "{path} has no Rosen project or Git checkout. Use New project to set up the folder."
        ),
        Refusal::TargetOccupied => format!(
            "{path} already exists and is not a clone of {host}/{repository}. Choose another clone folder."
        ),
        Refusal::AlreadyAProject => format!("{path} is already a project. Switch to it instead."),
        Refusal::RemoteMissing => match resolved.location {
            NewProjectLocation::OpenCollapse => {
                format!(
                    "{host}/{repository} was not found, or your Collapse account cannot see it."
                )
            }
            _ => format!("{host}/{repository} was not found, or your gh account cannot see it."),
        },
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::request::new_project_defaults;

    // The old dialog submitted the home-folder default, and setup initialized it.
    #[test]
    fn create_and_open_refuse_home_and_drive_roots() -> Result<(), NewProjectError> {
        let home = std::env::home_dir().ok_or(NewProjectError::NoFolder)?;
        for path in [
            home.clone(),
            home.join("Projects").join(".."),
            r"C:\".into(),
            r"D:\".into(),
        ] {
            let path = path.display().to_string();
            let create = resolve(&ProjectLocationRequest::RosenFolder {
                path: path.clone(),
                mirror: ProjectMirror::None,
            });
            let open = resolve_open(&OpenProjectRequest::LocalFolder { path });
            for result in [create, open] {
                assert!(matches!(
                    result,
                    Err(NewProjectError::ProtectedFolder { .. } | NewProjectError::DriveRoot(_))
                ));
            }
        }
        let defaults = new_project_defaults(&home, Default::default());
        assert_eq!(Path::new(&defaults.folder), projects_directory(&home));
        Ok(())
    }

    #[test]
    fn github_refuses_a_new_checkout_under_appdata() -> Result<(), NewProjectError> {
        let clone_to = std::env::var("APPDATA").map_err(|_| NewProjectError::NoFolder)?;
        let result = resolve_open(&OpenProjectRequest::GitHub {
            repository: "owner/velocity-project-root-check".into(),
            clone_to,
        });
        assert!(matches!(
            result,
            Err(NewProjectError::ProtectedFolder { .. })
        ));
        Ok(())
    }
}
