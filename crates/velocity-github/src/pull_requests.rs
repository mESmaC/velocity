use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;

use crate::approval::ReviewRecord;
use crate::pull_request_command::PullRequestCommand;
use crate::pull_request_queries::{CHECKS, FIELDS};
use crate::{PullRequest, PullRequestCheck, PullRequestError, PullRequestSnapshot};

/// How long a kept client's repository identity is trusted. Re-reading it costs one request,
/// so this is one request per kept client per half hour.
const IDENTITY_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30 * 60);

pub struct PullRequestClient {
    pub(crate) command: PullRequestCommand,
    pub(crate) repository: String,
    pub(crate) default_branch: Option<String>,
    pub(crate) review_record: Option<ReviewRecord>,
    owner: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryIdentity {
    name_with_owner: String,
    default_branch_ref: Option<DefaultBranch>,
}

#[derive(Deserialize)]
struct DefaultBranch {
    name: String,
}

#[derive(Deserialize)]
pub(crate) struct Connection<T> {
    nodes: Vec<T>,
    #[serde(rename = "pageInfo")]
    page: PageInfo,
}

impl<T> Connection<T> {
    /// This page's nodes, without reading further pages.
    pub(crate) fn into_nodes(self) -> Vec<T> {
        self.nodes
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl PullRequestClient {
    /// Reads the repository's identity once. Keep the client for repeated reads: every
    /// `discover` is a GitHub request.
    #[track_caller]
    pub fn discover(directory: &Path) -> Result<Self, PullRequestError> {
        let (command, repository) = PullRequestCommand::new(directory)?;
        let identity: RepositoryIdentity = command.read(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner,defaultBranchRef",
            "--",
            &repository,
        ])?;
        let (owner, name) = identity
            .name_with_owner
            .split_once('/')
            .ok_or_else(|| PullRequestError::Invalid("Invalid GitHub repository name".into()))?;
        if [owner, name].iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        }) {
            return Err(PullRequestError::Invalid(
                "Invalid GitHub repository name".into(),
            ));
        }
        Ok(Self {
            owner: owner.into(),
            name: name.into(),
            default_branch: identity.default_branch_ref.map(|branch| branch.name),
            repository: identity.name_with_owner,
            review_record: None,
            command,
        })
    }

    /// Whether this client still describes `directory`'s repository. Callers that keep a client
    /// between reads check this before each use: a changed `origin` is seen at once, from the
    /// local Git config, and a renamed default branch, which only GitHub knows, within
    /// `IDENTITY_LIFETIME` (#229).
    pub fn is_current(&self, directory: &Path) -> bool {
        self.command.is_current(directory, IDENTITY_LIFETIME)
    }

    /// Approve, request changes and merge read and write Velocity's approval
    /// record (F7); without one they refuse rather than fall back to GitHub's
    /// review state, which the shared login can never satisfy.
    pub fn with_review_record(mut self, record: ReviewRecord) -> Self {
        self.review_record = Some(record);
        self
    }

    pub fn list(&self) -> Result<Vec<PullRequest>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{
          repository(owner:$owner,name:$name) {{ pullRequests(states:OPEN,first:100,after:$cursor,
            orderBy:{{field:UPDATED_AT,direction:DESC}}) {{ nodes {{ {FIELDS} }}
              pageInfo {{ hasNextPage endCursor }} }} }} }}"
        );
        self.connection(&query, self.variables(0), "/data/repository/pullRequests")
    }

    pub fn get(&self, number: u32) -> Result<PullRequest, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$number:Int!) {{
          repository(owner:$owner,name:$name) {{ pullRequest(number:$number) {{ {FIELDS} }} }} }}"
        );
        self.graphql_at(
            &query,
            self.variables(number),
            "/data/repository/pullRequest",
        )
    }

    pub fn snapshot(&self, number: u32) -> Result<PullRequestSnapshot, PullRequestError> {
        let pull_request = self.get(number)?;
        let snapshot = PullRequestSnapshot {
            checks: self.checks(number)?,
            reviews: self.reviews(number)?,
            comments: self.comments(number)?,
            review_threads: self.review_threads(number)?,
            pull_request,
        };
        // Never combine reviews/checks for a new push with an older head identity.
        if self.get(number)?.head_ref_oid != snapshot.pull_request.head_ref_oid {
            return Err(PullRequestError::Invalid(
                "PR head changed while reading; refresh it".into(),
            ));
        }
        Ok(snapshot)
    }

    pub(crate) fn variables(&self, number: u32) -> Value {
        json!({"owner":self.owner,"name":self.name,"number":number})
    }

    pub(crate) fn graphql_at<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        path: &str,
    ) -> Result<T, PullRequestError> {
        let response: Value = self
            .command
            .graphql(path, &json!({"query":query,"variables":variables}))?;
        if let Some(errors) = response.get("errors") {
            return Err(PullRequestError::Invalid(format!(
                "GitHub GraphQL errors: {errors}"
            )));
        }
        let value = response
            .pointer(path)
            .filter(|value| !value.is_null())
            .ok_or_else(|| PullRequestError::Missing(path.into()))?;
        Ok(serde_json::from_value(value.clone())?)
    }

    pub(crate) fn connection<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        path: &str,
    ) -> Result<Vec<T>, PullRequestError> {
        let first_page = self.graphql_at(query, variables.clone(), path)?;
        self.complete_connection(query, variables, path, first_page)
    }

    pub(crate) fn complete_connection<T: DeserializeOwned>(
        &self,
        query: &str,
        mut variables: Value,
        path: &str,
        mut page: Connection<T>,
    ) -> Result<Vec<T>, PullRequestError> {
        let mut nodes = Vec::new();
        loop {
            nodes.extend(page.nodes);
            if !page.page.has_next_page {
                return Ok(nodes);
            }
            let cursor = page
                .page
                .end_cursor
                .filter(|cursor| !cursor.is_empty())
                .ok_or_else(|| PullRequestError::Missing("pagination cursor".into()))?;
            if variables.get("cursor").and_then(Value::as_str) == Some(&cursor) {
                return Err(PullRequestError::Invalid(
                    "GitHub repeated a pagination cursor".into(),
                ));
            }
            variables["cursor"] = json!(cursor);
            page = self.graphql_at(query, variables.clone(), path)?;
        }
    }

    pub(crate) fn checks(&self, number: u32) -> Result<Vec<PullRequestCheck>, PullRequestError> {
        let path = "/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup";
        let commit: Value = self.graphql_at::<Value>(
            CHECKS,
            self.variables(number),
            "/data/repository/pullRequest/commits/nodes/0/commit",
        )?;
        if commit.get("statusCheckRollup").is_none_or(Value::is_null) {
            return Ok(Vec::new());
        }
        let first_page = commit
            .pointer("/statusCheckRollup/contexts")
            .ok_or_else(|| PullRequestError::Missing("CI check contexts".into()))?;
        let first_page = serde_json::from_value(first_page.clone())?;
        self.complete_connection(
            CHECKS,
            self.variables(number),
            &format!("{path}/contexts"),
            first_page,
        )
    }
}
