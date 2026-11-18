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
    let (path, mirror) = match location {
        ProjectLocationRequest::LocalFolder { path, .. } => (path, None),
        ProjectLocationRequest::RosenFolder { path, mirror } => (path, mirror_url(mirror)?),
    };
    folder(path, NewProjectLocation::NewFolder, mirror)
}

fn mirror_url(mirror: &ProjectMirror) -> Result<Option<String>, NewProjectError> {
    let (input, host) = match mirror {
        ProjectMirror::None => return Ok(None),
        ProjectMirror::GitHub { repository } => (repository, "https://github.com/"),
        ProjectMirror::Collapse { repository } => (repository, "https://api.collapse.sh/"),
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
            let explicit_destination = !clone_to.trim().is_empty();
            let parent = if explicit_destination {
                absolute_folder(clone_to)?
            } else {
                projects_directory(&std::env::home_dir().ok_or(NewProjectError::NoFolder)?)
            };
            let path = parent.join(repository.name());
            validate_project_root(&path)?;
            Ok(ResolvedRequest {
                location: NewProjectLocation::OpenGitHub,
                path,
                name: repository.name().to_owned(),
                repository: Some(repository),
                mirror: None,
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
    let repository = resolved
        .repository
        .as_ref()
        .map(RepositoryName::full_name)
        .unwrap_or_default();
    match refusal {
        Refusal::NotARepository => format!(
            "{path} has no Rosen project or Git checkout. Use New project to set up the folder."
        ),
        Refusal::TargetOccupied => format!(
            "{path} already exists and is not a clone of github.com/{repository}. Choose another clone folder."
        ),
        Refusal::AlreadyAProject => format!("{path} is already a project. Switch to it instead."),
        Refusal::RemoteMissing => {
            format!("github.com/{repository} was not found, or your gh account cannot see it.")
        }
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
