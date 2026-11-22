//! Configuring and reading Collapse's downstream GitHub repository for a mirrored project
//! (Collapse C4, #579, #611).
//!
//! Calls `POST /api/downstream/{owner}/{name}` with `{"github_repository": "<owner/repo>"}`
//! only and reads the resulting state back with `GET /api/downstream/{owner}/{name}`. When
//! the Collapse GitHub App is not installed on that repository or lacks Contents or Workflows
//! write (collapse.sh#176), Collapse's reply is mapped to `DownstreamSetup::NeedsGitHubApp`.

use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::CollapseError;
use crate::credential::CollapseCredentials;
use crate::provision::{authorized, failed};
use crate::url_text::encode;

const COLLAPSE_WEB: &str = "https://collapse.sh";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of asking Collapse (`POST /api/downstream/{owner}/{name}`) to forward a repository
/// to GitHub (#579, #611).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownstreamSetup {
    /// Collapse configured the downstream GitHub repository and `GET` confirmed it.
    Configured,
    /// The Collapse GitHub App is not installed on the repository or lacks Contents or
    /// Workflows write permission (collapse.sh#176); the owner must finish in the browser.
    NeedsGitHubApp {
        /// Install or permission page URL returned by Collapse, when provided.
        page_url: Option<String>,
        /// Human-readable reason reported by Collapse.
        reason: String,
    },
}

/// Forwarder state returned inside `GET /api/downstream/{owner}/{name}` (#611).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DownstreamForwardState {
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub last_tip: String,
    #[serde(default)]
    pub pending_tip: String,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub action_url: Option<String>,
    #[serde(default)]
    pub moved_to: Option<String>,
    #[serde(default)]
    pub lands_behind: i64,
    #[serde(default)]
    pub next_attempt: Option<String>,
}

/// Repository downstream settings returned by `GET /api/downstream/{owner}/{name}` (#611).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DownstreamSettings {
    #[serde(default)]
    pub remote: String,
    #[serde(default)]
    pub credential_configured: bool,
    #[serde(default)]
    pub github: bool,
    #[serde(default)]
    pub github_remote: bool,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub state: DownstreamForwardState,
}

impl DownstreamSettings {
    /// Normalized `owner/repo` when `remote` points to GitHub, otherwise `None`.
    pub fn github_repository(&self) -> Option<String> {
        github_repository_from_remote(&self.remote)
    }
}

#[derive(Serialize)]
struct SetDownstreamRequest<'request> {
    github_repository: &'request str,
}

#[derive(Debug, Default, Deserialize)]
struct DownstreamReply {
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    needs_install: Option<bool>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    moved_to: Option<String>,
    #[serde(default)]
    integrations_url: Option<String>,
    #[serde(default)]
    action_url: Option<String>,
    #[serde(default)]
    install_url: Option<String>,
    #[serde(default)]
    installation_url: Option<String>,
    #[serde(default)]
    permissions_url: Option<String>,
    #[serde(default)]
    permission_url: Option<String>,
    #[serde(default)]
    app_install_url: Option<String>,
    #[serde(default)]
    setup_url: Option<String>,
    #[serde(default)]
    configure_url: Option<String>,
    #[serde(default)]
    redirect_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    missing_permissions: Vec<String>,
}

fn downstream_path(owner: &str, name: &str) -> String {
    format!("/api/downstream/{}/{}", encode(owner), encode(name))
}

/// Reads Collapse's downstream configuration and forwarder state via
/// `GET /api/downstream/{owner}/{name}` (#611).
pub async fn get_downstream(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<DownstreamSettings, CollapseError> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(failed)?;
    get_downstream_with_client(&client, credentials, owner, name).await
}

