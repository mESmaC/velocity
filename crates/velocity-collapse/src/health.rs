//! What Collapse reports about the account's repositories: last land and the downstream
//! forwarder (Collapse C2, C4; #427). One dashboard read with the read-only personal token
//! covers every project, so Velocity polls it rather than each repository.

use std::time::Duration;

use reqwest::{Client, Method};
use serde::Deserialize;

use crate::CollapseError;
use crate::credential::CollapseCredentials;
use crate::provision::{authorized, failed, success};
use crate::url_text::encode;

const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// A downstream remote reference Collapse may return as a string or an object (#574).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum DownstreamRef {
    Text(String),
    Object(DownstreamObject),
}

#[derive(Clone, Debug, Deserialize)]
pub struct DownstreamObject {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl DownstreamRef {
    fn as_remote(&self) -> Option<String> {
        match self {
            Self::Text(text) => non_empty(text),
            Self::Object(object) => first_non_empty([
                object.remote.as_deref(),
                object.remote_url.as_deref(),
                object.url.as_deref(),
                object.repository.as_deref(),
                object.full_name.as_deref(),
            ])
            .or_else(|| {
                let owner = object.owner.as_deref().and_then(non_empty)?;
                let name = object.name.as_deref().and_then(non_empty)?;
                Some(format!("{owner}/{name}"))
            }),
        }
    }
}

/// One repository's row as Collapse's dashboard (and MCP `list_repositories`) gives it.
#[derive(Clone, Debug, Deserialize)]
pub struct RepositoryActivity {
    pub owner: String,
    pub name: String,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub last_land: Option<LastLand>,
    pub forward: Forward,
    #[serde(default)]
    pub downstream: Option<DownstreamRef>,
    #[serde(default)]
    pub downstream_remote: Option<String>,
    #[serde(default)]
    pub downstream_url: Option<String>,
    #[serde(default)]
    pub downstream_repository: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LastLand {
    pub time: String,
    #[serde(default)]
    pub agent: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Forward {
    /// `unconfigured`, `synced`, `catching_up`, `retrying`, `stopped` or `unavailable`.
    pub state: String,
    #[serde(default)]
    pub behind: i64,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub next_attempt: Option<String>,
    #[serde(default)]
    pub downstream: Option<DownstreamRef>,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Deserialize)]
struct Dashboard {
    repositories: Vec<RepositoryActivity>,
}

#[derive(Deserialize)]
struct RepositoryDetail {
    #[serde(default)]
    forward: Option<Forward>,
    #[serde(default)]
    downstream: Option<DownstreamRef>,
    #[serde(default)]
    downstream_remote: Option<String>,
    #[serde(default)]
    downstream_url: Option<String>,
    #[serde(default)]
    downstream_repository: Option<String>,
}

impl Forward {
    /// The downstream remote URL or `owner/repository` reported on the forwarder, if any.
    pub fn downstream_remote(&self) -> Option<String> {
        self.downstream
            .as_ref()
            .and_then(DownstreamRef::as_remote)
            .or_else(|| {
                first_non_empty([
                    self.remote.as_deref(),
                    self.remote_url.as_deref(),
                    self.repository.as_deref(),
                    self.target.as_deref(),
                    self.url.as_deref(),
                ])
            })
    }
}

impl RepositoryActivity {
    /// Whether this row is `owner/name`; Collapse names are unique ignoring case.
    pub fn is(&self, repository: &str) -> bool {
        repository.split_once('/').is_some_and(|(owner, name)| {
            self.owner.eq_ignore_ascii_case(owner) && self.name.eq_ignore_ascii_case(name)
        })
    }

    /// The repository's configured downstream remote, when Collapse's dashboard reports it.
    pub fn downstream_remote(&self) -> Option<String> {
        self.forward
            .downstream_remote()
            .or_else(|| self.downstream.as_ref().and_then(DownstreamRef::as_remote))
            .or_else(|| {
                first_non_empty([
                    self.downstream_remote.as_deref(),
                    self.downstream_url.as_deref(),
                    self.downstream_repository.as_deref(),
                ])
            })
    }
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn first_non_empty<const N: usize>(candidates: [Option<&str>; N]) -> Option<String> {
    candidates.into_iter().flatten().find_map(non_empty)
}

/// Every repository the connected account can see, with its last land and forwarder.
pub async fn repository_activity(
    credentials: &CollapseCredentials,
) -> Result<Vec<RepositoryActivity>, CollapseError> {
    let client = Client::builder()
        .timeout(READ_TIMEOUT)
        .build()
        .map_err(failed)?;
    let response = authorized(
        &client,
        Method::GET,
        "/api/dashboard",
        &credentials.mcp_token,
    )
    .send()
    .await
    .map_err(failed)?;
    let dashboard: Dashboard = success(response, "read your repositories")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok(dashboard.repositories)
}

/// Reads a single Collapse repository to check its configured downstream remote (#574).
pub async fn repository_downstream(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<String>, CollapseError> {
    let client = Client::builder()
        .timeout(READ_TIMEOUT)
        .build()
        .map_err(failed)?;
    let path = format!("/api/repositories/{}/{}", encode(owner), encode(name));
    let response = authorized(&client, Method::GET, &path, &credentials.mcp_token)
        .send()
        .await
        .map_err(failed)?;
    let detail: RepositoryDetail = success(response, "read the repository")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok(detail
        .forward
        .as_ref()
        .and_then(Forward::downstream_remote)
        .or_else(|| {
            detail
                .downstream
                .as_ref()
                .and_then(DownstreamRef::as_remote)
        })
        .or_else(|| {
            first_non_empty([
                detail.downstream_remote.as_deref(),
                detail.downstream_url.as_deref(),
                detail.downstream_repository.as_deref(),
            ])
        }))
}

#[cfg(test)]
mod tests {
    use super::{RepositoryActivity, RepositoryDetail};

    #[test]
    fn reads_downstream_remote_from_dashboard_and_repository_shapes()
    -> Result<(), Box<dyn std::error::Error>> {
        let activity: RepositoryActivity = serde_json::from_str(
            r#"{
                "owner": "me",
                "name": "velocity",
                "forward": {
                    "state": "synced",
                    "behind": 0,
                    "remote": "https://github.com/mESmaC/velocity.git"
                }
            }"#,
        )?;
        assert_eq!(
            activity.downstream_remote().as_deref(),
            Some("https://github.com/mESmaC/velocity.git")
        );

        let detail: RepositoryDetail = serde_json::from_str(
            r#"{
                "downstream": { "owner": "mESmaC", "name": "rosen" }
            }"#,
        )?;
        assert_eq!(
            detail
                .downstream
                .as_ref()
                .and_then(|d| d.as_remote())
                .as_deref(),
            Some("mESmaC/rosen")
        );
        Ok(())
    }
}
