//! Your GitHub repositories for Open project (SPEC.md F2), and which repository a
//! local checkout's origin names.

use std::path::Path;

use crate::command::gh;
use crate::repository::{GitHubError, RepositoryName};

/// One repository the signed-in account owns or can see through an organisation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct GitHubRepository {
    /// `owner/name`.
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    /// When anything in the repository last changed, as GitHub reports it (RFC 3339).
    pub updated_at: String,
}

/// Repositories you own and repositories in organisations you belong to, most
/// recently updated first. One paginated `gh api` call covers both, so the list
/// never waits on one request per organisation.
pub fn list_your_repositories() -> Result<Vec<GitHubRepository>, GitHubError> {
    let listed = gh(&[
        "api",
        "--paginate",
        "user/repos?affiliation=owner,organization_member&sort=updated&per_page=100",
        "--jq",
        ".[] | {full_name, description, private, updated_at}",
    ])
    .output()
    .map_err(GitHubError::Spawn)?;
    if !listed.status.success() {
        return Err(GitHubError::ListFailed {
            stderr: String::from_utf8_lossy(&listed.stderr).trim().to_owned(),
        });
    }
    // `--jq` prints one JSON object per line across every page.
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|error| GitHubError::ListParse {
                message: error.to_string(),
            })
        })
        .collect()
}

/// The GitHub repository `folder`'s `origin` points at, or `None` when the folder
/// is not a checkout or its origin is not on github.com.
pub fn checkout_repository(folder: &Path) -> Option<RepositoryName> {
    let repository = gix::open(folder).ok()?;
    let remote = repository.find_remote("origin").ok()?;
    let url = remote.url(gix::remote::Direction::Fetch)?;
    if url.host() != Some("github.com") {
        return None;
    }
    // The path is `owner/name(.git)` for HTTPS and SSH, with or without a leading `/`.
    let path = url.path.to_string();
    RepositoryName::parse(path.trim_start_matches('/')).ok()
}