async fn get_downstream_with_client(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<DownstreamSettings, CollapseError> {
    let path = downstream_path(owner, name);
    let mut response = authorized(client, Method::GET, &path, &credentials.access_token)
        .send()
        .await
        .map_err(failed)?;
    if response.status() == StatusCode::FORBIDDEN
        && !credentials.mcp_token.is_empty()
        && credentials.mcp_token != credentials.access_token
    {
        response = authorized(client, Method::GET, &path, &credentials.mcp_token)
            .send()
            .await
            .map_err(failed)?;
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str::<DownstreamSettings>(&text).map_err(|error| {
            CollapseError::Api(format!(
                "Could not parse Collapse downstream settings: {error}"
            ))
        });
    }
    Err(downstream_http_error("read", status, &text))
}

/// Points Collapse's forwarder for `owner/name` at `github_repository` (`owner/repo`) via
/// `POST /api/downstream/{owner}/{name}` with `{"github_repository": ...}` only, then reads
/// the state back with `GET /api/downstream/{owner}/{name}` (Collapse C4, #579, #611).
pub async fn set_downstream(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
    github_repository: &str,
) -> Result<DownstreamSetup, CollapseError> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(failed)?;
    let path = downstream_path(owner, name);
    let body = SetDownstreamRequest { github_repository };
    let response = authorized(&client, Method::POST, &path, &credentials.access_token)
        .json(&body)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    match interpret_post_response(status, &text)? {
        DownstreamSetup::NeedsGitHubApp { page_url, reason } => {
            Ok(DownstreamSetup::NeedsGitHubApp { page_url, reason })
        }
        DownstreamSetup::Configured => {
            let settings = get_downstream_with_client(&client, credentials, owner, name).await?;
            verify_downstream_settings(&settings, github_repository)
        }
    }
}

fn interpret_post_response(
    status: StatusCode,
    body: &str,
) -> Result<DownstreamSetup, CollapseError> {
    let reply = serde_json::from_str::<DownstreamReply>(body).ok();
    if status.is_success() {
        return Ok(interpret_success(reply.as_ref(), body));
    }
    if needs_github_app(status, reply.as_ref(), body) {
        let page_url = reply.as_ref().and_then(error_app_url);
        let reason = non_empty(&reply_reason(reply.as_ref(), body)).unwrap_or_else(|| {
            "Collapse GitHub App is not installed or lacks Contents and Workflows write permissions"
                .to_owned()
        });
        return Ok(DownstreamSetup::NeedsGitHubApp { page_url, reason });
    }
    Err(downstream_http_error("set", status, body))
}

fn downstream_http_error(action: &str, status: StatusCode, body: &str) -> CollapseError {
    let reply = serde_json::from_str::<DownstreamReply>(body).ok();
    let reason = reply_reason(reply.as_ref(), body);
    if status == StatusCode::UNAUTHORIZED {
        return CollapseError::Api(format!(
            "Collapse rejected the sign-in token when trying to {action} the downstream repository ({status}); disconnect and connect Collapse again in Settings"
        ));
    }
    if status == StatusCode::FORBIDDEN {
        // 403 on `/api/downstream/{owner}/{name}` is a permission gap on the route (e.g.
        // desktop credential not yet permitted), not a stale sign-in (#611).
        let detail = non_empty(&reason)
            .filter(|text| !text.eq_ignore_ascii_case("forbidden"))
            .map(|text| format!(": {text}"))
            .unwrap_or_default();
        return CollapseError::Api(format!(
            "Collapse refused permission to {action} the downstream repository ({status}{detail})"
        ));
    }
    let suffix = non_empty(&reason)
        .map(|text| format!(" {text}"))
        .unwrap_or_default();
    CollapseError::Api(format!(
        "Could not {action} the downstream repository on Collapse: {status}{suffix}"
    ))
}

fn verify_downstream_settings(
    settings: &DownstreamSettings,
    expected_github: &str,
) -> Result<DownstreamSetup, CollapseError> {
    let actual_remote = settings.remote.trim();
    if actual_remote.is_empty() {
        return Err(CollapseError::Api(
            "Collapse accepted the request but left the downstream remote empty".into(),
        ));
    }
    let expected = github_repository_from_remote(expected_github)
        .unwrap_or_else(|| expected_github.trim().to_owned());
    if let Some(actual_github) = settings.github_repository() {
        if !actual_github.eq_ignore_ascii_case(&expected) {
            return Err(CollapseError::Api(format!(
                "Collapse downstream points to {actual_github} instead of {expected}"
            )));
        }
    } else {
        return Err(CollapseError::Api(format!(
            "Collapse downstream points to {actual_remote} instead of GitHub repository {expected}"
        )));
    }
    if settings.state.status.eq_ignore_ascii_case("stopped") {
        let reason = non_empty(&settings.state.last_error)
            .unwrap_or_else(|| "Collapse downstream forwarder is stopped".to_owned());
        let page_url = settings
            .state
            .action_url
            .as_deref()
            .and_then(resolve_collapse_url);
        if page_url.is_some() || has_github_app_marker(&reason) {
            return Ok(DownstreamSetup::NeedsGitHubApp { page_url, reason });
        }
        return Err(CollapseError::Api(reason));
    }
    Ok(DownstreamSetup::Configured)
}

