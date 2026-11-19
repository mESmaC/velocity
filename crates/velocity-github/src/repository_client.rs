use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;

use crate::GitHubCommandError;
use crate::gh_command::GitHubCommand;

/// How long a kept client's repository identity is trusted. Re-reading it costs one request,
/// so this is one request per kept client per half hour.
const IDENTITY_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// One GitHub repository's identity and its `gh` transport, shared by the issue reads and
/// writes (F9) and the Rosen issue and status mirrors.
pub struct RepositoryClient {
    pub(crate) command: GitHubCommand,
    pub(crate) repository: String,
    pub(crate) default_branch: Option<String>,
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

impl RepositoryClient {
    /// Reads the repository's identity once. Keep the client for repeated reads: every
    /// `discover` is a GitHub request.
    #[track_caller]
    pub fn discover(directory: &Path) -> Result<Self, GitHubCommandError> {
        let (command, repository) = GitHubCommand::new(directory)?;
        Self::read_identity(command, &repository)
    }

    #[track_caller]
    pub fn for_repository(repository: &str) -> Result<Self, GitHubCommandError> {
        let repository = crate::RepositoryName::parse(repository)
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        let command = GitHubCommand::for_repository(&repository);
        Self::read_identity(command, &repository.full_name())
    }

    fn read_identity(command: GitHubCommand, repository: &str) -> Result<Self, GitHubCommandError> {
        let identity: RepositoryIdentity = command.read(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner,defaultBranchRef",
            "--",
            repository,
        ])?;
        let (owner, name) = identity
            .name_with_owner
            .split_once('/')
            .ok_or_else(|| GitHubCommandError::Invalid("Invalid GitHub repository name".into()))?;
        if [owner, name].iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        }) {
            return Err(GitHubCommandError::Invalid(
                "Invalid GitHub repository name".into(),
            ));
        }
        Ok(Self {
            owner: owner.into(),
            name: name.into(),
            default_branch: identity.default_branch_ref.map(|branch| branch.name),
            repository: identity.name_with_owner,
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

    pub(crate) fn variables(&self, number: u32) -> Value {
        json!({"owner":self.owner,"name":self.name,"number":number})
    }

    pub(crate) fn graphql_at<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        path: &str,
    ) -> Result<T, GitHubCommandError> {
        let response: Value = self
            .command
            .graphql(path, &json!({"query":query,"variables":variables}))?;
        if let Some(errors) = response.get("errors") {
            return Err(GitHubCommandError::Invalid(format!(
                "GitHub GraphQL errors: {errors}"
            )));
        }
        let value = response
            .pointer(path)
            .filter(|value| !value.is_null())
            .ok_or_else(|| GitHubCommandError::Missing(path.into()))?;
        Ok(serde_json::from_value(value.clone())?)
    }

    pub(crate) fn connection<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        path: &str,
    ) -> Result<Vec<T>, GitHubCommandError> {
        let first_page = self.graphql_at(query, variables.clone(), path)?;
        self.complete_connection(query, variables, path, first_page)
    }

    pub(crate) fn complete_connection<T: DeserializeOwned>(
        &self,
        query: &str,
        mut variables: Value,
        path: &str,
        mut page: Connection<T>,
    ) -> Result<Vec<T>, GitHubCommandError> {
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
                .ok_or_else(|| GitHubCommandError::Missing("pagination cursor".into()))?;
            if variables.get("cursor").and_then(Value::as_str) == Some(&cursor) {
                return Err(GitHubCommandError::Invalid(
                    "GitHub repeated a pagination cursor".into(),
                ));
            }
            variables["cursor"] = json!(cursor);
            page = self.graphql_at(query, variables.clone(), path)?;
        }
    }
}
