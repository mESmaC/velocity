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
    pub description: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub private: Option<bool>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub last_land: Option<LastLand>,
    #[serde(default)]
    pub forward: Forward,
    #[serde(default)]
    pub downstream: Option<DownstreamRef>,
    #[serde(default)]
    pub downstream_remote: Option<String>,
    #[serde(default)]
    pub downstream_url: Option<String>,
    #[serde(default)]
    pub downstream_repository: Option<String>,
    #[serde(default)]
    pub linked_server: Option<bool>,
    #[serde(default)]
    pub has_linked_server: Option<bool>,
    #[serde(default)]
    pub server_linked: Option<bool>,
    #[serde(default)]
    pub rosen_linked: Option<bool>,
    #[serde(default)]
    pub has_server: Option<bool>,
    #[serde(default)]
    pub server_id: Option<String>,
    #[serde(default)]
    pub rosen_server_id: Option<String>,
    #[serde(default)]
    pub state_feed_at: Option<String>,
    #[serde(default)]
    pub live_state_at: Option<String>,
    #[serde(default)]
    pub rosen_state_at: Option<String>,
    #[serde(default)]
    pub last_state_at: Option<String>,
    #[serde(default)]
    pub rosen: Option<serde_json::Value>,
    #[serde(default)]
    pub server: Option<serde_json::Value>,
    #[serde(default)]
    pub live_state: Option<serde_json::Value>,
    #[serde(default)]
    pub state_feed: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LastLand {
    pub time: String,
    #[serde(default)]
    pub agent: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Forward {
    /// `unconfigured`, `synced`, `catching_up`, `retrying`, `stopped` or `unavailable`.
    #[serde(default)]
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
    /// `owner/name` for this repository row.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Whether this repository is private; Collapse repositories default to private.
    pub fn is_private(&self) -> bool {
        self.private.unwrap_or_else(|| {
            self.visibility
                .as_deref()
                .is_none_or(|visibility| !visibility.eq_ignore_ascii_case("public"))
        })
    }

    /// Whether this repository is archived on Collapse.
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// Whether Collapse's dashboard reports a linked Rosen server for this repository (#575).
    /// Explicit server-link flags win when present; otherwise a live-state/server block,
    /// server identifier, state-feed timestamp or recorded Rosen land indicates a server.
    pub fn has_linked_server(&self) -> bool {
        if let Some(flag) = [
            self.linked_server,
            self.has_linked_server,
            self.server_linked,
            self.rosen_linked,
            self.has_server,
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return flag;
        }
        if first_non_empty([
            self.server_id.as_deref(),
            self.rosen_server_id.as_deref(),
            self.state_feed_at.as_deref(),
            self.live_state_at.as_deref(),
            self.rosen_state_at.as_deref(),
            self.last_state_at.as_deref(),
        ])
        .is_some()
        {
            return true;
        }
        for nested in [
            self.rosen.as_ref(),
            self.server.as_ref(),
            self.live_state.as_ref(),
            self.state_feed.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if linked_from_value(nested) == Some(true) {
                return true;
            }
        }
        self.last_land.is_some()
    }

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

fn linked_from_value(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(flag) => Some(*flag),
        serde_json::Value::String(text) => linked_from_status(text),
        serde_json::Value::Array(items) => (!items.is_empty()).then_some(true),
        serde_json::Value::Number(number) => (number.as_u64().unwrap_or(0) > 0).then_some(true),
        serde_json::Value::Object(map) => linked_from_object(map),
    }
}

fn linked_from_object(map: &serde_json::Map<String, serde_json::Value>) -> Option<bool> {
    for key in ["linked", "has_server", "connected", "active"] {
        if let Some(flag) = map.get(key).and_then(serde_json::Value::as_bool) {
            return Some(flag);
        }
    }
    for key in ["state", "status"] {
        if let Some(text) = map.get(key).and_then(serde_json::Value::as_str)
            && let Some(flag) = linked_from_status(text)
        {
            return Some(flag);
        }
    }
    let metadata_present = [
        "server_id",
        "identity",
        "last_seen_at",
        "updated_at",
        "received_at",
        "sent_at",
        "at",
        "state_at",
    ]
    .into_iter()
    .any(|key| {
        map.get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(non_empty)
            .is_some()
    });
    if metadata_present {
        return Some(true);
    }
    let count_present = [
        "event_sequence",
        "sequence",
        "open_changes",
        "changes",
        "machines",
    ]
    .into_iter()
    .any(|key| match map.get(key) {
        Some(serde_json::Value::Array(items)) => !items.is_empty(),
        Some(serde_json::Value::Number(number)) => number.as_u64().unwrap_or(0) > 0,
        _ => false,
    });
    count_present.then_some(true)
}

fn linked_from_status(text: &str) -> Option<bool> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(false);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "none" | "unlinked" | "unconfigured" | "disconnected" | "absent" | "not_linked" => {
            Some(false)
        }
        "linked" | "connected" | "online" | "offline" | "stale" | "live" | "synced" | "active" => {
            Some(true)
        }
        _ => None,
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

/// Reads a single Collapse repository to check its configured downstream remote (#574, #611).
/// Queries `GET /api/downstream/{owner}/{name}` first and falls back to the repository detail
/// route if downstream settings are not readable with the current credential.
pub async fn repository_downstream(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<String>, CollapseError> {
    if let Ok(settings) = crate::downstream::get_downstream(credentials, owner, name).await {
        return Ok(non_empty(&settings.remote));
    }
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

    /// Risk (#575): Open from Collapse must distinguish repositories created on the web
    /// (empty or imported from Git, no linked server) from repositories that already have
    /// a linked Rosen server so Velocity never spins up a competing server.
    #[test]
    fn detects_linked_server_from_dashboard_shapes() -> Result<(), Box<dyn std::error::Error>> {
        let imported: RepositoryActivity = serde_json::from_str(
            r#"{
                "owner": "me",
                "name": "web-import",
                "visibility": "private",
                "forward": { "state": "unconfigured", "behind": 0 }
            }"#,
        )?;
        assert!(!imported.has_linked_server());
        assert!(imported.is_private());

        let landed: RepositoryActivity = serde_json::from_str(
            r#"{
                "owner": "me",
                "name": "active-project",
                "last_land": { "time": "2026-11-21T10:00:00Z", "agent": "issue-1@server" },
                "forward": { "state": "unconfigured", "behind": 0 }
            }"#,
        )?;
        assert!(landed.has_linked_server());

        let live_state: RepositoryActivity = serde_json::from_str(
            r#"{
                "owner": "me",
                "name": "live-project",
                "rosen": { "state": "connected", "open_changes": 1 },
                "forward": { "state": "unconfigured", "behind": 0 }
            }"#,
        )?;
        assert!(live_state.has_linked_server());

        let explicit_unlinked: RepositoryActivity = serde_json::from_str(
            r#"{
                "owner": "me",
                "name": "unlinked-again",
                "linked_server": false,
                "last_land": { "time": "2026-11-21T10:00:00Z", "agent": "issue-1@server" },
                "forward": { "state": "unconfigured", "behind": 0 }
            }"#,
        )?;
        assert!(!explicit_unlinked.has_linked_server());
        Ok(())
    }
}