fn interpret_success(reply: Option<&DownstreamReply>, body: &str) -> DownstreamSetup {
    let Some(reply) = reply else {
        return DownstreamSetup::Configured;
    };
    let page_url = explicit_app_url(reply);
    let incomplete = page_url.is_some()
        || reply.needs_install == Some(true)
        || reply.ok == Some(false)
        || !reply.missing_permissions.is_empty();
    if !incomplete {
        return DownstreamSetup::Configured;
    }
    let reason = non_empty(&reply_reason(Some(reply), body)).unwrap_or_else(|| {
        "Collapse GitHub App needs installation or Contents and Workflows write permissions"
            .to_owned()
    });
    DownstreamSetup::NeedsGitHubApp { page_url, reason }
}

fn needs_github_app(status: StatusCode, reply: Option<&DownstreamReply>, body: &str) -> bool {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return false;
    }
    if let Some(reply) = reply {
        if explicit_app_url(reply).is_some()
            || reply.needs_install == Some(true)
            || !reply.missing_permissions.is_empty()
            || status == StatusCode::UNPROCESSABLE_ENTITY
        {
            return true;
        }
        for code in [reply.code.as_deref(), reply.error.as_deref()]
            .into_iter()
            .flatten()
        {
            if has_github_app_code(code) {
                return true;
            }
        }
        for text in [
            reply.message.as_deref(),
            reply.detail.as_deref(),
            reply.reason.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if has_github_app_marker(text) {
                return true;
            }
        }
    }
    has_github_app_marker(body)
}

fn explicit_app_url(reply: &DownstreamReply) -> Option<String> {
    first_non_empty([
        reply.integrations_url.as_deref(),
        reply.action_url.as_deref(),
        reply.install_url.as_deref(),
        reply.installation_url.as_deref(),
        reply.permissions_url.as_deref(),
        reply.permission_url.as_deref(),
        reply.app_install_url.as_deref(),
        reply.setup_url.as_deref(),
        reply.configure_url.as_deref(),
        reply.redirect_url.as_deref(),
    ])
    .and_then(|url| resolve_collapse_url(&url))
}

fn error_app_url(reply: &DownstreamReply) -> Option<String> {
    explicit_app_url(reply).or_else(|| {
        reply
            .url
            .as_deref()
            .and_then(non_empty)
            .and_then(|url| resolve_collapse_url(&url))
    })
}

fn resolve_collapse_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('/') && !trimmed.starts_with("//") {
        return Some(format!("{COLLAPSE_WEB}{trimmed}"));
    }
    Some(trimmed.to_owned())
}

fn reply_reason(reply: Option<&DownstreamReply>, body: &str) -> String {
    if let Some(reply) = reply {
        if let Some(text) = first_non_empty([
            reply.message.as_deref(),
            reply.detail.as_deref(),
            reply.reason.as_deref(),
            reply.error.as_deref(),
        ]) {
            if let Some(moved) = reply.moved_to.as_deref().and_then(non_empty)
                && !text.contains(&moved)
            {
                return format!("{text} (moved to {moved})");
            }
            return text;
        }
        if !reply.missing_permissions.is_empty() {
            return format!(
                "Collapse GitHub App is missing required permissions: {}",
                reply.missing_permissions.join(", ")
            );
        }
    }
    body.trim().chars().take(200).collect()
}

fn has_github_app_code(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "github_app",
        "app_not_installed",
        "not_installed",
        "install_required",
        "installation",
        "missing_permission",
        "insufficient_permission",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn has_github_app_marker(text: &str) -> bool {
    if has_github_app_code(text) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    [
        "github app",
        "not installed",
        "install the",
        "permission",
        "workflows",
        "contents",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn github_repository_from_remote(remote: &str) -> Option<String> {
    let trimmed = remote.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = strip_prefix_ignoring_case(trimmed, "https://github.com/")
        .or_else(|| strip_prefix_ignoring_case(trimmed, "http://github.com/"))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "ssh://git@github.com/"))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "git@github.com:"))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "github.com/"))
        .unwrap_or(trimmed);
    if path.contains([':', '@', '?', '#', '\\']) {
        return None;
    }
    let path = path
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_end_matches('/'));
    let (owner, repo) = path.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/'))
        .then(|| format!("{owner}/{repo}"))
}

