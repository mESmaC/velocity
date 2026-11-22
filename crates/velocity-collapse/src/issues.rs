//! Collapse C14 Issues REST client (collapse.sh#191, collapse.sh#212, #589).
//!
//! Talks to `https://api.collapse.sh/api/repositories/{owner}/{name}/issues...`:
//! - Polls the no-wake edge cursor (`/issues/cursor`) with `If-None-Match` so idle
//!   repositories never wake the Fly Machine.
//! - Fetches incremental deltas through `/issues/changes?after=<cursor>` and batch reads
//!   through `?numbers=1,2,3`.
//! - Writes require an owner personal access token with the `issues` scope, which is
//!   exempt from per-IP and per-token rate limits on Collapse.

use std::time::Duration;

use reqwest::header::{ACCEPT, ETAG, IF_NONE_MATCH};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::CollapseError;
use crate::provision::failed;
use crate::url_text::encode;

/// Default Collapse API origin serving C14 issue routes and the edge cursor.
pub const API_HOST: &str = "https://api.collapse.sh";
/// Collapse accepts at most 100 issue numbers in one `?numbers=` batch read.
pub const MAX_BATCH_NUMBERS: usize = 100;
/// Collapse accepts at most 200 issues per list page.
pub const MAX_LIST_LIMIT: usize = 200;
/// Default change-feed page size.
pub const DEFAULT_CHANGES_LIMIT: usize = 500;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of a conditional GET carrying `If-None-Match`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conditional<T> {
    NotModified,
    Modified { value: T, etag: Option<String> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseAuthor {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseLabel {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseDependency {
    pub owner: String,
    pub repo: String,
    pub number: u32,
    pub state: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseComment {
    pub id: u64,
    pub issue_number: u32,
    #[serde(default)]
    pub author: CollapseAuthor,
    pub body: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseIssue {
    pub number: u32,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub state: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub author: CollapseAuthor,
    #[serde(default)]
    pub labels: Vec<CollapseLabel>,
    #[serde(default)]
    pub blocked_by: Vec<CollapseDependency>,
    #[serde(default)]
    pub blocks: Vec<CollapseDependency>,
    #[serde(default)]
    pub comment_count: usize,
    #[serde(default)]
    pub comments: Vec<CollapseComment>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CursorResponse {
    pub cursor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ListIssuesResponse {
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub issues: Vec<CollapseIssue>,
    #[serde(default)]
    pub open_count: i64,
    #[serde(default)]
    pub closed_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ListCommentsResponse {
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub comments: Vec<CollapseComment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ListLabelsResponse {
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub labels: Vec<CollapseLabel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct IssueChange {
    pub cursor: u64,
    pub kind: String,
    #[serde(default)]
    pub issue_number: Option<u32>,
    #[serde(default)]
    pub comment_id: Option<u64>,
    #[serde(default)]
    pub label_name: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ChangesResponse {
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub after: u64,
    #[serde(default)]
    pub changes: Vec<IssueChange>,
    #[serde(default)]
    pub issues: Vec<CollapseIssue>,
    #[serde(default)]
    pub comments: Vec<CollapseComment>,
    #[serde(default)]
    pub labels: Vec<CollapseLabel>,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CreateIssueRequest<'a> {
    pub title: &'a str,
    pub body: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct UpdateIssueRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_labels: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct CloseIssueRequest<'a> {
    reason: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    comment: &'a str,
}

#[derive(Clone, Debug, Serialize)]
struct CreateCommentRequest<'a> {
    body: &'a str,
}

#[derive(Clone, Debug, Serialize)]
struct CreateLabelRequest<'a> {
    name: &'a str,
    color: &'a str,
    description: &'a str,
}

/// Same-repository or cross-repository blocker reference for `POST .../dependencies`.
#[derive(Clone, Debug, Serialize)]
pub struct DependencyTarget<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<&'a str>,
    pub number: u32,
}

/// One commit row returned by `GET /api/history/{owner}/{name}` (#590).
#[derive(Clone, Debug, Deserialize)]
pub struct CollapseHistoryLand {
    pub commit: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub message: String,
}

/// One page returned by `GET /api/history/{owner}/{name}?page={page}` (#590).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CollapseHistoryPage {
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub page: usize,
    #[serde(default)]
    pub lands: Vec<CollapseHistoryLand>,
    #[serde(default, rename = "hasNext")]
    pub has_next: bool,
}

/// Reusable async REST client for Collapse C14 Issues.
#[derive(Clone)]
pub struct CollapseIssueApi {
    client: Client,
    base_url: String,
}

impl CollapseIssueApi {
    /// Builds a client pointing at `https://api.collapse.sh`.
    pub fn new() -> Result<Self, CollapseError> {
        Self::with_base_url(API_HOST)
    }

    /// Builds a client pointing at a custom base URL (for example a local test server).
    pub fn with_base_url(base_url: &str) -> Result<Self, CollapseError> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("velocity-collapse-issues")
            .build()
            .map_err(failed)?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Reads the per-repository issue cursor from the edge (`GET .../issues/cursor`).
    /// Returns `Ok(None)` when the edge KV entry is not yet populated (`404`).
    pub async fn get_cursor(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        if_none_match: Option<&str>,
    ) -> Result<Option<Conditional<u64>>, CollapseError> {
        let path = format!("{}/cursor", issues_path(owner, name));
        let response = self
            .request(Method::GET, &path, token, if_none_match)
            .send()
            .await
            .map_err(failed)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(Some(Conditional::NotModified));
        }
        let etag = response_etag(&response);
        let body: CursorResponse = parse_json(response, "read issue cursor", false).await?;
        Ok(Some(Conditional::Modified {
            value: body.cursor,
            etag,
        }))
    }

    /// Lists issues (`GET .../issues`) with optional `state`, batch `numbers`, `limit` and
    /// `offset`.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_issues(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        state: Option<&str>,
        numbers: Option<&[u32]>,
        limit: usize,
        offset: usize,
        if_none_match: Option<&str>,
    ) -> Result<Conditional<ListIssuesResponse>, CollapseError> {
        let mut params = Vec::new();
        if let Some(state) = state {
            params.push(format!("state={state}"));
        }
        if let Some(numbers) = numbers
            && !numbers.is_empty()
        {
            let joined = numbers
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            params.push(format!("numbers={joined}"));
        }
        params.push(format!("limit={}", limit.clamp(1, MAX_LIST_LIMIT)));
        if offset > 0 {
            params.push(format!("offset={offset}"));
        }
        params.push("sort=number&direction=asc".to_owned());
        let path = format!("{}?{}", issues_path(owner, name), params.join("&"));
        self.get_conditional(token, &path, if_none_match, "list issues")
            .await
    }

    /// Reads every issue, comment, label and dependency edit recorded after `after`
    /// (`GET .../issues/changes?after=<cursor>`).
    pub async fn get_changes(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        after: u64,
        limit: usize,
        if_none_match: Option<&str>,
    ) -> Result<Conditional<ChangesResponse>, CollapseError> {
        let path = format!(
            "{}/changes?after={after}&limit={}",
            issues_path(owner, name),
            limit.clamp(1, 1000)
        );
        self.get_conditional(token, &path, if_none_match, "read issue changes")
            .await
    }

    /// Reads one issue with its labels, blocked-by dependencies and comments
    /// (`GET .../issues/{number}`).
    pub async fn get_issue(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        number: u32,
        if_none_match: Option<&str>,
    ) -> Result<Conditional<CollapseIssue>, CollapseError> {
        let path = format!("{}/{number}", issues_path(owner, name));
        self.get_conditional(token, &path, if_none_match, "read the issue")
            .await
    }

    /// Creates a new issue (`POST .../issues`).
    pub async fn create_issue(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        body: &CreateIssueRequest<'_>,
    ) -> Result<CollapseIssue, CollapseError> {
        let path = issues_path(owner, name);
        let response = self
            .request(Method::POST, &path, Some(token), None)
            .json(body)
            .send()
            .await
            .map_err(failed)?;
        parse_json(response, "create the issue", true).await
    }

    /// Patches an issue's title, body, state, reason or labels (`PATCH .../issues/{number}`).
    pub async fn update_issue(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        number: u32,
        patch: &UpdateIssueRequest,
    ) -> Result<CollapseIssue, CollapseError> {
        let path = format!("{}/{number}", issues_path(owner, name));
        let response = self
            .request(Method::PATCH, &path, Some(token), None)
            .json(patch)
            .send()
            .await
            .map_err(failed)?;
        parse_json(response, "update the issue", true).await
    }

    /// Closes an issue with `completed` or `not_planned` (`POST .../issues/{number}/close`).
    pub async fn close_issue(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        number: u32,
        reason: &str,
        comment: &str,
    ) -> Result<CollapseIssue, CollapseError> {
        let path = format!("{}/{number}/close", issues_path(owner, name));
        let response = self
            .request(Method::POST, &path, Some(token), None)
            .json(&CloseIssueRequest { reason, comment })
            .send()
            .await
            .map_err(failed)?;
        parse_json(response, "close the issue", true).await
    }

    /// Lists all comments on `number` (`GET .../issues/{number}/comments`).
    pub async fn list_comments(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        number: u32,
        if_none_match: Option<&str>,
    ) -> Result<Conditional<ListCommentsResponse>, CollapseError> {
        let path = format!("{}/{number}/comments", issues_path(owner, name));
        self.get_conditional(token, &path, if_none_match, "list issue comments")
            .await
    }

    /// Posts a comment on `number` (`POST .../issues/{number}/comments`).
    pub async fn create_comment(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        number: u32,
        body: &str,
    ) -> Result<CollapseComment, CollapseError> {
        let path = format!("{}/{number}/comments", issues_path(owner, name));
        let response = self
            .request(Method::POST, &path, Some(token), None)
            .json(&CreateCommentRequest { body })
            .send()
            .await
            .map_err(failed)?;
        parse_json(response, "post the issue comment", true).await
    }

    /// Updates a comment's body (`PATCH .../issues/{number}/comments/{comment_id}`) (#628).
    pub async fn update_comment(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        number: u32,
        comment_id: u64,
        body: &str,
    ) -> Result<CollapseComment, CollapseError> {
        let path = format!(
            "{}/{number}/comments/{comment_id}",
            issues_path(owner, name)
        );
        let response = self
            .request(Method::PATCH, &path, Some(token), None)
            .json(&CreateCommentRequest { body })
            .send()
            .await
            .map_err(failed)?;
        parse_json(response, "update the issue comment", true).await
    }

    /// Lists repository labels (`GET .../issues/labels`).
    pub async fn list_labels(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        if_none_match: Option<&str>,
    ) -> Result<Conditional<ListLabelsResponse>, CollapseError> {
        let path = format!("{}/labels", issues_path(owner, name));
        self.get_conditional(token, &path, if_none_match, "list repository labels")
            .await
    }

    /// Creates a repository label (`POST .../issues/labels`). Returns `Ok(None)` on `409 Conflict`
    /// when another caller created the same label concurrently.
    pub async fn create_label(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        label_name: &str,
        color: &str,
        description: &str,
    ) -> Result<Option<CollapseLabel>, CollapseError> {
        let path = format!("{}/labels", issues_path(owner, name));
        let response = self
            .request(Method::POST, &path, Some(token), None)
            .json(&CreateLabelRequest {
                name: label_name,
                color,
                description,
            })
            .send()
            .await
            .map_err(failed)?;
        if response.status() == StatusCode::CONFLICT {
            return Ok(None);
        }
        parse_json(response, "create the repository label", true)
            .await
            .map(Some)
    }

    /// Links a same-repository or cross-repository blocker (`POST .../issues/{number}/dependencies`).
    pub async fn add_dependency(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        number: u32,
        target: &DependencyTarget<'_>,
    ) -> Result<CollapseDependency, CollapseError> {
        let path = format!("{}/{number}/dependencies", issues_path(owner, name));
        let response = self
            .request(Method::POST, &path, Some(token), None)
            .json(target)
            .send()
            .await
            .map_err(failed)?;
        parse_json(response, "link the issue dependency", true).await
    }

    /// Reports whether `commit` is already mirrored on Collapse (`GET /api/history/{o}/{n}/land/{sha}/summary`).
    pub async fn commit_exists(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        commit: &str,
    ) -> Result<bool, CollapseError> {
        let path = format!(
            "/api/history/{}/{}/land/{}/summary",
            encode(owner),
            encode(name),
            encode(commit)
        );
        let response = self
            .request(Method::GET, &path, token, None)
            .send()
            .await
            .map_err(failed)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        issue_success(response, "check mirrored commit", false).await?;
        Ok(true)
    }

    /// Reads a raw UTF-8 file from Collapse's codeview API
    /// (`GET /api/code/{owner}/{name}/raw?path=...&ref=...`) (#590).
    /// Returns `Ok(None)` when the repository, ref or path is not found.
    pub async fn get_raw_file(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        file_path: &str,
        reference: Option<&str>,
    ) -> Result<Option<String>, CollapseError> {
        let mut path = format!(
            "/api/code/{}/{}/raw?path={}",
            encode(owner),
            encode(name),
            encode(file_path)
        );
        if let Some(reference) = reference.filter(|value| !value.is_empty()) {
            path.push_str("&ref=");
            path.push_str(&encode(reference));
        }
        let mut builder = self
            .client
            .request(Method::GET, format!("{}{path}", self.base_url))
            .header(ACCEPT, "text/plain, */*");
        if let Some(bearer) = token.filter(|text| !text.is_empty()) {
            builder = builder.bearer_auth(bearer);
        }
        let response = builder.send().await.map_err(failed)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let text = issue_success(response, "read repository file", false)
            .await?
            .text()
            .await
            .map_err(failed)?;
        Ok(Some(text))
    }

    /// Reads one page of default-branch history from Collapse
    /// (`GET /api/history/{owner}/{name}?page={page}`) (#590).
    pub async fn list_history(
        &self,
        token: Option<&str>,
        owner: &str,
        name: &str,
        page: usize,
    ) -> Result<CollapseHistoryPage, CollapseError> {
        let page = page.max(1);
        let path = format!(
            "/api/history/{}/{}?page={page}",
            encode(owner),
            encode(name)
        );
        let response = self
            .request(Method::GET, &path, token, None)
            .send()
            .await
            .map_err(failed)?;
        parse_json(response, "read repository history", false).await
    }

    async fn get_conditional<T: DeserializeOwned>(
        &self,
        token: Option<&str>,
        path: &str,
        if_none_match: Option<&str>,
        action: &str,
    ) -> Result<Conditional<T>, CollapseError> {
        let response = self
            .request(Method::GET, path, token, if_none_match)
            .send()
            .await
            .map_err(failed)?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let etag = response_etag(&response);
        let value = parse_json(response, action, false).await?;
        Ok(Conditional::Modified { value, etag })
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        if_none_match: Option<&str>,
    ) -> RequestBuilder {
        let mut builder = self
            .client
            .request(method, format!("{}{path}", self.base_url))
            .header(ACCEPT, "application/json");
        if let Some(bearer) = token.filter(|text| !text.is_empty()) {
            builder = builder.bearer_auth(bearer);
        }
        if let Some(etag) = if_none_match.filter(|text| !text.is_empty()) {
            builder = builder.header(IF_NONE_MATCH, etag);
        }
        builder
    }
}

fn issues_path(owner: &str, name: &str) -> String {
    format!(
        "/api/repositories/{}/{}/issues",
        encode(owner),
        encode(name)
    )
}

fn response_etag(response: &Response) -> Option<String> {
    response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn parse_json<T: DeserializeOwned>(
    response: Response,
    action: &str,
    write: bool,
) -> Result<T, CollapseError> {
    issue_success(response, action, write)
        .await?
        .json()
        .await
        .map_err(failed)
}

async fn issue_success(
    response: Response,
    action: &str,
    write: bool,
) -> Result<Response, CollapseError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let detail = extract_error_message(&body);
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let guidance = if write {
            " Save a Collapse personal access token with the `issues` scope (https://collapse.sh/settings/tokens) in Settings → Collapse."
        } else {
            " Connect Collapse or save a Collapse personal access token in Settings → Collapse."
        };
        return Err(CollapseError::Api(format!(
            "HTTP {}: Collapse refused to {action}{detail}.{guidance}",
            status.as_u16()
        )));
    }
    Err(CollapseError::Api(format!(
        "HTTP {}: Could not {action} on Collapse{detail}",
        status.as_u16()
    )))
}

fn extract_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(message) = value.get("error").and_then(serde_json::Value::as_str)
        && !message.trim().is_empty()
    {
        return format!(": {}", message.trim());
    }
    let trimmed: String = body.trim().chars().take(200).collect();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(" {trimmed}")
    }
}
