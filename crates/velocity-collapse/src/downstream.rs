//! Configuring Collapse's downstream GitHub repository for a mirrored project (Collapse C4, #579).
//!
//! Calls `POST /api/downstream` with `github_repository` using the provisioning token so a
//! GitHub project moved to Collapse keeps forwarding lands to GitHub without a manual browser
//! step. When the Collapse GitHub App is not installed on that repository or lacks Contents or
//! Workflows write (collapse.sh#176), Collapse's reply is mapped to `DownstreamSetup::NeedsGitHubApp`
//! so Velocity can open the App's install or permission page and surface a project health row.

use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::CollapseError;
use crate::credential::CollapseCredentials;
use crate::provision::{authorized, failed};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of asking Collapse (`POST /api/downstream`) to forward a repository to GitHub (#579).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownstreamSetup {
    /// Collapse configured the downstream GitHub repository; no browser step is needed.
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

#[derive(Serialize)]
struct SetDownstreamRequest<'request> {
    owner: &'request str,
    name: &'request str,
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

/// Points Collapse's forwarder for `owner/name` at `github_repository` (`owner/repo`) via
/// `POST /api/downstream` (Collapse C4, #579).
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
    let body = SetDownstreamRequest {
        owner,
        name,
        github_repository,
    };
    let response = authorized(
        &client,
        Method::POST,
        "/api/downstream",
        &credentials.access_token,
    )
    .json(&body)
    .send()
    .await
    .map_err(failed)?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    interpret_response(status, &text)
}

fn interpret_response(status: StatusCode, body: &str) -> Result<DownstreamSetup, CollapseError> {
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
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(CollapseError::Api(format!(
            "Collapse refused to set the downstream repository ({status}); disconnect and connect Collapse again in Settings"
        )));
    }
    let reason = reply_reason(reply.as_ref(), body);
    Err(CollapseError::Api(format!(
        "Could not set the downstream repository on Collapse: {status} {reason}"
    )))
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
    if let Some(reply) = reply {
        if explicit_app_url(reply).is_some()
            || reply.needs_install == Some(true)
            || !reply.missing_permissions.is_empty()
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
        if status != StatusCode::UNAUTHORIZED {
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
    }
    status != StatusCode::UNAUTHORIZED && has_github_app_marker(body)
}

fn explicit_app_url(reply: &DownstreamReply) -> Option<String> {
    first_non_empty([
        reply.install_url.as_deref(),
        reply.installation_url.as_deref(),
        reply.permissions_url.as_deref(),
        reply.permission_url.as_deref(),
        reply.app_install_url.as_deref(),
        reply.setup_url.as_deref(),
        reply.configure_url.as_deref(),
        reply.redirect_url.as_deref(),
    ])
}

fn error_app_url(reply: &DownstreamReply) -> Option<String> {
    explicit_app_url(reply).or_else(|| reply.url.as_deref().and_then(non_empty))
}

fn reply_reason(reply: Option<&DownstreamReply>, body: &str) -> String {
    if let Some(reply) = reply {
        if let Some(text) = first_non_empty([
            reply.message.as_deref(),
            reply.detail.as_deref(),
            reply.reason.as_deref(),
            reply.error.as_deref(),
        ]) {
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

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn first_non_empty<const N: usize>(candidates: [Option<&str>; N]) -> Option<String> {
    candidates.into_iter().flatten().find_map(non_empty)
}