fn strip_prefix_ignoring_case<'text>(text: &'text str, prefix: &str) -> Option<&'text str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn first_non_empty<const N: usize>(candidates: [Option<&str>; N]) -> Option<String> {
    candidates.into_iter().flatten().find_map(non_empty)
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::{
        DownstreamForwardState, DownstreamSettings, DownstreamSetup, SetDownstreamRequest,
        downstream_path, interpret_post_response, verify_downstream_settings,
    };

    /// Risk (#611): Collapse's downstream route is `/api/downstream/{owner}/{name}` and decodes
    /// JSON with `DisallowUnknownFields`, so sending `owner`/`name` in the body returns HTTP 400.
    #[test]
    fn request_uses_owner_name_path_and_github_repository_body_only()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            downstream_path("mESmaC", "collapse.sh"),
            "/api/downstream/mESmaC/collapse.sh"
        );
        let value = serde_json::to_value(SetDownstreamRequest {
            github_repository: "mESmaC/velocity",
        })?;
        assert_eq!(
            value,
            serde_json::json!({ "github_repository": "mESmaC/velocity" })
        );
        Ok(())
    }

    /// Risk (#611): `403 Forbidden` on `/api/downstream/{owner}/{name}` is a permission gap,
    /// not a stale sign-in, so Velocity must not tell the owner to reconnect Collapse.
    #[test]
    fn forbidden_drops_reconnect_advice() -> Result<(), Box<dyn std::error::Error>> {
        let Err(error) = interpret_post_response(StatusCode::FORBIDDEN, r#"{"error":"Forbidden"}"#)
        else {
            return Err("403 must return an error".into());
        };
        let message = error.to_string();
        assert!(message.contains("403"), "unexpected message: {message}");
        assert!(
            !message.to_ascii_lowercase().contains("disconnect"),
            "403 must not advise reconnecting: {message}"
        );
        Ok(())
    }

    /// Risk (#611): Collapse returns `422` with a relative `integrations_url` when the GitHub App
    /// cannot access the repository.
    #[test]
    fn unprocessable_entity_maps_relative_integrations_url()
    -> Result<(), Box<dyn std::error::Error>> {
        let reply = r#"{"error":"Collapse GitHub App is not installed","integrations_url":"/settings/integrations"}"#;
        let setup = interpret_post_response(StatusCode::UNPROCESSABLE_ENTITY, reply)?;
        assert_eq!(
            setup,
            DownstreamSetup::NeedsGitHubApp {
                page_url: Some("https://collapse.sh/settings/integrations".into()),
                reason: "Collapse GitHub App is not installed".into(),
            }
        );
        Ok(())
    }

    /// Risk (#611): `GET /api/downstream/{owner}/{name}` read-back must confirm the configured
    /// GitHub repository and catch empty, mismatched, or stopped states.
    #[test]
    fn verifies_readback_downstream_settings() {
        let ok_settings = DownstreamSettings {
            remote: "https://github.com/mESmaC/velocity.git".into(),
            github: true,
            github_remote: true,
            state: DownstreamForwardState {
                configured: true,
                status: "in_sync".into(),
                ..DownstreamForwardState::default()
            },
            ..DownstreamSettings::default()
        };
        assert_eq!(
            verify_downstream_settings(&ok_settings, "mESmaC/velocity").ok(),
            Some(DownstreamSetup::Configured)
        );

        let empty = DownstreamSettings::default();
        assert!(verify_downstream_settings(&empty, "mESmaC/velocity").is_err());

        let stopped = DownstreamSettings {
            remote: "https://github.com/mESmaC/velocity.git".into(),
            state: DownstreamForwardState {
                configured: true,
                status: "stopped".into(),
                last_error: "GitHub push rejected".into(),
                ..DownstreamForwardState::default()
            },
            ..DownstreamSettings::default()
        };
        assert!(verify_downstream_settings(&stopped, "mESmaC/velocity").is_err());
    }
}
