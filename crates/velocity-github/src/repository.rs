use std::io;
use std::path::Path;
use std::process::Output;

use crate::command::gh;

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error("could not run gh: {0}")]
    Spawn(#[source] io::Error),
    #[error("{text:?} is not a GitHub repository name; use owner/name")]
    InvalidName { text: String },
    #[error("gh {operation} failed for {repository}: {stderr}")]
    Failed {
        operation: &'static str,
        repository: String,
        stderr: String,
    },
    #[error("gh could not list your repositories: {stderr}")]
    ListFailed { stderr: String },
    #[error("gh listed your repositories in a form Velocity cannot read: {message}")]
    ListParse { message: String },
}

/// `github.com/<owner>/<name>`, as typed in the New project dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryName {
    owner: String,
    name: String,
}

impl RepositoryName {
    /// Accepts `owner/name`, optionally pasted as `github.com/owner/name`, an
    /// `https://` URL, or with a trailing `.git`.
    pub fn parse(text: &str) -> Result<Self, GitHubError> {
        let invalid = || GitHubError::InvalidName {
            text: text.to_owned(),
        };
        let trimmed = text.trim();
        let without_scheme = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("http://"))
            .unwrap_or(trimmed);
        let without_host = without_scheme
            .strip_prefix("github.com/")
            .unwrap_or(without_scheme);
        let without_suffix = without_host.trim_end_matches('/');
        let bare = without_suffix
            .strip_suffix(".git")
            .unwrap_or(without_suffix);
        let (owner, name) = bare.split_once('/').ok_or_else(invalid)?;
        if is_owner(owner) && is_repository_name(name) {
            Ok(Self {
                owner: owner.to_owned(),
                name: name.to_owned(),
            })
        } else {
            Err(invalid())
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// GitHub user and organisation names: letters, digits and single inner hyphens.
fn is_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && !owner.contains("--")
        && owner
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

/// GitHub repository names: letters, digits, `.`, `-` and `_`, but not `.` or `..`.
fn is_repository_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Public,
}

/// Whether the repository exists and the signed-in account can see it. Any other
/// failure (signed out, offline) is an error, not "missing".
pub fn repository_exists(repository: &RepositoryName) -> Result<bool, GitHubError> {
    let viewed = gh(&["repo", "view", &repository.full_name(), "--json", "name"])
        .output()
        .map_err(GitHubError::Spawn)?;
    if viewed.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&viewed.stderr);
    if stderr.contains("Could not resolve to a Repository") {
        Ok(false)
    } else {
        Err(failure("repo view", repository, &viewed))
    }
}

pub fn create_repository(
    repository: &RepositoryName,
    visibility: Visibility,
) -> Result<(), GitHubError> {
    let visibility_flag = match visibility {
        Visibility::Private => "--private",
        Visibility::Public => "--public",
    };
    let created = gh(&["repo", "create", &repository.full_name(), visibility_flag])
        .output()
        .map_err(GitHubError::Spawn)?;
    succeeded("repo create", repository, &created)
}

/// `gh repo clone` into `target`, with long paths on in the clone so it works past
/// 260 characters on Windows.
pub fn clone_repository(repository: &RepositoryName, target: &Path) -> Result<(), GitHubError> {
    let cloned = gh(&["repo", "clone", &repository.full_name()])
        .arg(target)
        .args(["--", "--config", "core.longpaths=true"])
        .output()
        .map_err(GitHubError::Spawn)?;
    succeeded("repo clone", repository, &cloned)
}

fn succeeded(
    operation: &'static str,
    repository: &RepositoryName,
    output: &Output,
) -> Result<(), GitHubError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(failure(operation, repository, output))
    }
}

fn failure(operation: &'static str, repository: &RepositoryName, output: &Output) -> GitHubError {
    GitHubError::Failed {
        operation,
        repository: repository.full_name(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}
