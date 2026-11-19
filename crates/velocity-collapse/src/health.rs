//! What Collapse reports about the account's repositories: last land and the downstream
//! forwarder (Collapse C2, C4; #427). One dashboard read with the read-only personal token
//! covers every project, so Velocity polls it rather than each repository.

use std::time::Duration;

use reqwest::{Client, Method};
use serde::Deserialize;

use crate::CollapseError;
use crate::credential::CollapseCredentials;
use crate::provision::{authorized, failed, success};

const READ_TIMEOUT: Duration = Duration::from_secs(20);

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
}

#[derive(Deserialize)]
struct Dashboard {
    repositories: Vec<RepositoryActivity>,
}

impl RepositoryActivity {
    /// Whether this row is `owner/name`; Collapse names are unique ignoring case.
    pub fn is(&self, repository: &str) -> bool {
        repository.split_once('/').is_some_and(|(owner, name)| {
            self.owner.eq_ignore_ascii_case(owner) && self.name.eq_ignore_ascii_case(name)
        })
    }
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
