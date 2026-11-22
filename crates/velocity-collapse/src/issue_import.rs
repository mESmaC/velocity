//! Collapse C14 GitHub issue import client (collapse.sh#193, velocity#592).
//!
//! Calls `POST /api/repositories/{owner}/{name}/issues/import` and polls
//! `GET /api/repositories/{owner}/{name}/issues/import` until the import has caught up.
//! Writes require a personal access token with the `issues` scope.

use std::time::{Duration, Instant};

use reqwest::{Client, Method, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::provision::{authorized, failed};
use crate::url_text::encode;

/// Collapse runs one synchronous import for up to 5 minutes (`importRunTimeout`).
const IMPORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(6 * 60);
const STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const POLL_WAIT_TIMEOUT: Duration = Duration::from_secs(5 * 60 + 30);

/// Counts returned by Collapse after importing GitHub issues, comments, labels and
/// blocked-by dependencies (collapse.sh#193).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssueImportResult {
    #[serde(default)]
    pub github_repository: String,
    #[serde(default)]
    pub issues_created: u32,
    #[serde(default)]
    pub issues_updated: u32,
    #[serde(default)]
    pub issues_unchanged: u32,
    #[serde(default)]
    pub pull_requests_skipped: u32,
    #[serde(default)]
    pub comments_created: u32,
    #[serde(default)]
    pub comments_updated: u32,
    #[serde(default)]
    pub comments_unchanged: u32,
    #[serde(default)]
    pub labels_created: u32,
    #[serde(default)]
    pub labels_updated: u32,
    #[serde(default)]
    pub dependencies_added: u32,
    #[serde(default)]
    pub dependencies_removed: u32,
    #[serde(default)]
    pub dependencies_skipped: u32,
    #[serde(default)]
    pub next_number: u64,
    #[serde(default)]
    pub cursor: u64,
}

/// Status returned by `GET` or `POST /api/repositories/{owner}/{name}/issues/import`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssueImportStatus {
    #[serde(default)]
    pub github_repository: String,
    #[serde(default)]
    pub suggested_github_repository: String,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub last_synced_at: Option<String>,
    #[serde(default)]
    pub result: Option<GitHubIssueImportResult>,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub action_url: String,
    #[serde(default)]
    pub moved_to: String,
}

impl GitHubIssueImportStatus {
    /// Returns the non-empty `finished_at` timestamp when Collapse's last GitHub issue
    /// import finished without an error (#613).
    pub fn finished_without_error_at(&self) -> Option<&str> {
        if !self.error.trim().is_empty() {
            return None;
        }
        self.finished_at
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// Whether Collapse has recorded at least one completed GitHub issue import (#612, #613).
    pub fn has_finished(&self) -> bool {
        self.result.is_some()
            || self.finished_without_error_at().is_some()
            || self
                .last_synced_at
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }
}

/// Structured failure from Collapse's issue import endpoint, preserving remediation URLs
/// (for example GitHub App installation/permissions) and token-scope errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueImportFailure {
    pub message: String,
    pub action_url: Option<String>,
    pub moved_to: Option<String>,
    pub needs_issues_token: bool,
}

impl std::fmt::Display for IssueImportFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IssueImportFailure {}

// Deliberately no `Debug`: `token` must never reach a log.
#[derive(Serialize)]
struct ImportRequestBody<'a> {
    github_repository: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<&'a str>,
}

#[derive(Deserialize)]
struct ErrorPayload {
    #[serde(default)]
    error: String,
    #[serde(default)]
    action_url: String,
    #[serde(default)]
    moved_to: String,
}

/// Reads the current GitHub issue import status for `owner/name` on Collapse.
pub async fn get_issue_import_status(
    token: &str,
    owner: &str,
    name: &str,
) -> Result<GitHubIssueImportStatus, IssueImportFailure> {
    let client = build_client(STATUS_REQUEST_TIMEOUT)?;
    read_status(&client, token, owner, name).await
}

/// Triggers a GitHub issue import for `owner/name` from `github_repository` and waits until
/// Collapse finishes syncing. Safe to re-run: if an import is already running (409 Conflict)
/// or still marked `running`, this polls `GET .../issues/import` until it catches up.
pub async fn import_github_issues_and_wait(
    token: &str,
    owner: &str,
    name: &str,
    github_repository: &str,
    github_token: Option<&str>,
) -> Result<GitHubIssueImportStatus, IssueImportFailure> {
    let import_client = build_client(IMPORT_REQUEST_TIMEOUT)?;
    let status_client = build_client(STATUS_REQUEST_TIMEOUT)?;
    let trimmed_github_token = github_token
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let body = ImportRequestBody {
        github_repository,
        auth_mode: Some(if trimmed_github_token.is_some() {
            "token"
        } else {
            "github_app"
        }),
        token: trimmed_github_token,
    };
    let path = import_path(owner, name);
    let response = authorized(&import_client, Method::POST, &path, token)
        .json(&body)
        .send()
        .await
        .map_err(|error| simple_failure(failed(error).to_string()))?;
    if response.status() == StatusCode::CONFLICT {
        return wait_until_idle(&status_client, token, owner, name).await;
    }
    let status = parse_status_response(response, "import GitHub issues").await?;
    if status.running {
        return wait_until_idle(&status_client, token, owner, name).await;
    }
    finish_status(status)
}

async fn wait_until_idle(
    client: &Client,
    token: &str,
    owner: &str,
    name: &str,
) -> Result<GitHubIssueImportStatus, IssueImportFailure> {
    let deadline = Instant::now() + POLL_WAIT_TIMEOUT;
    loop {
        let status = read_status(client, token, owner, name).await?;
        if !status.running {
            return finish_status(status);
        }
        if Instant::now() >= deadline {
            return Err(simple_failure(
                "GitHub issue import is still running on Collapse; wait a moment and re-run Move to Collapse"
                    .into(),
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn read_status(
    client: &Client,
    token: &str,
    owner: &str,
    name: &str,
) -> Result<GitHubIssueImportStatus, IssueImportFailure> {
    let path = import_path(owner, name);
    let response = authorized(client, Method::GET, &path, token)
        .send()
        .await
        .map_err(|error| simple_failure(failed(error).to_string()))?;
    parse_status_response(response, "check GitHub issue import status").await
}

fn finish_status(
    status: GitHubIssueImportStatus,
) -> Result<GitHubIssueImportStatus, IssueImportFailure> {
    if !status.error.trim().is_empty() {
        return Err(IssueImportFailure {
            message: status.error.trim().to_owned(),
            action_url: non_empty(&status.action_url),
            moved_to: non_empty(&status.moved_to),
            needs_issues_token: false,
        });
    }
    Ok(status)
}

async fn parse_status_response(
    response: Response,
    action: &str,
) -> Result<GitHubIssueImportStatus, IssueImportFailure> {
    let status_code = response.status();
    let body = response.text().await.unwrap_or_default();
    if status_code.is_success() {
        return serde_json::from_str::<GitHubIssueImportStatus>(&body).map_err(|error| {
            simple_failure(format!(
                "Collapse returned an unexpected issue import reply: {error}"
            ))
        });
    }
    let payload = serde_json::from_str::<ErrorPayload>(&body).ok();
    let needs_issues_token = matches!(
        status_code,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    );
    let message = payload
        .as_ref()
        .and_then(|item| non_empty(&item.error))
        .unwrap_or_else(|| {
            if needs_issues_token {
                format!("Collapse refused to {action} ({status_code}); save a Collapse PAT with the issues scope")
            } else {
                let snippet: String = body.trim().chars().take(200).collect();
                format!("Could not {action} on Collapse: {status_code} {snippet}")
            }
        });
    Err(IssueImportFailure {
        message,
        action_url: payload
            .as_ref()
            .and_then(|item| non_empty(&item.action_url)),
        moved_to: payload.as_ref().and_then(|item| non_empty(&item.moved_to)),
        needs_issues_token,
    })
}

fn import_path(owner: &str, name: &str) -> String {
    format!(
        "/api/repositories/{}/{}/issues/import",
        encode(owner),
        encode(name)
    )
}

fn build_client(timeout: Duration) -> Result<Client, IssueImportFailure> {
    Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| simple_failure(failed(error).to_string()))
}

fn simple_failure(message: String) -> IssueImportFailure {
    IssueImportFailure {
        message,
        action_url: None,
        moved_to: None,
        needs_issues_token: false,
    }
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::GitHubIssueImportStatus;

    #[test]
    fn finished_without_error_requires_finished_at_and_empty_error() {
        let finished = GitHubIssueImportStatus {
            finished_at: Some(" 2026-04-12T10:20:30Z ".into()),
            error: String::new(),
            ..GitHubIssueImportStatus::default()
        };
        assert_eq!(
            finished.finished_without_error_at(),
            Some("2026-04-12T10:20:30Z")
        );
        assert!(finished.has_finished());

        let errored = GitHubIssueImportStatus {
            finished_at: Some("2026-04-12T10:20:30Z".into()),
            error: "rate limited".into(),
            ..GitHubIssueImportStatus::default()
        };
        assert_eq!(errored.finished_without_error_at(), None);

        let never_finished = GitHubIssueImportStatus {
            finished_at: Some("   ".into()),
            error: String::new(),
            ..GitHubIssueImportStatus::default()
        };
        assert_eq!(never_finished.finished_without_error_at(), None);
        assert!(!never_finished.has_finished());
    }
}
